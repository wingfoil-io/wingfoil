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
| `WebServer::bind(addr) -> WebServerBuilder` | handle | then `.codec(..)` / `.delivery(..)` / `.lossless_stall_timeout(..)` / `.serve_static(dir)` / `.tls(cert, key)` |
| `WebServerBuilder::start()` | handle | binds a real port |
| `WebServerBuilder::start_historical()` | handle | **binds nothing**; `web_pub`/`web_sub` become no-ops |
| `server.port()` / `codec()` / `delivery()` / `lossless_stall_timeout()` / `is_tls()` / `is_historical_noop()` / `stop()` | handle | |
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
    Delivery is **lossless** here by default — see the `Delivery` section
    below.
    `web_sub` yields an **empty** source in historical mode — live browser
    input has no place in a deterministic replay, and an open listener would
    block the run waiting for frames that never arrive.
  - `start_historical()` — bind no TCP port at all; both `web_pub` and
    `web_sub` no-op, so the same graph runs unmodified in a server-less
    backtest.

  Note `web_sub` is therefore **not** rejected at wiring, unlike the live `_sub`
  sources of register B2 — it is *finite* under historical replay.
- **A client's `Subscribe` takes effect asynchronously, and there is no ack.**
  The connection's reader task registers the connection on the topic when it
  processes the frame; until then the publisher's fan-out does not include it,
  and a frame published in that window is dropped rather than queued. So
  anything published between a client sending `Subscribe` and the server acting
  on it is lost. Publishers that run for a while don't notice; a short, finite
  publish can vanish entirely. `WebServer::subscriber_count(topic)` is the
  observable — wait for it to reach the expected count before publishing, which
  is what `tests/web_integration.rs`'s `wait_for_subscribers` does. This was a live
  test flake, not a hypothetical.
- **`Delivery` decides whether a client can back-pressure the graph, and the
  two run modes want opposite answers** (`Delivery { Auto, Lossy, Lossless }`
  on the builder, default `Auto`; #437).
  - **Real time is lossy, and that is not negotiable.** A client that falls
    behind is already showing stale data, and the only alternative to dropping
    is stalling the graph — so a frozen browser tab would back-pressure a
    trading system. `Auto` resolves to lossy here: the publisher `try_send`s
    into each subscribed connection's outbound queue and drops the frame for
    anyone whose queue is full. Nothing a client does can make it wait.
  - **Historical is lossless.** A replay has no live clock to fall behind, so
    dropping does not keep up with anything — it corrupts the replay the
    browser is drawing. A probe on #437 delivered 4586 of 5000 frames to one
    loopback subscriber. `Auto` resolves to lossless: the publisher *awaits* a
    slot in each subscribed connection's outbound queue instead of dropping.
    That stall reaches the graph through the **bounded sink channel** between
    the graph thread and the publish consumer (`PUBLISH_INFLIGHT_INSTANTS` in
    `write.rs`, passed as `consume_async_bursts`' `buffer_size`): a stalled
    consumer stops draining it, it fills, and `spawn_sink`'s `send_blocking`
    parks the graph thread. **There is no separate pacing mechanism** — the
    channel bound is it. An earlier cut of #437 added a semaphore alongside an
    unbounded channel; if you find yourself reaching for one again, bound the
    channel instead.
  - **Zero subscribers never waits** — same hang class as the `web_sub`
    historical hang, and the reason `deliver_to` returns immediately on an
    empty target list. **Several subscribers pace to the slowest**: the waits
    are issued **concurrently** (`join_all`), so an instant costs the slowest
    subscriber rather than the sum of them, and one wedged client costs one
    stall-timeout window in total rather than one per client. Being stalled by a genuinely slow client is the contract, and
    it is why lossless is not the real-time default.
  - **"Slowest" is bounded, or it would include "gone".** A half-open peer — a
    dead machine, a partitioned network, a closed laptop lid — never closes its
    socket, so an unbounded await parks the graph until TCP keepalive notices,
    hours later; and since the end-of-run `Complete` rides the same path, `run()`
    itself would hang after the last cycle. `lossless_stall_timeout` (default
    30 s, settable on the builder) is the bound: a subscriber whose queue is
    full and accepts *nothing* for that long is withdrawn, with a `log::warn!`.
    It cannot catch a merely slow client — the wait is for one slot in a
    1024-deep queue, so anything draining at all resets it. It is settable
    mostly so the tests can prove it in 200-300 ms rather than 30 s; an
    untestable timeout is a bad timeout.
  - **Withdrawing a stalled subscriber closes its whole connection**, and that
    is load-bearing rather than tidy. Removing it from the registry alone would
    leave the socket open and silent — no further data frames, and no
    end-of-run `Complete` either, because `finally` snapshots a registry the
    client is no longer in. `@wingfoil/client` keys *both* `onComplete` and its
    stop-reconnecting logic on that marker, so such a client would neither
    complete nor retry; it would just wait. The clients that reach that state
    are precisely the **recoverable** ones (a laptop that suspends, trips the
    bound, wakes and drains) — a genuinely dead peer notices nothing either
    way. So `deliver_to` raises the connection's `Notify`, the reader
    loop selects on it, and teardown **aborts** the writer rather than awaiting
    its drain (awaiting would park forever on the same wedged socket and never
    drop the `WebSocket`, which is the thing being fixed). Closing the whole
    connection rather than the one subscription is right because every topic on
    a connection shares one outbound queue, so a stall is connection-level.
  - **There is one fan-out registry, not one per policy.** `pub_topics` maps a
    topic to the outbound queue of every connection subscribed to it, and
    `Delivery` chooses only how a frame is *offered* to those queues —
    `try_send`-and-drop under lossy, `send_timeout` under lossless. So
    `subscriber_count` is a single source of truth that means the same thing in
    either mode, there is no per-subscription relay task, and a subscription
    cannot be half-registered. An earlier cut of #437 ran a `broadcast` channel
    and a lossless registry side by side, which needed a load-bearing rule
    about which order to register them in so the count stayed honest; the rule
    went away with the second registry. **Don't reintroduce one.**
  - **`Auto` is resolved at graph `start`, not at `bind`, and the answer is
    stored per publish, not per server.** The run mode does not exist when the
    server is built — `web_pub` composes a `compose_spawn_at_start` hook onto
    its sink node that resolves it, before the first cycle, so nothing is ever
    published against an unresolved policy. The resolved flag then lives in an
    `Arc<AtomicBool>` created per publish inside `publish_frames`, **not** on
    `WebServerInner`: one `WebServer` can serve several graphs, and two running
    at once in opposite run modes would otherwise overwrite each other's
    policy — flipping a live graph into pacing on browser sockets, or reverting
    an in-flight replay to lossy.
  - **The publisher snapshots its subscribers once per instant**, not once per
    frame. Lossless is the path that sets a replay's throughput ceiling, so a
    per-frame `pub_topics` lock and `Vec` clone would be a mutex acquisition
    and an allocation on every frame. The set can only usefully change between
    instants; a subscription that goes away mid-instant surfaces as a send
    failure, which withdraws it from the snapshot too.
  - Changing any of this changes the contract; `Delivery::Lossy` is the opt-out
    that restores legacy behaviour in both modes.
- **The fan-out registry is behind a mutex**, so it must not be
  touched from a cycle. The envelope is encoded and delivered **inside the
  `consume_async_bursts` consumer** (as legacy did, one instant at a time
  rather than one value at a time so the subscriber snapshot is taken once per
  instant); the graph thread only clones the `(time, value)` pair into the sink
  channel.
- **`Complete` is emitted from the sink's teardown.** `web_pub` chains its own
  `finally` that flushes every queued frame, joins the consumer, and *then*
  sends `Complete { topic }` — so the marker arrives strictly after the last
  data frame, on the topic's own channel, for both a finite `RunFor` and the
  end of a historical replay. It goes out by whichever `Delivery` path the run
  resolved to; on the lossless one that is a `block_on` from the graph thread,
  which is safe for the same reason `flush` is.
- `consume_async_bursts` ⇒ the `block_on` footgun (A5a): build, run and drop the graph
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

**The `Delivery` probes are in tier 2 and are fussy about their own sizing.**
`historical_streaming_is_lossless_for_a_fast_graph` only tests anything if the
lossy path would actually have dropped, and a loopback client that merely
decodes keeps pace with a CPU-speed replay frame for frame. Two things make it a
real reproduction, and both are load-bearing: the client sits on the socket
without reading for 750 ms (the frozen-tab case), and frames are ~2 KiB. At a
few dozen bytes a frame the buffers in the path — the 1024-slot outbound
queue and the kernel's auto-tuned loopback socket buffer —
swallow the whole replay and nothing is ever dropped. If you shrink either
number, check the probe still fails against `Delivery::Lossy` before trusting
it.

The same rule caught both of the tests added for review findings, and both were
checked the same way: `lossless_withdraws_a_subscriber_that_stops_accepting_frames`
fails (at 27 s) when `lossless_stall_timeout` is raised to 25 s, and
`concurrent_runs_on_one_server_resolve_delivery_independently` fails when the
per-publish policy flag is made process-wide. The second one can only *miss* —
if the live run happens to finish before the replay resolves its policy there is
no overlap — so it never flakes red, but do not read a pass as proof the two
runs overlapped. `a_withdrawn_subscriber_gets_its_connection_closed` was checked
the same way: drop the `close.notify_one()` and it fails on "must see its
connection close".

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
cargo test -p wingfoil --features web --test web_adapter
# tier 2 — the socket suite, plain + TLS
cargo test -p wingfoil \
    --features web-tls-integration-test --test web_integration -- --test-threads=1
```

**Workflow:** `.github/workflows/web-integration.yml` (in
`integration-tests.yml`) runs
`cargo test --features web-tls-integration-test -p wingfoil` plus a
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
  `WebServer(addr, …)`, `.port()`, `.codec_name()`, `.delivery_name()`,
  `.sub(graph, topic)`,
  `.pub(stream, topic)`, `.pub_bursts(stream, topic)`, `.stop()`. `delivery=`
  takes the same three strings as the Rust enum (`"auto"` / `"lossy"` /
  `"lossless"`), and like `codec` it is a string rather than a `#[pyclass]`
  enum. It is the
  **first handle class that wires a source**, which is why `sub` takes the
  `Graph` explicitly (`web_sub` needs a builder); contrast prometheus's
  exporter, which takes no `Graph` at all.
- Payload edge: Python values marshal through `serde_json::Value`, serialized
  with whichever codec the server was built with. **That is not the same as
  being wire-compatible with a Rust peer, and the docs used to claim it was.**
  `Value` carries no schema, so the codec decides what is possible:
  - `sub` **rejects `bincode`** at wiring. It decodes into `Value`, whose
    `Deserialize` calls `deserialize_any`, and bincode refuses that for every
    value of every shape — a Python subscription could not read a frame from
    any peer. The rejection covers the `historical=True` no-op server too, so a
    backtest cannot pass where the identical live graph would abort.
  - `pub` **keeps `bincode`**: it is peer-dependent, not impossible. Scalars
    and same-width sequences reach a typed Rust peer byte-for-byte; a `dict`
    against a `struct` is the #821 silent-garbage case, while a `dict` against
    a `HashMap` is fine. The peer's type is not observable from the adapter, so
    rejecting outright would break correct configurations — hence docs, and no
    constructor-time warning that could not tell the two apart.
- `bytes` become a JSON **array of ints** (as legacy did), wire-compatible with
  a Rust `Vec<u8>` peer **under JSON only** — `Value` writes each element as a
  `u64`, so the bincode encoding does not match. A subscription decodes such a
  frame back to a `list` of ints, not `bytes` — deliberately asymmetric,
  because nothing on the wire distinguishes them.
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
cargo test -p wingfoil --features web-tls-integration-test
```
