use derive_new::new;
use serde::{Deserialize, Serialize};
#[cfg(feature = "async")]
use std::boxed::Box;
#[cfg(feature = "async")]
use std::pin::Pin;
use std::sync::Arc;

use crate::queue::ValueAt;
use crate::time::NanoTime;
use crate::types::Element;
#[cfg(feature = "zmq-beta")]
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
    /// Used in [RunMode::HistoricalFrom].  A value and its
    /// associated time.
    HistoricalValue(ValueAt<T>),
    /// Sent in [RunMode::RealTime].  Just a value.
    RealtimeValue(T),
    /// Used in [RunMode::HistoricalFrom] for bulk data transfer.
    /// Contains multiple values with their timestamps.
    HistoricalBatch(Box<[ValueAt<T>]>),
    /// Error message that can be propagated through channels.
    /// When received, the receiver will immediately return the error.
    Error(#[serde(with = "arc_error_serde")] Arc<anyhow::Error>),
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
            (Message::HistoricalBatch(b1), Message::HistoricalBatch(b2)) => b1 == b2,
            (Message::Error(_), Message::Error(_)) => false,
            _ => false,
        }
    }
}

impl<T: Element + Send + PartialEq> Eq for Message<T> {}

impl<T: Element + Send> Message<T> {
    #[cfg(feature = "zmq-beta")]
    pub fn build(value: T, graph_state: &GraphState) -> Message<T> {
        match graph_state.run_mode() {
            RunMode::RealTime => Message::RealtimeValue(value),
            RunMode::HistoricalFrom(_) => {
                Message::HistoricalValue(ValueAt::new(value, graph_state.time()))
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
        let v1 = ValueAt::new(1u64, NanoTime::new(10));
        let v2 = ValueAt::new(1u64, NanoTime::new(10));
        let v3 = ValueAt::new(2u64, NanoTime::new(10));
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
    fn historical_batch_eq() {
        let b1: Box<[ValueAt<u64>]> = vec![ValueAt::new(1, NanoTime::new(1))].into_boxed_slice();
        let b2: Box<[ValueAt<u64>]> = vec![ValueAt::new(1, NanoTime::new(1))].into_boxed_slice();
        assert_eq!(Message::HistoricalBatch(b1), Message::HistoricalBatch(b2));
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
        let msg = Message::HistoricalValue(ValueAt::new(42u64, NanoTime::new(5)));
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
    fn serde_roundtrip_historical_batch() {
        let batch: Box<[ValueAt<u64>]> = vec![
            ValueAt::new(1, NanoTime::new(1)),
            ValueAt::new(2, NanoTime::new(2)),
        ]
        .into_boxed_slice();
        let msg = Message::HistoricalBatch(batch);
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: Message<u64> = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }
}
