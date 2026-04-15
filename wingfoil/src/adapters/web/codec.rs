//! Wire codec for the `web` adapter.
//!
//! Two formats are supported: [`CodecKind::Bincode`] (compact binary, the
//! default) and [`CodecKind::Json`] (human-readable for dev/debug in the
//! browser devtools). Both produce binary WebSocket frames — [`Envelope`]
//! bytes in the bincode case, UTF-8 JSON bytes in the JSON case.
//!
//! The same encoder is used for server → client and client → server
//! payloads. Envelopes are shared between the server and the
//! `wingfoil-wasm` client via the [`wingfoil_wire_types`] crate.

use anyhow::Context as _;
use serde::{Serialize, de::DeserializeOwned};

pub use wingfoil_wire_types::{
    CONTROL_TOPIC, CodecKind, ControlMessage, Envelope, WIRE_PROTOCOL_VERSION,
};

/// The wire protocol version advertised by this build.
pub fn wire_version() -> u16 {
    WIRE_PROTOCOL_VERSION
}

/// A pair of (envelope codec, payload codec). Both use the same format.
#[derive(Debug, Clone, Copy, Default)]
pub struct Codec(pub CodecKind);

impl Codec {
    /// The default bincode codec.
    pub const BINCODE: Codec = Codec(CodecKind::Bincode);
    /// The dev/debug JSON codec.
    pub const JSON: Codec = Codec(CodecKind::Json);

    /// Serialize a user value `T` into a payload byte buffer.
    pub fn encode_payload<T: Serialize>(&self, value: &T) -> anyhow::Result<Vec<u8>> {
        match self.0 {
            CodecKind::Bincode => bincode::serialize(value).context("web codec: bincode encode"),
            CodecKind::Json => serde_json::to_vec(value).context("web codec: json encode"),
        }
    }

    /// Deserialize a user value `T` from a payload byte buffer.
    pub fn decode_payload<T: DeserializeOwned>(&self, bytes: &[u8]) -> anyhow::Result<T> {
        match self.0 {
            CodecKind::Bincode => bincode::deserialize(bytes).context("web codec: bincode decode"),
            CodecKind::Json => serde_json::from_slice(bytes).context("web codec: json decode"),
        }
    }

    /// Encode a full [`Envelope`] into a framed WebSocket message body.
    pub fn encode_envelope(&self, env: &Envelope) -> anyhow::Result<Vec<u8>> {
        match self.0 {
            CodecKind::Bincode => bincode::serialize(env).context("web codec: bincode envelope"),
            CodecKind::Json => serde_json::to_vec(env).context("web codec: json envelope"),
        }
    }

    /// Decode a full [`Envelope`] from a WebSocket message body.
    pub fn decode_envelope(&self, bytes: &[u8]) -> anyhow::Result<Envelope> {
        match self.0 {
            CodecKind::Bincode => {
                bincode::deserialize(bytes).context("web codec: bincode envelope decode")
            }
            CodecKind::Json => {
                serde_json::from_slice(bytes).context("web codec: json envelope decode")
            }
        }
    }

    /// The wire codec kind.
    pub fn kind(&self) -> CodecKind {
        self.0
    }
}

impl From<CodecKind> for Codec {
    fn from(kind: CodecKind) -> Self {
        Codec(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bincode_envelope_roundtrip() {
        let codec = Codec::BINCODE;
        let env = Envelope {
            topic: "order_book".to_string(),
            time_ns: 123_456_789,
            payload: vec![1, 2, 3, 4],
        };
        let bytes = codec.encode_envelope(&env).unwrap();
        let back = codec.decode_envelope(&bytes).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn json_envelope_roundtrip() {
        let codec = Codec::JSON;
        let env = Envelope {
            topic: "ui_events".to_string(),
            time_ns: 42,
            payload: b"{}".to_vec(),
        };
        let bytes = codec.encode_envelope(&env).unwrap();
        let back = codec.decode_envelope(&bytes).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn bincode_payload_roundtrip() {
        let codec = Codec::BINCODE;
        let value = (42u32, "hello".to_string());
        let bytes = codec.encode_payload(&value).unwrap();
        let back: (u32, String) = codec.decode_payload(&bytes).unwrap();
        assert_eq!(value, back);
    }

    #[test]
    fn control_message_hello_roundtrip() {
        let codec = Codec::BINCODE;
        let ctrl = ControlMessage::Hello {
            codec: CodecKind::Bincode,
            version: 1,
        };
        let bytes = codec.encode_payload(&ctrl).unwrap();
        let back: ControlMessage = codec.decode_payload(&bytes).unwrap();
        assert_eq!(ctrl, back);
    }

    #[test]
    fn control_message_subscribe_roundtrip() {
        let codec = Codec::JSON;
        let ctrl = ControlMessage::Subscribe {
            topics: vec!["a".into(), "b".into()],
        };
        let bytes = codec.encode_payload(&ctrl).unwrap();
        let back: ControlMessage = codec.decode_payload(&bytes).unwrap();
        assert_eq!(ctrl, back);
    }

    #[test]
    fn bincode_rejects_corrupt_envelope() {
        let codec = Codec::BINCODE;
        let err = codec.decode_envelope(&[0xFF; 4]).unwrap_err();
        assert!(err.to_string().contains("web codec"));
    }
}
