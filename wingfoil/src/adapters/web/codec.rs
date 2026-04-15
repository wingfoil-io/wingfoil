//! Wire codec re-exports for the `web` adapter.
//!
//! All encode / decode logic lives on [`CodecKind`] in the shared
//! [`wingfoil_wire_types`] crate so the server and the `wingfoil-wasm`
//! browser client can't diverge.

pub use wingfoil_wire_types::{
    CONTROL_TOPIC, CodecKind, ControlMessage, Envelope, WIRE_PROTOCOL_VERSION,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bincode_envelope_roundtrip() {
        let env = Envelope {
            topic: "order_book".to_string(),
            time_ns: 123_456_789,
            payload: vec![1, 2, 3, 4],
        };
        let bytes = CodecKind::Bincode.encode(&env).unwrap();
        let back: Envelope = CodecKind::Bincode.decode(&bytes).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn json_envelope_roundtrip() {
        let env = Envelope {
            topic: "ui_events".to_string(),
            time_ns: 42,
            payload: b"{}".to_vec(),
        };
        let bytes = CodecKind::Json.encode(&env).unwrap();
        let back: Envelope = CodecKind::Json.decode(&bytes).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn control_message_hello_roundtrip() {
        let ctrl = ControlMessage::Hello {
            codec: CodecKind::Bincode,
            version: WIRE_PROTOCOL_VERSION,
        };
        let bytes = CodecKind::Bincode.encode(&ctrl).unwrap();
        let back: ControlMessage = CodecKind::Bincode.decode(&bytes).unwrap();
        assert_eq!(ctrl, back);
    }

    #[test]
    fn control_message_subscribe_roundtrip() {
        let ctrl = ControlMessage::Subscribe {
            topics: vec!["a".into(), "b".into()],
        };
        let bytes = CodecKind::Json.encode(&ctrl).unwrap();
        let back: ControlMessage = CodecKind::Json.decode(&bytes).unwrap();
        assert_eq!(ctrl, back);
    }

    #[test]
    fn bincode_rejects_corrupt_envelope() {
        let err = CodecKind::Bincode
            .decode::<Envelope>(&[0xFF; 4])
            .unwrap_err();
        assert!(err.to_string().contains("wire codec"));
    }
}
