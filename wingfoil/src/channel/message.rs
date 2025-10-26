use derive_new::new;
use std::boxed::Box;
use std::pin::Pin;

use crate::queue::ValueAt;
use crate::time::NanoTime;
use crate::types::Element;

/// Message that can be sent between threads.
#[derive(new, Debug, Clone, PartialEq, Eq)]
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
}

pub trait ReceiverMessageSource<T: Element + Send> {
    fn to_boxed_message_stream(self) -> Pin<Box<dyn futures::Stream<Item = Message<T>> + Send>>;
}
