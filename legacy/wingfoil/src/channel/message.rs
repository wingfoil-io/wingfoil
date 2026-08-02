use derive_new::new;
use serde::{Deserialize, Serialize};
#[cfg(feature = "async")]
use std::boxed::Box;
#[cfg(feature = "async")]
use std::pin::Pin;
use std::sync::Arc;

use crate::queue::ValueAt;
use crate::time::NanoTime;
use crate::types::{Burst, Element};
#[cfg(feature = "zmq")]
use crate::{GraphState, RunMode};

/// Message that can be sent between threads.
#[derive(new, Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Message<T: Element + Send> {
    /// In [RunMode::HistoricalFrom], this message
    /// allows receiving graph to progress, even when
    /// the channel is ticking less frequently than
    /// other inputs.
    CheckPoint(NanoTime),
    /// Tells the receiving node that there are no messages
    /// so it can shutdown cleanly.
    EndOfStream,
    /// Used in [RunMode::HistoricalFrom]. A burst of values sharing one
    /// timestamp, delivered atomically so a same-time group can never be split
    /// across the graph's strictly-monotonic clock.
    HistoricalValue(#[serde(with = "burst_value_at_serde")] ValueAt<Burst<T>>),
    /// Sent in [RunMode::RealTime].  Just a value.
    RealtimeValue(T),
    /// Error message that can be propagated through channels.
    /// When received, the receiver will immediately return the error.
    Error(#[serde(with = "arc_error_serde")] Arc<anyhow::Error>),
}

// Serialize the burst as a plain slice/`Vec` so we don't need tinyvec's `serde`
// feature: enabling it would make `Burst<T>: Serialize`, which makes
// `Stream<Burst<T>>` satisfy both the `Stream<T>` and `Stream<Burst<T>>`
// operator impls (e.g. `CsvOperators`) and breaks inference across the crate.
mod burst_value_at_serde {
    use super::*;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Element + Send + Serialize>(
        value_at: &ValueAt<Burst<T>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        (value_at.time, value_at.value.as_slice()).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Element + Send + Deserialize<'de>>(
        d: D,
    ) -> Result<ValueAt<Burst<T>>, D::Error> {
        let (time, values): (NanoTime, Vec<T>) = Deserialize::deserialize(d)?;
        Ok(ValueAt::new(values.into_iter().collect(), time))
    }
}

mod arc_error_serde {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(err: &Arc<anyhow::Error>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{err}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<anyhow::Error>, D::Error> {
        let msg = String::deserialize(d)?;
        Ok(Arc::new(anyhow::anyhow!(msg)))
    }
}

impl<T: Element + Send + PartialEq> PartialEq for Message<T> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Message::CheckPoint(t1), Message::CheckPoint(t2)) => t1 == t2,
            (Message::EndOfStream, Message::EndOfStream) => true,
            (Message::HistoricalValue(v1), Message::HistoricalValue(v2)) => v1 == v2,
            (Message::RealtimeValue(v1), Message::RealtimeValue(v2)) => v1 == v2,
            (Message::Error(_), Message::Error(_)) => false,
            _ => false,
        }
    }
}

impl<T: Element + Send + PartialEq> Eq for Message<T> {}

impl<T: Element + Send> Message<T> {
    // This is used by optional adapters (e.g. `zmq`). When those features are disabled,
    // the helper is not compiled. When they are enabled, it can be unused depending on which
    // adapters/tests are built, so keep clippy quiet.
    #[cfg(feature = "zmq")]
    #[allow(dead_code)]
    pub fn build(value: T, graph_state: &GraphState) -> Message<T> {
        match graph_state.run_mode() {
            RunMode::RealTime => Message::RealtimeValue(value),
            RunMode::HistoricalFrom(_) => {
                Message::HistoricalValue(ValueAt::new(crate::burst![value], graph_state.time()))
            }
        }
    }
}

#[cfg(feature = "async")]
pub trait ReceiverMessageSource<T: Element + Send> {
    fn to_boxed_message_stream(self) -> Pin<Box<dyn futures::Stream<Item = Message<T>> + Send>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::NanoTime;

    #[test]
    fn checkpoint_eq() {
        let t = NanoTime::new(42);
        assert_eq!(Message::<u64>::CheckPoint(t), Message::CheckPoint(t));
        assert_ne!(
            Message::<u64>::CheckPoint(t),
            Message::CheckPoint(NanoTime::new(99))
        );
    }

    #[test]
    fn end_of_stream_eq() {
        assert_eq!(Message::<u64>::EndOfStream, Message::EndOfStream);
    }

    #[test]
    fn historical_value_eq() {
        let v1 = ValueAt::new(crate::burst![1u64], NanoTime::new(10));
        let v2 = ValueAt::new(crate::burst![1u64], NanoTime::new(10));
        let v3 = ValueAt::new(crate::burst![2u64], NanoTime::new(10));
        assert_eq!(
            Message::HistoricalValue(v1.clone()),
            Message::HistoricalValue(v2)
        );
        assert_ne!(Message::HistoricalValue(v1), Message::HistoricalValue(v3));
    }

    #[test]
    fn realtime_value_eq() {
        assert_eq!(Message::RealtimeValue(7u64), Message::RealtimeValue(7));
        assert_ne!(Message::RealtimeValue(7u64), Message::RealtimeValue(8));
    }

    #[test]
    fn historical_value_burst_eq() {
        // A multi-value same-time burst compares by its contained values.
        let b1 = ValueAt::new(crate::burst![1u64, 2], NanoTime::new(1));
        let b2 = ValueAt::new(crate::burst![1u64, 2], NanoTime::new(1));
        let b3 = ValueAt::new(crate::burst![1u64, 3], NanoTime::new(1));
        assert_eq!(
            Message::HistoricalValue(b1.clone()),
            Message::HistoricalValue(b2)
        );
        assert_ne!(Message::HistoricalValue(b1), Message::HistoricalValue(b3));
    }

    #[test]
    fn error_never_equals_itself() {
        let e1 = Message::<u64>::Error(std::sync::Arc::new(anyhow::anyhow!("boom")));
        let e2 = Message::<u64>::Error(std::sync::Arc::new(anyhow::anyhow!("boom")));
        assert_ne!(e1, e2);
    }

    #[test]
    fn mismatching_variants_not_equal() {
        assert_ne!(
            Message::<u64>::EndOfStream,
            Message::CheckPoint(NanoTime::new(0))
        );
    }

    #[test]
    fn serde_roundtrip_checkpoint() {
        let msg = Message::<u64>::CheckPoint(NanoTime::new(123));
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: Message<u64> = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn serde_roundtrip_end_of_stream() {
        let msg = Message::<u64>::EndOfStream;
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: Message<u64> = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn serde_roundtrip_historical_value() {
        let msg = Message::HistoricalValue(ValueAt::new(crate::burst![42u64], NanoTime::new(5)));
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: Message<u64> = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn serde_roundtrip_historical_value_multi() {
        let msg =
            Message::HistoricalValue(ValueAt::new(crate::burst![1u64, 2, 3], NanoTime::new(7)));
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: Message<u64> = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn serde_roundtrip_realtime_value() {
        let msg = Message::RealtimeValue(99u64);
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: Message<u64> = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn serde_roundtrip_error_variant() {
        use std::sync::Arc;
        let err_msg = "something went wrong";
        let msg: Message<u64> = Message::Error(Arc::new(anyhow::anyhow!(err_msg)));
        let json = serde_json::to_string(&msg).unwrap();
        // Error deserializes back to an Error variant with the original message
        let decoded: Message<u64> = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Message::Error(_)));
        if let Message::Error(e) = decoded {
            assert!(e.to_string().contains(err_msg));
        }
    }
}
