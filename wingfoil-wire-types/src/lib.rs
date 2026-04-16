//! Wire-format types shared between the wingfoil server's `web` adapter
//! and the `wingfoil-wasm` browser client.
//!
//! Putting these types in a dedicated crate lets both the native server
//! (`wingfoil` compiled for x86_64 / aarch64) and the WebAssembly client
//! (`wingfoil-wasm` compiled for `wasm32-unknown-unknown`) depend on the
//! same [`Envelope`] struct and share the same [`CodecKind`] methods,
//! so wire compatibility is enforced at compile time.

#![cfg_attr(docsrs, feature(doc_auto_cfg))]

use anyhow::Context as _;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Protocol version. Bumped when the wire format changes in a
/// non-backwards-compatible way. Hello frames carry this value so clients
/// and servers can reject mismatched peers early.
pub const WIRE_PROTOCOL_VERSION: u16 = 1;

/// The dedicated topic name for control frames.
pub const CONTROL_TOPIC: &str = "$ctrl";

/// The envelope used for every binary WebSocket frame in both directions.
///
/// Server → client: `time_ns` is the graph engine time when the value was
/// produced; `payload` is the user type serialized by the active
/// [`CodecKind`]. Client → server: `time_ns` is ignored (clients cannot set
/// graph time); `payload` is the user type serialized by the active codec.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Envelope {
    /// The topic this frame belongs to. Keep this short — it is sent on
    /// every frame. [`CONTROL_TOPIC`] is reserved for control messages.
    pub topic: String,
    /// Graph time in nanoseconds since the UNIX epoch when the value was
    /// emitted. Zero for client-originated frames.
    pub time_ns: u64,
    /// The serialized user value (or [`ControlMessage`] for the control topic).
    pub payload: Vec<u8>,
}

/// Control-plane messages exchanged on the control topic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ControlMessage {
    /// Sent by the server to each newly connected client immediately
    /// after the WebSocket upgrade.
    Hello {
        /// The codec the server is using on this connection.
        codec: CodecKind,
        /// Server-side protocol version.
        version: u16,
    },
    /// Sent by the client to subscribe to one or more topics.
    Subscribe { topics: Vec<String> },
    /// Sent by the client to unsubscribe from one or more topics.
    Unsubscribe { topics: Vec<String> },
}

/// The serialization format used for envelope payloads and envelopes.
///
/// `Bincode` is the default — compact and fast. `Json` is an escape hatch
/// for debugging in browser devtools.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum CodecKind {
    #[default]
    Bincode,
    Json,
}

impl CodecKind {
    /// Serialize a value to its wire bytes.
    pub fn encode<T: Serialize>(self, value: &T) -> anyhow::Result<Vec<u8>> {
        match self {
            CodecKind::Bincode => bincode::serialize(value).context("wire codec: bincode encode"),
            CodecKind::Json => serde_json::to_vec(value).context("wire codec: json encode"),
        }
    }

    /// Deserialize a value from its wire bytes.
    pub fn decode<T: DeserializeOwned>(self, bytes: &[u8]) -> anyhow::Result<T> {
        match self {
            CodecKind::Bincode => bincode::deserialize(bytes).context("wire codec: bincode decode"),
            CodecKind::Json => serde_json::from_slice(bytes).context("wire codec: json decode"),
        }
    }
}
