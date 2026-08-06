mod read;

pub use read::*;
use serde::Deserialize;
use tungstenite::{Bytes, Utf8Bytes};

/// Connection status reported by the subscriber socket monitor.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub enum WebSocketStatus {
    Connected,
    #[default]
    Disconnected,
}

#[derive(Debug, Clone)]
pub enum WebSocketMessage {
    // default
    Text(Utf8Bytes),
    Binary(Bytes),
}

/// Internal per-message event wrapping either data or a status change.
#[derive(Debug, Clone)]
pub enum WebSocketEvent {
    Data(WebSocketMessage),
    //ConnectionInfo(String),
    Status(WebSocketStatus),
}

impl Default for WebSocketMessage {
    fn default() -> Self {
        WebSocketMessage::Text(tungstenite::Utf8Bytes::from_static(""))
    }
}

impl Default for WebSocketEvent {
    fn default() -> Self {
        WebSocketEvent::Data(WebSocketMessage::Text(tungstenite::Utf8Bytes::from_static(
            "",
        )))
    }
}
