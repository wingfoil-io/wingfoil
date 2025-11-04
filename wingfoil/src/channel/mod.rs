pub mod message;

pub mod kanal_chan;
pub use kanal_chan::*;

// pub mod ring_buffer;
// pub mod ring_buffer_chan;
// pub use ring_buffer_chan::*;

pub(crate) use message::*;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendNodeError {
    ChannelClosed,
    NotifierClosed,
}

impl fmt::Display for SendNodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendNodeError::ChannelClosed => f.write_str("channel closed"),
            SendNodeError::NotifierClosed => f.write_str("ready-notifier dropped"),
        }
    }
}

impl std::error::Error for SendNodeError {}

pub type SendResult = Result<(), SendNodeError>;

use crate::graph::ReadyNotifier;
pub type NotifierChannelSender = kanal::Sender<ReadyNotifier>;
