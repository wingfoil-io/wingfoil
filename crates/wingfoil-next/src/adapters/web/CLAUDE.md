# web Adapter (wingfoil-next)

Bidirectional streaming between a graph and one or more browsers over
WebSocket: an axum HTTP/WS server plus a publish sink and a browser-input
source. Ports legacy `wingfoil::adapters::web` onto the Op model.

**The wire protocol is engine-agnostic and byte-identical to legacy** — the
shared `wingfoil-wire-types` crate is reused as-is, which is exactly why
`wingfoil-wasm` and `@wingfoil/client` (`js/`) need no changes. Treat
that as a hard constraint: a wire change here breaks two other packages.

## Layout

```
adapters/
  web/
    mod.rs         # module docs + re-exports
    server.rs      # WebServer / WebServerBuilder (bind, codec, serve_static, tls, start*)
    read.rs        # web_sub
    write.rs       # WebSinkOps / WebBurstSinkOps
    codec.rs       # re-export of wingfoil-wire-types + wire round-trip tests
    CLAUDE.md      # this file
```

## Feature gating

```toml
web     = ["async", "dep:serde", "dep:axum", "dep:tower-http", "dep:tokio-tungstenite",
           "dep:wingfoil-wire-types", "dep:bincode", "dep:async-stream", "tokio/net", "tokio/sync"]
web-tls = ["web", "dep:axum-server", "dep:rustls", "dep:rustls-pemfile"]
web-tls-integration-test = ["web-tls", "tokio-tungstenite/rustls-tls-webpki-roots",
                            "dep:rcgen", "dep:scopeguard"]
```

`rcgen` generates a fresh self-signed cert at test time (cheaper and more
deterministic than a static fixture that would need rotating); `scopeguard`
removes the PEM files afterwards.

## Entry points

| Item | Kind | Notes |
|---|---|---|
| `WebServer::bind(addr) -> WebServerBuilder` | handle | then `.codec(..)` / `.serve_static(dir)` / `.tls(cert, key)` |
| `WebServerBuilder::start()` | handle | binds a real port |
| `WebServerBuilder::start_historical()` | handle | **binds nothing**; `web_pub`/`web_sub` become no-ops |
| `server.port()` / `codec()` / `is_tls()` / `is_historical_noop()` / `stop()` | handle | |
| `web_sub::<T>(g, &server, topic)` | source | `Result<Stream<Burst<T>>>` |
| `WebSinkOps::web_pub(&server, topic)` | sink trait on `Stream<T>` | one scalar payload per frame |
| `WebBurstSinkOps::web_pub_bursts(&server, topic)` | sink trait on `Stream<Burst<T>>` | the whole same-instant group as one array frame |

## What to know before changing it

- **The server runs on its own thread and its own tokio runtime**, independent
  of the graph's (the same pattern as the prometheus exporter).
- **Wire format:** `bincode` by default, `CodecKind::Json` for a
  devtools-readable mode. `Envelope` / `ControlMessage` live in
  `wingfoil-wire-types` — one source of truth for the server, the WASM client
  and `@wingfoil/client`. The control topic `"$ctrl"` carries
  `Hello { codec, version }` (server → client on upgrade),
  `Subscribe`/`Unsubscribe { topics }` (client → server) and
  `Complete { topic }` (server → client at end-of-stream).
  `codec.rs`'s `control_message_existing_variants_keep_wire_layout` test is the
  guard against accidentally reordering a variant.
- **Two historical strategies, both supported:**
  - `start()` — stream the replay to browsers. `web_pub` publishes historical
    values exactly as in real time (this is what powers browser-side
    visualisation of a backtest), and subscribed clients get a
    `ControlMessage::Complete` at the end (surfaced as `onComplete`).
    `web_sub` yields an **empty** source in historical mode — live browser
    input has no place in a deterministic replay, and an open listener would
    block the run waiting for frames that never arrive.
  - `start_historical()` — bind no TCP port at all; both `web_pub` and
    `web_sub` no-op, so the same graph runs unmodified in a server-less
    backtest.

  Note `web_sub` is therefore **not** rejected at wiring, unlike the live `_sub`
  sources of register B2 — it is *finite* under historical replay.
- **Clients never back-pressure the graph.** The broadcast buffer is lossy: a
  client that cannot keep up drops frames. For a faithful, loss-free replay,
  keep the graph from outrunning the client (e.g. a genuinely compute-bound
  historical run). Do not add back-pressure without a decision — it changes the
  contract.
- **`tokio::sync::broadcast::send` takes an internal lock**, so it must not be
  touched from a cycle. The envelope is encoded and broadcast **inside the
  `consume_async` consumer** (as legacy did); the graph thread only clones the
  `(time, value)` pair into the sink channel.
- **`Complete` is emitted from the sink's teardown.** `web_pub` chains its own
  `finally` that flushes every queued frame, joins the consumer, and *then*
  broadcasts `Complete { topic }` — so the marker arrives strictly after the
  last data frame, on the same broadcast channel, for both a finite `RunFor`
  and the end of a historical replay.
- `consume_async` ⇒ the `block_on` footgun (A5a): build, run and drop the graph
  from a **non-async** thread. The server's own runtime is unaffected.
- **TLS** (`web-tls`): `.tls(cert, key)` with PEM files on disk; crypto
  provider is `ring`, matching [`fix`](../fix/CLAUDE.md). `wingfoil-js` honours
  `location.protocol`, so the only client-side change is loading the page over
  HTTPS.

## Deviations from legacy

Canonical list: the `# Deviations from legacy` block in `web/mod.rs` — five
items: the source takes a `GraphBuilder` and returns `Result` (and is *not*
historical-rejected); the sink is a trait only (D1); a **burst overload**
(`web_pub_bursts`) is added — legacy could only publish an atomic same-instant
array by mapping `Burst<T>` to `Vec<T>` by hand, since `Burst`/`TinyVec` is not
`Serialize` and so cannot be a second impl of the same trait, and the frames are
byte-identical either way; `Complete` comes from the sink's teardown rather
than a consumer noticing its source ended; and the envelope is encoded off the
graph thread. Every legacy capability is preserved and the **wire format is
byte-identical**.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/web_adapter.rs` | `#![cfg(feature = "web")]` | nothing (loopback WS) |
| — the TLS round trip inside it | `#[cfg(feature = "web-tls-integration-test")]` | nothing external; `rcgen` makes the cert |

There is **no separate `web_integration.rs`** — the adapter *is* the server, so
the round trips need no service and live in the one file, with only the TLS
case behind the extra feature (it needs `tokio-tungstenite`'s rustls client and
the cert fixture).

```bash
cargo test -p wingfoil-next --features web --test web_adapter
cargo test -p wingfoil-next --features web-tls-integration-test --test web_adapter
```

**Workflow:** `.github/workflows/web-next-integration.yml` (in
`integration-tests.yml`) runs
`cargo test --features web-tls-integration-test -p wingfoil-next` plus a
`pytest -m requires_web` Python leg.

## Example

`examples/web/main.rs` → example `web_adapter`, `required-features = ["web"]`
(a directory with a README, port of legacy's). Streams a synthetic mid price
to the browser and logs UI events back.

## Python

`wingfoil-next-python` feature
`web = ["wingfoil-next/web-tls", "dep:serde_json", "_common"]` — note it turns
on **`web-tls`** so the constructor's `cert_path`/`key_path` always exist
rather than appearing and disappearing with a feature; rustls is pure Rust, so
it costs the wheel only build time. `serde_json` is named directly because the
payload edge marshals through `serde_json::Value`. **In `all-adapters` and in
the wheel.**

- **Hand-written, not `#[pyadapter]`**: `WebServer` is a stateful handle with a
  lifecycle. One class, no free functions —
  `WebServer(addr, …)`, `.port()`, `.codec_name()`, `.sub(graph, topic)`,
  `.pub(stream, topic)`, `.pub_bursts(stream, topic)`, `.stop()`. It is the
  **first handle class that wires a source**, which is why `sub` takes the
  `Graph` explicitly (`web_sub` needs a builder); contrast prometheus's
  exporter, which takes no `Graph` at all.
- Payload edge: Python values marshal through `serde_json::Value`, serialized
  with whichever codec the server was built with — so a Python publisher is
  wire-compatible with a Rust one. `bytes` become a JSON **array of ints** (as
  legacy did, for wire compatibility with a Rust `Vec<u8>` peer), and a
  subscription decodes such a frame back to a `list` of ints, not `bytes` —
  deliberately asymmetric, because nothing on the wire distinguishes them.
- `sub` is burst-shaped: each tick yields a Python `list` of the frames that
  arrived between cycles.
- Tests: `tests/test_web.py` — service-free group by default,
  `@pytest.mark.requires_web` group (needs the `websockets` package) in the
  workflow above.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test -p wingfoil-next --features web-tls-integration-test
```
