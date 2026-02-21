use derive_new::new;
#[cfg(feature = "async")]
use std::boxed::Box;
#[cfg(feature = "async")]
use std::pin::Pin;
use std::sync::Arc;

use crate::queue::ValueAt;
use crate::time::NanoTime;
use crate::types::Element;

/// Message that can be sent between threads.
#[derive(new, Debug, Clone)]
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
    Error(Arc<anyhow::Error>),
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

#[cfg(feature = "async")]
pub trait ReceiverMessageSource<T: Element + Send> {
    fn to_boxed_message_stream(self) -> Pin<Box<dyn futures::Stream<Item = Message<T>> + Send>>;
}
