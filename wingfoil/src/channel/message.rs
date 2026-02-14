use derive_new::new;
use serde::{Deserialize, Serialize};
use std::boxed::Box;
use std::pin::Pin;
use std::sync::Arc;

use crate::queue::ValueAt;
use crate::time::NanoTime;
use crate::types::Element;
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
    pub fn build(value: T, graph_state: &GraphState) -> Message<T> {
        match graph_state.run_mode() {
            RunMode::RealTime => Message::RealtimeValue(value),
            RunMode::HistoricalFrom(_) => {
                Message::HistoricalValue(ValueAt::new(value, graph_state.time()))
            }
        }
    }
}

pub trait ReceiverMessageSource<T: Element + Send> {
    fn to_boxed_message_stream(self) -> Pin<Box<dyn futures::Stream<Item = Message<T>> + Send>>;
}
