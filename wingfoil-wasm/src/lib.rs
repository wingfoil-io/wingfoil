//! Browser-side WebAssembly codec for the wingfoil `web` adapter.
//!
//! Compiles to `wasm32-unknown-unknown` via `wasm-pack` and is consumed
//! by the `@wingfoil/client` npm package. Exposes encoder / decoder
//! helpers that read and write the same [`Envelope`] format used by the
//! Rust server — Rust is the single source of truth for the wire
//! schema, so there is no hand-maintained TypeScript mirror.
//!
//! ```sh
//! # Build the wasm bundle:
//! cd wingfoil-wasm && wasm-pack build --target web --release
//! # Output in ./pkg is consumed by wingfoil-js.
//! ```

use serde::{Serialize, de::DeserializeOwned};
use wasm_bindgen::prelude::*;

pub use wingfoil_wire_types::{
    CONTROL_TOPIC, CodecKind, ControlMessage, Envelope, WIRE_PROTOCOL_VERSION,
};

/// Install the panic hook so Rust panics land in `console.error`. Safe
/// to call multiple times. Automatically called by [`decode_envelope`]
/// and [`encode_envelope`] on first use.
#[wasm_bindgen]
pub fn install_panic_hook() {
    #[cfg(feature = "panic-hook")]
    console_error_panic_hook::set_once();
}

/// The wire protocol version this wasm build speaks.
#[wasm_bindgen]
pub fn wire_version() -> u16 {
    WIRE_PROTOCOL_VERSION
}

/// Decode an [`Envelope`] from a raw WebSocket binary frame body.
///
/// `codec` must be one of `"bincode"` or `"json"` (matching the
/// server's codec from the Hello control frame).
///
/// Returned object shape:
///
/// ```json
/// { "topic": "tick", "timeNs": 12345, "payload": Uint8Array }
/// ```
#[wasm_bindgen]
pub fn decode_envelope(codec: &str, bytes: &[u8]) -> Result<JsValue, JsError> {
    install_panic_hook();
    let env: Envelope = decode_generic(codec, bytes)?;
    JsEnvelope::from(env).into_js()
}

/// Encode an [`Envelope`] to a WebSocket binary frame body.
#[wasm_bindgen]
pub fn encode_envelope(
    codec: &str,
    topic: &str,
    time_ns: u64,
    payload: &[u8],
) -> Result<Vec<u8>, JsError> {
    install_panic_hook();
    let env = Envelope {
        topic: topic.to_string(),
        time_ns,
        payload: payload.to_vec(),
    };
    encode_generic(codec, &env)
}

/// Encode a [`ControlMessage::Subscribe`] to a ready-to-send binary
/// frame body.
#[wasm_bindgen]
pub fn encode_subscribe(codec: &str, topics: Vec<String>) -> Result<Vec<u8>, JsError> {
    install_panic_hook();
    let ctrl = ControlMessage::Subscribe { topics };
    encode_control(codec, &ctrl)
}

/// Encode a [`ControlMessage::Unsubscribe`] to a ready-to-send binary
/// frame body.
#[wasm_bindgen]
pub fn encode_unsubscribe(codec: &str, topics: Vec<String>) -> Result<Vec<u8>, JsError> {
    install_panic_hook();
    let ctrl = ControlMessage::Unsubscribe { topics };
    encode_control(codec, &ctrl)
}

/// Encode a user value (supplied as a JSON-compatible JS object) under
/// `topic` as a ready-to-send binary frame body. Use this for
/// `client → server` publishes.
///
/// The JS value is first serialized to a Rust-compatible representation
/// via `serde-wasm-bindgen`, then to the wire codec's bytes. This
/// matches what the server's `web_sub::<T>` expects as long as the JS
/// object shape matches `T`'s Serde derives.
#[wasm_bindgen]
pub fn encode_payload(codec: &str, topic: &str, value: JsValue) -> Result<Vec<u8>, JsError> {
    install_panic_hook();
    // The value is a JSON-compatible JS object — round-trip through a
    // `serde_json::Value` so we work for both bincode and json codecs
    // without requiring the user to declare the Rust schema here.
    let json_value: serde_json::Value = serde_wasm_bindgen::from_value(value)
        .map_err(|e| JsError::new(&format!("encode_payload: from_value: {e}")))?;
    let payload = encode_generic(codec, &json_value)?;
    let env = Envelope {
        topic: topic.to_string(),
        time_ns: 0,
        payload,
    };
    encode_generic(codec, &env)
}

/// Decode the payload bytes of an envelope into a JS value. Pass the
/// `payload` from [`decode_envelope`]'s returned envelope.
#[wasm_bindgen]
pub fn decode_payload(codec: &str, bytes: &[u8]) -> Result<JsValue, JsError> {
    install_panic_hook();
    let json_value: serde_json::Value = decode_generic(codec, bytes)?;
    serde_wasm_bindgen::to_value(&json_value)
        .map_err(|e| JsError::new(&format!("decode_payload: to_value: {e}")))
}

/// Decode a control-topic payload into a JS object of the form
/// `{ type: "Hello", codec, version }` etc.
#[wasm_bindgen]
pub fn decode_control(codec: &str, bytes: &[u8]) -> Result<JsValue, JsError> {
    install_panic_hook();
    let ctrl: ControlMessage = decode_generic(codec, bytes)?;
    serde_wasm_bindgen::to_value(&ctrl).map_err(|e| JsError::new(&format!("decode_control: {e}")))
}

/// Name of the reserved control topic. Re-exported for JS convenience.
#[wasm_bindgen]
pub fn control_topic() -> String {
    CONTROL_TOPIC.to_string()
}

// ---- internal helpers ----

fn encode_generic<T: Serialize>(codec: &str, value: &T) -> Result<Vec<u8>, JsError> {
    match parse_codec(codec)? {
        CodecKind::Bincode => {
            bincode::serialize(value).map_err(|e| JsError::new(&format!("bincode: {e}")))
        }
        CodecKind::Json => {
            serde_json::to_vec(value).map_err(|e| JsError::new(&format!("json: {e}")))
        }
    }
}

fn decode_generic<T: DeserializeOwned>(codec: &str, bytes: &[u8]) -> Result<T, JsError> {
    match parse_codec(codec)? {
        CodecKind::Bincode => {
            bincode::deserialize(bytes).map_err(|e| JsError::new(&format!("bincode: {e}")))
        }
        CodecKind::Json => {
            serde_json::from_slice(bytes).map_err(|e| JsError::new(&format!("json: {e}")))
        }
    }
}

fn parse_codec(s: &str) -> Result<CodecKind, JsError> {
    match s {
        "bincode" => Ok(CodecKind::Bincode),
        "json" => Ok(CodecKind::Json),
        other => Err(JsError::new(&format!(
            "unknown codec '{other}' (expected 'bincode' or 'json')"
        ))),
    }
}

fn encode_control(codec: &str, ctrl: &ControlMessage) -> Result<Vec<u8>, JsError> {
    let payload = encode_generic(codec, ctrl)?;
    let env = Envelope {
        topic: CONTROL_TOPIC.to_string(),
        time_ns: 0,
        payload,
    };
    encode_generic(codec, &env)
}

#[derive(Serialize)]
struct JsEnvelope {
    topic: String,
    #[serde(rename = "timeNs")]
    time_ns: u64,
    payload: serde_bytes::ByteBuf,
}

impl From<Envelope> for JsEnvelope {
    fn from(env: Envelope) -> Self {
        Self {
            topic: env.topic,
            time_ns: env.time_ns,
            payload: serde_bytes::ByteBuf::from(env.payload),
        }
    }
}

impl JsEnvelope {
    fn into_js(self) -> Result<JsValue, JsError> {
        // Use the json_compatible serializer so `payload` becomes a real
        // `Uint8Array` in JS.
        let serializer =
            serde_wasm_bindgen::Serializer::new().serialize_large_number_types_as_bigints(true);
        self.serialize(&serializer)
            .map_err(|e| JsError::new(&format!("envelope to JsValue: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_codec_accepts_bincode_and_json() {
        // `parse_codec` returns a `JsError` on failure, and `JsError::new`
        // panics on non-wasm targets. Only verify the success paths here;
        // the error path is covered by `wasm-pack test`.
        assert!(matches!(parse_codec("bincode"), Ok(CodecKind::Bincode)));
        assert!(matches!(parse_codec("json"), Ok(CodecKind::Json)));
    }

    #[test]
    fn bincode_envelope_rust_roundtrip() {
        let bytes = encode_generic(
            "bincode",
            &Envelope {
                topic: "t".into(),
                time_ns: 1,
                payload: vec![1, 2, 3],
            },
        )
        .unwrap();
        let back: Envelope = decode_generic("bincode", &bytes).unwrap();
        assert_eq!(back.topic, "t");
        assert_eq!(back.time_ns, 1);
        assert_eq!(back.payload, vec![1, 2, 3]);
    }

    #[test]
    fn json_control_rust_roundtrip() {
        let bytes = encode_control(
            "json",
            &ControlMessage::Subscribe {
                topics: vec!["a".into(), "b".into()],
            },
        )
        .unwrap();
        let env: Envelope = decode_generic("json", &bytes).unwrap();
        assert_eq!(env.topic, CONTROL_TOPIC);
        let ctrl: ControlMessage = decode_generic("json", &env.payload).unwrap();
        match ctrl {
            ControlMessage::Subscribe { topics } => assert_eq!(topics, vec!["a", "b"]),
            _ => panic!("wrong variant"),
        }
    }
}
