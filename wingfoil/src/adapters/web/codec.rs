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
    fn control_message_complete_roundtrip() {
        let ctrl = ControlMessage::Complete {
            topic: "price".into(),
        };
        for codec in [CodecKind::Bincode, CodecKind::Json] {
            let bytes = codec.encode(&ctrl).unwrap();
            let back: ControlMessage = codec.decode(&bytes).unwrap();
            assert_eq!(ctrl, back);
        }
    }

    #[test]
    fn control_message_existing_variants_keep_wire_layout() {
        // `Complete` was appended after `Unsubscribe`, so the older
        // variants must keep their bincode representation (variant index).
        // A hardcoded byte check guards against an accidental reorder that
        // would silently break v1 peers.
        let hello = ControlMessage::Hello {
            codec: CodecKind::Bincode,
            version: 2,
        };
        let bytes = CodecKind::Bincode.encode(&hello).unwrap();
        assert_eq!(bytes[0..4], [0, 0, 0, 0], "Hello must stay variant 0");
        let sub = ControlMessage::Subscribe { topics: vec![] };
        let bytes = CodecKind::Bincode.encode(&sub).unwrap();
        assert_eq!(bytes[0..4], [1, 0, 0, 0], "Subscribe must stay variant 1");
    }

    #[test]
    fn bincode_rejects_corrupt_envelope() {
        let err = CodecKind::Bincode
            .decode::<Envelope>(&[0xFF; 4])
            .unwrap_err();
        assert!(err.to_string().contains("wire codec"));
    }
}
