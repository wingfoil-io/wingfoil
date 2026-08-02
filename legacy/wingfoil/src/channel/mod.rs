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

#[cfg(feature = "async")]
use crate::graph::ReadyNotifier;
#[cfg(feature = "async")]
pub type NotifierChannelSender = kanal::Sender<ReadyNotifier>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_closed_display() {
        assert_eq!(SendNodeError::ChannelClosed.to_string(), "channel closed");
    }

    #[test]
    fn notifier_closed_display() {
        assert_eq!(
            SendNodeError::NotifierClosed.to_string(),
            "ready-notifier dropped"
        );
    }

    #[test]
    fn send_node_error_is_error() {
        let e: &dyn std::error::Error = &SendNodeError::ChannelClosed;
        assert!(e.source().is_none());
    }

    #[test]
    fn send_node_error_eq_and_copy() {
        let a = SendNodeError::ChannelClosed;
        let b = a; // Copy
        assert_eq!(a, b);
        assert_ne!(SendNodeError::ChannelClosed, SendNodeError::NotifierClosed);
    }
}
