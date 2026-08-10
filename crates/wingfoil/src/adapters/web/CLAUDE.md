# web Adapter (wingfoil)

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
web-integration-test = ["web"]          # gates tests/web_integration.rs; no extra deps
web-tls-integration-test = ["web-tls", "web-integration-test",
                            "tokio-tungstenite/rustls-tls-webpki-roots",
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
| `server.subscriber_count(topic)` | handle | receivers a publish would reach *now*; 0 → 1 when the server has acted on a client's `Subscribe` |
| `web_sub::<T>(g, &server, topic)` | source | `Result<Stream<Burst<T>>>` |
| `WebSinkOps::web_pub(&server, topic)` | sink trait on `Stream<T>` | one scalar payload per frame |
| `WebBurstSinkOps::web_pub_bursts(&server, topic)` | sink trait on `Stream<Burst<T>>` | the whole same-instant group as one array frame |
| `WebBurstSinkOps::web_pub_each(&server, topic)` | sink trait on `Stream<Burst<T>>` | one frame per value, byte-identical to `web_pub` — lets a pipeline stay burst-shaped without a client change |

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
- **A client's `Subscribe` takes effect asynchronously, and there is no ack.**
  The connection's reader task calls `broadcast::Sender::subscribe()` when it
  processes the frame; until then the client has no receiver, and
  `broadcast` *drops* rather than queues for a receiver that does not exist. So
  anything published between a client sending `Subscribe` and the server acting
  on it is lost. Publishers that run for a while don't notice; a short, finite
  publish can vanish entirely. `WebServer::subscriber_count(topic)` is the
  observable — wait for it to reach the expected count before publishing, which
  is what `tests/web_integration.rs`'s `wait_for_subscribers` does. This was a live
  test flake, not a hypothetical.
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
historical-rejected); the sink is a trait only (D1); **two burst
overloads** are added (`web_pub_bursts`, one atomic array frame — legacy could
only get that by mapping `Burst<T>` to `Vec<T>` by hand; and `web_pub_each`,
one frame per value, which legacy has no equivalent for and which exists so a
pipeline can avoid `collapse`'s silent data loss without changing the wire
format). Both live on a separate trait because `Burst`/`TinyVec` is not
`Serialize` *and* `WebSinkOps` is not generic over its payload, so a second impl
of the same trait would collide on coherence; frames are byte-identical to
legacy either way; `Complete` comes from the sink's teardown rather
than a consumer noticing its source ended; and the envelope is encoded off the
graph thread. Every legacy capability is preserved and the **wire format is
byte-identical**.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/web_adapter.rs` (tier 1) | `#![cfg(feature = "web")]` | nothing listening — bind, historical contracts, TLS-config error |
| `tests/web_integration.rs` (tier 2) | `#![cfg(feature = "web-integration-test")]` | loopback WS clients; no external service |
| — the TLS round trip inside it | `#[cfg(feature = "web-tls-integration-test")]` | nothing external; `rcgen` makes the cert |

**The split is about speed and load-sensitivity, not about needing a service.**
The adapter *is* the server, so nothing external is required either way — but
the round trips bind sockets, spawn client threads and wait on frames with
multi-second deadlines, and CI's `test` job filters on
`not binary(/_integration$/)`. While they lived in `web_adapter.rs` they ran in
that job too (under `--all-features`, which turns on
`web-tls-integration-test`), on top of running here — which is what made the
web tests a repeat source of timeouts and flakes. The **filename suffix** is
the whole mechanism; a socket test added to `web_adapter.rs` silently rejoins
the fast job.

`web-tls-integration-test` implies `web-integration-test`, so the one flag runs
everything.

```bash
# tier 1 — fast, runs in CI's ordinary `test` job
cargo test --manifest-path crates/wingfoil/Cargo.toml --features web --test web_adapter
# tier 2 — the socket suite, plain + TLS
cargo test --manifest-path crates/wingfoil/Cargo.toml \
    --features web-tls-integration-test --test web_integration -- --test-threads=1
```

**Workflow:** `.github/workflows/web-integration.yml` (in
`integration-tests.yml`) runs
`cargo test --features web-tls-integration-test --manifest-path crates/wingfoil/Cargo.toml` plus a
`pytest -m requires_web` Python leg.

The same workflow carries the **browser half** of this adapter — the
`wingfoil-wasm-build` and `wingfoil-js-typecheck` jobs, which build
`crates/wingfoil-wasm` (the WASM codec) and `js/` (`@wingfoil/client`). They
share a trigger with the server jobs above because both sides speak the
`wingfoil-wire-types` contract: a change to the wire types has to build both or
nothing catches a mismatch. A `js/**` or `crates/wingfoil-wasm/**` change is
therefore enough to run this whole workflow.

## Example

`examples/web/main.rs` → example `web_adapter`, `required-features = ["web"]`
(a directory with a README, port of legacy's). Streams a synthetic mid price
to the browser and logs UI events back.

## Python

`wingfoil-python` feature
`web = ["wingfoil/web-tls", "dep:serde_json", "_common"]` — note it turns
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
cargo test --manifest-path crates/wingfoil/Cargo.toml --features web-tls-integration-test
```
