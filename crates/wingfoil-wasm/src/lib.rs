//! Browser-side WebAssembly codec for the wingfoil `web` adapter.
//!
//! Compiles to `wasm32-unknown-unknown` via `wasm-pack` and is consumed
//! by the `@wingfoil/client` npm package. All encode / decode logic
//! lives on [`CodecKind`] in the shared [`wingfoil_wire_types`] crate,
//! so the browser cannot drift out of sync with the server.
//!
//! ```sh
//! cd wingfoil-wasm && wasm-pack build --target web --release
//! ```
//!
//! ## JS usage
//!
//! Call [`init_panic_hook`] once after the wasm module loads; the rest
//! of the exports are plain functions.

use serde::Serialize;
use wasm_bindgen::prelude::*;

pub use wingfoil_wire_types::{
    CONTROL_TOPIC, CodecKind, ControlMessage, Envelope, WIRE_PROTOCOL_VERSION,
};

/// Route Rust panics to `console.error`. Call once after the wasm module
/// loads.
#[wasm_bindgen]
pub fn init_panic_hook() {
    #[cfg(feature = "panic-hook")]
    console_error_panic_hook::set_once();
}

/// The wire protocol version this wasm build speaks.
#[wasm_bindgen(js_name = wireVersion)]
pub fn wire_version() -> u16 {
    WIRE_PROTOCOL_VERSION
}

/// Name of the reserved control topic.
#[wasm_bindgen(js_name = controlTopic)]
pub fn control_topic() -> String {
    CONTROL_TOPIC.to_string()
}

/// Decode an [`Envelope`] from a raw WebSocket binary frame body.
///
/// Returns a JS object of the form
/// `{ topic: string, timeNs: bigint, payload: Uint8Array }`.
#[wasm_bindgen(js_name = decodeEnvelope)]
pub fn decode_envelope(codec: &str, bytes: &[u8]) -> Result<JsValue, JsError> {
    let env: Envelope = parse_codec(codec)?.decode(bytes).map_err(to_js_error)?;
    JsEnvelope::from(env).into_js()
}

/// Encode an [`Envelope`] to a WebSocket binary frame body.
#[wasm_bindgen(js_name = encodeEnvelope)]
pub fn encode_envelope(
    codec: &str,
    topic: &str,
    time_ns: u64,
    payload: &[u8],
) -> Result<Vec<u8>, JsError> {
    let env = Envelope {
        topic: topic.to_string(),
        time_ns,
        payload: payload.to_vec(),
    };
    parse_codec(codec)?.encode(&env).map_err(to_js_error)
}

/// Encode a [`ControlMessage::Subscribe`] frame.
#[wasm_bindgen(js_name = encodeSubscribe)]
pub fn encode_subscribe(codec: &str, topics: Vec<String>) -> Result<Vec<u8>, JsError> {
    encode_control_frame(codec, &ControlMessage::Subscribe { topics })
}

/// Encode a [`ControlMessage::Unsubscribe`] frame.
#[wasm_bindgen(js_name = encodeUnsubscribe)]
pub fn encode_unsubscribe(codec: &str, topics: Vec<String>) -> Result<Vec<u8>, JsError> {
    encode_control_frame(codec, &ControlMessage::Unsubscribe { topics })
}

/// Encode a JSON-compatible JS value under `topic` as a full binary
/// frame (ready to send on the WebSocket). Used for client → server
/// publishes.
///
/// For the JSON codec we sidestep `serde_wasm_bindgen` and use the JS
/// engine's own `JSON.stringify` to produce the payload bytes. The
/// detour through `serde_json::Value` forces JS Numbers outside the
/// safe-integer range (e.g. nanosecond timestamps ~1.78e18) through
/// `f64`, which serde_json then writes as JSON floats — and the
/// server's `u64` fields reject floating-point input. JS's native
/// `JSON.stringify` writes integer-valued Numbers without a decimal
/// regardless of magnitude, so the server decodes cleanly.
///
/// For bincode we still need the serde round-trip because there is no
/// JS-native bincode encoder.
#[wasm_bindgen(js_name = encodePayload)]
pub fn encode_payload(codec: &str, topic: &str, value: JsValue) -> Result<Vec<u8>, JsError> {
    let codec = parse_codec(codec)?;
    let payload: Vec<u8> = match codec {
        CodecKind::Json => js_sys::JSON::stringify(&value)
            .map(|s| String::from(s).into_bytes())
            .map_err(|e| JsError::new(&format!("encode_payload: JSON.stringify: {e:?}")))?,
        CodecKind::Bincode => {
            let json: serde_json::Value = serde_wasm_bindgen::from_value(value)
                .map_err(|e| JsError::new(&format!("encode_payload: from JS: {e}")))?;
            codec.encode(&json).map_err(to_js_error)?
        }
    };
    let env = Envelope {
        topic: topic.to_string(),
        time_ns: 0,
        payload,
    };
    codec.encode(&env).map_err(to_js_error)
}

/// Decode the payload bytes of an envelope into a JS value.
///
/// JSON: parse with `JSON.parse` rather than the serde_json →
/// `serde_wasm_bindgen` round-trip. The latter rejects u64 values past
/// `Number.MAX_SAFE_INTEGER` outright; `JSON.parse` accepts them with
/// the standard JS Number precision loss (still integer-valued, just
/// approximated).
#[wasm_bindgen(js_name = decodePayload)]
pub fn decode_payload(codec: &str, bytes: &[u8]) -> Result<JsValue, JsError> {
    let codec = parse_codec(codec)?;
    match codec {
        CodecKind::Json => {
            let s = std::str::from_utf8(bytes)
                .map_err(|e| JsError::new(&format!("decode_payload: utf8: {e}")))?;
            js_sys::JSON::parse(s)
                .map_err(|e| JsError::new(&format!("decode_payload: JSON.parse: {e:?}")))
        }
        CodecKind::Bincode => {
            let json: serde_json::Value = codec.decode(bytes).map_err(to_js_error)?;
            serde_wasm_bindgen::to_value(&json)
                .map_err(|e| JsError::new(&format!("decode_payload: to JS: {e}")))
        }
    }
}

/// Decode a control-topic payload into a JS object.
#[wasm_bindgen(js_name = decodeControl)]
pub fn decode_control(codec: &str, bytes: &[u8]) -> Result<JsValue, JsError> {
    let ctrl: ControlMessage = parse_codec(codec)?.decode(bytes).map_err(to_js_error)?;
    serde_wasm_bindgen::to_value(&ctrl).map_err(|e| JsError::new(&format!("decode_control: {e}")))
}

// ---- internal helpers ----

fn parse_codec(s: &str) -> Result<CodecKind, JsError> {
    match s {
        "bincode" => Ok(CodecKind::Bincode),
        "json" => Ok(CodecKind::Json),
        other => Err(JsError::new(&format!(
            "unknown codec '{other}' (expected 'bincode' or 'json')"
        ))),
    }
}

fn encode_control_frame(codec: &str, ctrl: &ControlMessage) -> Result<Vec<u8>, JsError> {
    let codec = parse_codec(codec)?;
    let payload = codec.encode(ctrl).map_err(to_js_error)?;
    let env = Envelope {
        topic: CONTROL_TOPIC.to_string(),
        time_ns: 0,
        payload,
    };
    codec.encode(&env).map_err(to_js_error)
}

fn to_js_error(e: anyhow::Error) -> JsError {
    JsError::new(&format!("{e:#}"))
}

/// Wire-format DTO shown to JS. Renames `time_ns` to `timeNs` and uses
/// `serde_bytes::ByteBuf` so `payload` round-trips as a `Uint8Array`
/// instead of a JSON array of integers.
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
        // `serialize_large_number_types_as_bigints` — u64 time_ns > 2^53
        // would lose precision as a JS Number, so emit it as a BigInt.
        let serializer =
            serde_wasm_bindgen::Serializer::new().serialize_large_number_types_as_bigints(true);
        self.serialize(&serializer)
            .map_err(|e| JsError::new(&format!("envelope to JS: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_codec_accepts_bincode_and_json() {
        // `JsError::new` panics off-wasm, so only verify the success
        // path here; the error path is covered by `wasm-pack test`.
        assert!(matches!(parse_codec("bincode"), Ok(CodecKind::Bincode)));
        assert!(matches!(parse_codec("json"), Ok(CodecKind::Json)));
    }

    #[test]
    fn bincode_envelope_rust_roundtrip() {
        let env = Envelope {
            topic: "t".into(),
            time_ns: 1,
            payload: vec![1, 2, 3],
        };
        let bytes = CodecKind::Bincode.encode(&env).unwrap();
        let back: Envelope = CodecKind::Bincode.decode(&bytes).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn json_control_rust_roundtrip() {
        let ctrl = ControlMessage::Subscribe {
            topics: vec!["a".into(), "b".into()],
        };
        let bytes = CodecKind::Json.encode(&ctrl).unwrap();
        let back: ControlMessage = CodecKind::Json.decode(&bytes).unwrap();
        assert_eq!(back, ctrl);
    }
}
