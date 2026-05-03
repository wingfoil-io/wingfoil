# web Adapter

Bidirectional WebSocket streaming between a wingfoil graph and one or more
browsers. Publishes graph values to clients (`web_pub`) and exposes client
frames as a wingfoil source stream (`web_sub`).

## Module Structure

```
web/
  mod.rs               # Re-exports, module-level //! docs, example usage
  codec.rs             # Envelope + bincode/JSON encoding (backed by wingfoil-wire-types)
  server.rs            # WebServer + axum router + per-connection task
  write.rs             # web_pub() sink + WebPubOperators fluent trait
  read.rs              # web_sub() source
  integration_tests.rs # Ordinary `#[cfg(test)]`; in-process server + tungstenite client
  CLAUDE.md            # This file
```

## Key Design Decisions

- **Protocol: WebSocket (binary frames).** Bidirectional, kHz-capable, universal
  browser support. Not SSE (one-way, text-only) and not WebTransport (browser
  support still gated; complex TLS story).
- **Default codec: bincode** via [`wingfoil_wire_types::Envelope`]. JSON is
  available via `.codec(CodecKind::Json)` for debugging in browser devtools.
- **Shared wire types** live in the top-level `wingfoil-wire-types` crate so the
  server and the `wingfoil-wasm` browser client can't drift — wire mismatches
  become compile errors.
- **Axum 0.8** with `tokio-tungstenite` under the hood. Adds HTTP routing and
  `tower-http::services::ServeDir` for hosting the UI bundle on the same
  origin as the WebSocket endpoint.
- **Dedicated thread + current-thread tokio runtime** for the HTTP server
  (same pattern as `PrometheusExporter::serve`). Bind is synchronous so port
  conflicts surface before the graph starts.
- **Broadcast per publish topic** (`tokio::sync::broadcast`). Slow consumers
  are lossy — `broadcast::error::RecvError::Lagged` drops oldest frames so a
  frozen browser tab can never back-pressure the graph.
- **Bounded mpsc per connection outbound queue** and **per subscribe-topic
  listener queue**, both with `try_send` + drop-newest-under-overload so a
  misbehaving client cannot grow memory without bound or push graph latency.
- **Control plane on topic `"$ctrl"`**: `Hello { codec, version }` is sent by
  the server on upgrade; clients send `Subscribe { topics }` /
  `Unsubscribe { topics }` to manage forwarders.
- **Historical-mode safety**: `WebServerBuilder::start_historical()` returns
  a no-op server. Both `web_pub` and `web_sub` become no-ops so the same
  graph can run in `RunMode::HistoricalFrom(...)` without touching the
  network — mirrors `PrometheusMetricNode`'s `historical` flag.
- **Optional TLS via the `web-tls` feature**. Adds a `.tls(cert, key)`
  builder method that loads PEM files synchronously at `start()` time
  (so a missing/malformed cert surfaces alongside bind errors, before
  the graph starts) and serves over HTTPS / WSS. Implementation
  delegates the per-connection TLS handshake to `axum-server`'s
  rustls integration; the same axum `Router` (and therefore the
  existing `/ws` upgrade handler) is reused. The crypto provider is
  pinned to `ring` to match the FIX adapter.

## Threading Model

- Graph publishers and subscribers are async consumers/producers (via
  `consume_async` / `produce_async`). They run on the graph's shared tokio
  runtime.
- The HTTP/WS server runs on its own dedicated current-thread tokio runtime
  on a named `wingfoil-web` OS thread. The server is bound when `start()`
  is called and shut down when the `WebServer` handle drops.
- Per-connection handler spawns one writer task (drains outbound mpsc →
  WebSocket) and one forwarder task per subscribed publish topic (reads
  broadcast receiver → outbound mpsc).

## Wire Format

Every WebSocket binary frame is an `Envelope`:

```rust
pub struct Envelope {
    pub topic: String,   // e.g. "order_book" or "$ctrl"
    pub time_ns: u64,    // graph engine time (0 for client → server frames)
    pub payload: Vec<u8>, // bincode(T) or serde_json(T) of the user type
}
```

Control topic `"$ctrl"` carries a `ControlMessage`:

```rust
pub enum ControlMessage {
    Hello { codec: CodecKind, version: u16 }, // server → client on upgrade
    Subscribe   { topics: Vec<String> },      // client → server
    Unsubscribe { topics: Vec<String> },      // client → server
}
```

## Pre-Commit Requirements

```bash
# 1. Standard checks
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings

# 2. Unit + integration tests (no external service required)
cargo test --features web -p wingfoil \
  -- --test-threads=1 adapters::web

# 3. TLS round-trip (rcgen-generated self-signed cert + rustls client)
cargo test --features web-tls-integration-test -p wingfoil \
  -- --test-threads=1 adapters::web::integration_tests::test_pub_round_trip_tls
```

Integration tests run entirely in-process — there is no Docker container
dependency. Tests that need both a server and a client use a dedicated thread
with its own `tokio::runtime::Runtime` for the client so the graph's `Rc`
nodes remain on the original thread.

## Gotchas

- The WebSocket endpoint is `GET /ws`. Set your client URL to
  `ws://HOST:PORT/ws`.
- `WebServer::bind("127.0.0.1:0")` is the recommended pattern for tests —
  read back the bound port via `WebServer::port()` afterwards.
- `broadcast::channel` refuses to `send` when there are no subscribers; this
  is expected and silently ignored in `web_pub`. A graph can publish with
  zero clients connected — values are simply dropped.
- A `web_sub::<T>` listener's mpsc is registered eagerly at construction
  time, so frames sent by a client that connects *before* the graph starts
  are buffered (up to the mpsc capacity) rather than lost.
- Tests use `--test-threads=1` only as a safety measure; they bind port 0
  and are independent, so parallel execution also works. The `=1` matches
  the rest of the wingfoil adapter test suite.

## wingfoil-wasm browser client

The sibling `wingfoil-wasm` crate (at the workspace root, excluded from the
default workspace because it targets `wasm32-unknown-unknown`) provides a
Rust-compiled-to-wasm decoder/encoder so JS / TS apps can consume and emit
frames without maintaining hand-written schemas. The `wingfoil-js` npm
package wraps that wasm module and provides reactive-framework adapters
(Solid.js, Svelte).
