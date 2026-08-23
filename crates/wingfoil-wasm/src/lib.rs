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
/// # The JSON codec is required for data payloads
///
/// **This function errors under [`CodecKind::Bincode`]** — see
/// [`BINCODE_PAYLOAD_UNSUPPORTED`]. bincode is schema-driven and
/// non-self-describing: a JS object can only be encoded as a
/// length-prefixed map (keys and all), while the server decodes with
/// `deserialize_struct`, which expects bare fields in declaration
/// order. The two do not line up, and the mismatch does not surface as
/// an error — the server decodes *silent garbage*. A browser has no
/// Rust schema, so it cannot produce a correct bincode encoding of a
/// server struct at all. Start the server with `.codec(CodecKind::Json)`
/// and construct the client with `codec: "json"`.
///
/// Envelope and control frames are unaffected: their shapes are fixed
/// by [`wingfoil_wire_types`] and known to both sides, so
/// [`encode_envelope`] / [`encode_subscribe`] work under either codec.
/// It is only the *user* payload — whose type only the server knows —
/// that bincode cannot carry from the browser.
///
/// For the JSON codec we sidestep `serde_wasm_bindgen` and use the JS
/// engine's own `JSON.stringify` to produce the payload bytes. The
/// detour through `serde_json::Value` forces JS Numbers outside the
/// safe-integer range (e.g. nanosecond timestamps ~1.78e18) through
/// `f64`, which serde_json then writes as JSON floats — and the
/// server's `u64` fields reject floating-point input. JS's native
/// `JSON.stringify` writes integer-valued Numbers without a decimal
/// regardless of magnitude, so the server decodes cleanly.
#[wasm_bindgen(js_name = encodePayload)]
pub fn encode_payload(codec: &str, topic: &str, value: JsValue) -> Result<Vec<u8>, JsError> {
    let codec = parse_codec(codec)?;
    let payload: Vec<u8> = match codec {
        CodecKind::Json => js_sys::JSON::stringify(&value)
            .map(|s| String::from(s).into_bytes())
            .map_err(|e| JsError::new(&format!("encodePayload: JSON.stringify: {e:?}")))?,
        CodecKind::Bincode => {
            return Err(JsError::new(&bincode_payload_error("encodePayload")));
        }
    };
    frame(codec, topic, payload).map_err(to_js_error)
}

/// Decode the payload bytes of an envelope into a JS value.
///
/// # The JSON codec is required for data payloads
///
/// **This function errors under [`CodecKind::Bincode`]** — see
/// [`BINCODE_PAYLOAD_UNSUPPORTED`] and the note on [`encode_payload`].
/// Decoding a bincode payload needs the Rust schema of the value the
/// server published, which the browser does not have; the schema-less
/// attempt (`decode::<serde_json::Value>`) always fails with
/// `DeserializeAnyNotSupported`, because bincode carries no type tags to
/// deserialize *any* from. Start the server with
/// `.codec(CodecKind::Json)` and construct the client with
/// `codec: "json"`.
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
                .map_err(|e| JsError::new(&format!("decodePayload: utf8: {e}")))?;
            js_sys::JSON::parse(s)
                .map_err(|e| JsError::new(&format!("decodePayload: JSON.parse: {e:?}")))
        }
        CodecKind::Bincode => Err(JsError::new(&bincode_payload_error("decodePayload"))),
    }
}

/// Decode a control-topic payload into a JS object.
#[wasm_bindgen(js_name = decodeControl)]
pub fn decode_control(codec: &str, bytes: &[u8]) -> Result<JsValue, JsError> {
    let ctrl: ControlMessage = parse_codec(codec)?.decode(bytes).map_err(to_js_error)?;
    serde_wasm_bindgen::to_value(&ctrl).map_err(|e| JsError::new(&format!("decode_control: {e}")))
}

/// Why the browser cannot carry a **data payload** under
/// [`CodecKind::Bincode`], and what to do instead.
///
/// Appended to an `encodePayload:` / `decodePayload:` prefix and surfaced
/// to JS as a `JsError`. This is a hard limitation of the format, not a
/// missing feature: bincode is schema-driven, and the browser has no Rust
/// schema.
pub const BINCODE_PAYLOAD_UNSUPPORTED: &str = "\
the bincode codec cannot carry browser data payloads. bincode is \
schema-driven and non-self-describing, so a JS value encodes as a \
length-prefixed map while the server's `deserialize_struct` expects bare \
fields in declaration order — the server would decode silent garbage rather \
than report an error — and a server-encoded struct cannot be decoded in the \
browser at all without its Rust schema. Use the JSON codec for browser \
payloads: start the server with `.codec(CodecKind::Json)` and construct the \
client with `codec: \"json\"`. Envelope and control frames are unaffected; \
this applies only to user data payloads.";

// ---- internal helpers ----

/// The `JsError` text for a browser payload attempted under bincode.
/// `func` is the JS-facing function name (`encodePayload` /
/// `decodePayload`) so the message points at the call that failed.
fn bincode_payload_error(func: &str) -> String {
    format!("{func}: {BINCODE_PAYLOAD_UNSUPPORTED}")
}

/// Wrap already-serialized `payload` bytes in an [`Envelope`] on `topic`
/// and encode the whole frame — the exact bytes that go on the wire.
///
/// `time_ns` is always zero: clients cannot set graph time, and the
/// server ignores the field on inbound frames.
fn frame(codec: CodecKind, topic: &str, payload: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    let env = Envelope {
        topic: topic.to_string(),
        time_ns: 0,
        payload,
    };
    codec.encode(&env)
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

fn encode_control_frame(codec: &str, ctrl: &ControlMessage) -> Result<Vec<u8>, JsError> {
    let codec = parse_codec(codec)?;
    // Control messages *are* schema-known on both sides, so unlike user
    // data payloads they encode correctly under either codec.
    let payload = codec.encode(ctrl).map_err(to_js_error)?;
    frame(codec, CONTROL_TOPIC, payload).map_err(to_js_error)
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
        // path here; the error path requires a wasm target.
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

    // ---- browser payload codec: the four directions ----
    //
    // The server-side struct a browser publishes to / receives from. Kept
    // byte-identical in shape to `UiClick` in
    // `crates/wingfoil/tests/web_integration.rs`, which pins the same four
    // facts from the server's side of the socket.
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct UiClick {
        button: String,
        count: u32,
    }

    fn a_click() -> UiClick {
        UiClick {
            button: "ok".to_string(),
            count: 1,
        }
    }

    /// **Client → server, JSON: works.**
    ///
    /// Reproduces `encode_payload`'s JSON leg — the payload bytes are the
    /// JS engine's `JSON.stringify` output, which for this value is
    /// byte-identical to `serde_json::to_vec` — then wraps it with the
    /// same [`frame`] helper the exported function uses, and decodes it
    /// the way the server's `web_sub` does (`Envelope`, then
    /// `codec.decode::<T>(&env.payload)`).
    #[test]
    fn json_payload_client_to_server_round_trip() {
        let click = a_click();
        let payload = serde_json::to_vec(&click).unwrap();
        let bytes = frame(CodecKind::Json, "ui_events", payload).unwrap();

        // --- server side ---
        let env: Envelope = CodecKind::Json.decode(&bytes).unwrap();
        assert_eq!(env.topic, "ui_events");
        assert_eq!(env.time_ns, 0);
        let decoded: UiClick = CodecKind::Json.decode(&env.payload).unwrap();
        assert_eq!(decoded, click);
    }

    /// **Server → client, JSON: works.**
    ///
    /// The server encodes the struct with its codec; `decode_payload`'s
    /// JSON leg is `str::from_utf8` + `JSON.parse`. `JSON.parse` cannot run
    /// off-wasm, so the parse is stood in for by `serde_json` — what is
    /// being pinned here is that the server's payload bytes *are* valid
    /// UTF-8 JSON with the expected fields, which is exactly what that leg
    /// requires.
    #[test]
    fn json_payload_server_to_client_round_trip() {
        let click = a_click();
        let payload = CodecKind::Json.encode(&click).unwrap();

        let s = std::str::from_utf8(&payload).expect("JSON payload is UTF-8");
        let parsed: serde_json::Value = serde_json::from_str(s).unwrap();
        assert_eq!(parsed["button"], serde_json::json!("ok"));
        assert_eq!(parsed["count"], serde_json::json!(1));
    }

    /// **Client → server, bincode: silently corrupt — hence rejected.**
    ///
    /// This is the bug behind the rejection. The browser can only offer a
    /// schema-less `serde_json::Value`; bincode writes it as a
    /// length-prefixed map, and the server's `deserialize_struct` reads
    /// bare fields in declaration order. The result is *not* an error, it
    /// is garbage — which is why `encode_payload` refuses rather than
    /// letting the value onto the wire.
    #[test]
    fn bincode_payload_client_to_server_would_corrupt() {
        let click = a_click();
        let as_json: serde_json::Value = serde_json::to_value(&click).unwrap();
        let payload = CodecKind::Bincode.encode(&as_json).unwrap();

        match CodecKind::Bincode.decode::<UiClick>(&payload) {
            Ok(garbage) => assert_ne!(
                garbage, click,
                "bincode decoded a browser payload *correctly*: if this ever \
                 holds, revisit the encodePayload rejection"
            ),
            Err(_) => { /* also fine — either way the value never arrives */ }
        }
    }

    /// **Server → client, bincode: impossible — hence rejected.**
    ///
    /// The browser has no schema for the published type, so the only
    /// schema-less decode available (`serde_json::Value`) hits bincode's
    /// `DeserializeAnyNotSupported`. `decode_payload` now says so up front
    /// instead of surfacing that error per frame.
    #[test]
    fn bincode_payload_server_to_client_cannot_decode() {
        let payload = CodecKind::Bincode.encode(&a_click()).unwrap();
        let err = CodecKind::Bincode
            .decode::<serde_json::Value>(&payload)
            .expect_err("schema-less bincode decode must fail");
        assert!(
            format!("{err:#}").contains("deserialize_any"),
            "unexpected bincode error: {err:#}"
        );
    }

    /// The rejection message names the failing call and points at the fix.
    /// `JsError::new` panics off-wasm, so the exported functions cannot be
    /// called here — the message builder they both use is asserted instead.
    #[test]
    fn bincode_payload_error_is_actionable() {
        for func in ["encodePayload", "decodePayload"] {
            let msg = bincode_payload_error(func);
            assert!(msg.starts_with(&format!("{func}: ")), "{msg}");
            assert!(msg.contains("bincode codec cannot carry browser data payloads"));
            assert!(msg.contains("CodecKind::Json"));
            assert!(msg.contains("codec: \"json\""));
        }
    }
}
