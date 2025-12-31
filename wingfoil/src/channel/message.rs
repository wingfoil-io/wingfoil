use derive_new::new;
use serde::{Deserialize, Serialize};
use std::boxed::Box;
use std::pin::Pin;

use crate::{GraphState, RunMode};
use crate::queue::ValueAt;
use crate::time::NanoTime;
use crate::types::Element;

/// Message that can be sent between threads.
#[derive(new, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

impl <T: Element + Send> Message<T> {
    pub fn build(value: T, graph_state: &GraphState) -> Message<T> {
        match graph_state.run_mode() {
            RunMode::RealTime => {
                Message::RealtimeValue(value)
            },
            RunMode::HistoricalFrom(_) => {
                Message::HistoricalValue(ValueAt::new(value, graph_state.time()))
            }
        }
    }
}

pub trait ReceiverMessageSource<T: Element + Send> {
    fn to_boxed_message_stream(self) -> Pin<Box<dyn futures::Stream<Item = Message<T>> + Send>>;
}
