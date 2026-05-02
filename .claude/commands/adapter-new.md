---
description: Implement a wingfoil I/O adapter end-to-end (TDD branch + tests + CI). Use after /adapter-new-issue has scoped it.
argument-hint: <adapter-name>
---

Implement a new I/O adapter for wingfoil named `$ARGUMENTS`. Follow these steps in order. Work test-driven: write each test before its implementation.

## Invariants

These rules apply to every step below.

### No locks on the graph execution path

`Mutex` / `RwLock` must not be locked inside `cycle()`, `setup()`, or any other
method the graph engine calls per-tick. Graph execution is single-threaded on
the graph thread; taking a lock there adds uncontended-lock overhead on every
tick and risks blocking the hot path behind a background thread (HTTP scraper,
async task, reconnect loop, etc.).

Locks *are* acceptable in, and only in:

- **Wiring / factory functions** — e.g. `$ARGUMENTS_pub(...)`, `$ARGUMENTS_sub(...)`,
  `WebServer::bind(...)`, `PrometheusExporter::register(...)`. Runs once before
  the graph starts.
- **`start()` / `stop()` / `teardown()`** — once-per-run lifecycle hooks.
- **Background threads** — the OS thread spawned by `ReceiverStream`, the dedicated
  HTTP/tokio thread behind `WebServer` / `PrometheusExporter`, a FIX session thread,
  etc. These are *not* the graph thread.
- **Cross-thread handles handed to user code** — e.g. `FixInjector::inject()` called
  from outside the graph.

To communicate between a background thread and graph `cycle()`, use the existing
channel primitives (`ChannelSender` / `ChannelReceiver`, `ReceiverStream`,
`produce_async` / `consume_async`) rather than a shared `Mutex<...>`. Channels
give single-producer / single-consumer lock-free hand-off and preserve the
"cycle never blocks" invariant.

When the payload is a whole value that a background thread reads ad-hoc (not a
stream of deltas), `arc_swap::ArcSwap<T>` gives a lock-free atomic pointer swap
in `cycle()` that the reader can `.load()` off-thread — see
`wingfoil/src/adapters/prometheus/exporter.rs` for the per-slot pattern.

## 1. Branch

```bash
git checkout main && git pull origin main && git checkout -b $ARGUMENTS
```

## 2. Feature flags — `wingfoil/Cargo.toml`

Add two feature flags:
```toml
[features]
$ARGUMENTS = ["dep:some-client-crate", "async"]
$ARGUMENTS-integration-test = ["$ARGUMENTS", "dep:testcontainers"]

[dependencies]
some-client-crate = { version = "x.y", optional = true }
testcontainers = { version = "0.27", features = ["blocking"], optional = true }
```

Note: `testcontainers` must go in `[dependencies]` as optional (not `[dev-dependencies]`) because Cargo feature flags cannot gate dev-deps. Only add `testcontainers-modules` if a module for the service actually exists in that crate — otherwise use `GenericImage` directly (see step 4).

**Multiple I/O libraries via feature flags:** if the adapter can use different underlying
libraries for the same functionality (e.g. a discovery backend, a TLS provider, or an
alternative codec), gate each library behind its own feature flag and use `#[cfg(feature = "...")]`
to switch implementations:

```toml
[features]
$ARGUMENTS = ["dep:primary-client"]
$ARGUMENTS-alt-backend = ["$ARGUMENTS", "dep:alt-backend-crate"]

[dependencies]
primary-client    = { version = "x.y", optional = true }
alt-backend-crate = { version = "x.y", optional = true }
```

Then in code, provide a trait for the pluggable concern and gate the concrete impl:

```rust
pub trait <Name>Backend: Send + 'static { /* ... */ }

#[cfg(feature = "$ARGUMENTS-alt-backend")]
impl <Name>Backend for AltBackend { /* ... */ }
```

This pattern is used by the ZMQ adapter: `zmq` works standalone for direct TCP addresses, but
when the `etcd` feature is also enabled, `EtcdRegistry` becomes available as a `ZmqRegistry`
backend for service discovery. The FIX adapter similarly declares `fefix` as an optional
dependency reserved for future dictionary-driven validation alongside the hand-rolled codec.

## 3. Module registration — `wingfoil/src/adapters/mod.rs`

```rust
#[cfg(feature = "$ARGUMENTS")]
pub mod $ARGUMENTS;
```

## 4. Docker image / container setup

Choose the test infrastructure that fits the service:

### Option A — testcontainers (preferred for open-source services)

Use `SyncRunner` (blocking) so container startup stays in a plain `#[test]` function without a wrapping async runtime:

```rust
// In integration_tests.rs
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};

// If testcontainers-modules has a module for this service:
use testcontainers_modules::some_service::SomeService;
let container = SomeService::default().start()?;
let port = container.get_host_port_ipv4(DEFAULT_PORT)?;

// Otherwise use GenericImage (most common):
let container = GenericImage::new("vendor/image", "tag")
    .with_wait_for(WaitFor::message_on_stderr("ready to serve"))
    .with_env_var("KEY", "value")
    .start()?;
let port = container.get_host_port_ipv4(DEFAULT_PORT)?;
let endpoint = format!("http://127.0.0.1:{port}");
```

The container is stopped automatically when dropped. Hold the container in a binding for the duration of the test (`let _container = ...`). No `docker-compose.yml` needed.

### Option B — external service with skip-if-unavailable

When the service requires a commercial license (e.g. KDB+), cannot run in a container,
or connects to a remote endpoint (e.g. FIX to an exchange), skip tests if the service is
not reachable. Use a plain Docker command or manual setup documented in the adapter's
`CLAUDE.md` and example `README.md`:

```rust
fn service_available() -> bool {
    std::net::TcpStream::connect("localhost:PORT").is_ok()
}

#[test]
fn test_round_trip() -> anyhow::Result<()> {
    if !service_available() {
        eprintln!("skipping: service not running on localhost:PORT");
        return Ok(());
    }
    // ...
}
```

### Option C — no external service needed

File-based adapters (e.g. CSV) or IPC adapters (e.g. iceoryx2) may not need any
container or external service. Unit tests with fixture files or in-process communication
are sufficient. In this case, skip the integration test feature flag and put tests
directly in the module's `#[cfg(test)]` blocks.

## 5. File structure

The default layout for bidirectional pub/sub adapters:

```
wingfoil/src/adapters/$ARGUMENTS/
  mod.rs               # Connection config, public types, re-exports
  read.rs              # sub/read function (producer)
  write.rs             # pub/write function (consumer)
  integration_tests.rs # gated by $ARGUMENTS-integration-test feature
  CLAUDE.md            # documents design decisions and pre-commit requirements
```

**Variations for non-standard adapters:** name files after their function when the
`read.rs`/`write.rs` split doesn't fit:

- **Push-only adapters** (e.g. OTLP): use `push.rs` instead of `write.rs`; omit `read.rs`
- **Pull-based exporters** (e.g. Prometheus): use `exporter.rs`; omit `read.rs`/`write.rs`
- **Stateful bidirectional sessions** (e.g. FIX): keep all logic in `mod.rs` when
  read/write share session state that is hard to split cleanly
- **File-based adapters** (e.g. CSV): omit `integration_tests.rs` when unit tests with
  fixture files provide sufficient coverage

## 6. Types and module doc — `mod.rs`

All types used on-graph must satisfy `Element = Debug + Clone + Default + 'static` and be `Send`.

```rust
pub struct <Name>Connection { /* endpoint, credentials, etc. */ }

// Value type for the pub (consumer) input — name this after the domain concept,
// e.g. EtcdEntry, KafkaMessage, RedisCommand
#[derive(Debug, Clone, Default)]
pub struct <Name>Entry { /* fields appropriate to the service */ }

// Event type for the sub (producer) output — include a Default variant
#[derive(Debug, Clone, Default)]
pub struct <Name>Event { /* fields */ }
```

**Naming conventions:** the default verbs are `_sub` (producer) and `_pub` (consumer), but
adapters should use the verb that best fits the domain:

| Pattern | Verbs | Examples |
|---------|-------|----------|
| Pub/sub or event streaming | `_sub` / `_pub` | etcd, zmq, iceoryx2 |
| Batch/file I/O | `_read` / `_write` | kdb, csv |
| Session/connection | `_connect` / `_accept` | fix |
| Push-only telemetry | `_push` (trait method) | otlp |
| Pull-based exporter | `.register()` (method) | prometheus |

Choose the verb that reads naturally at the call site. Generic transports (zmq, iceoryx2)
typically use generic `T` instead of concrete Entry/Event types — this is fine when the
adapter is protocol-agnostic. Similarly, adapters that use a config struct (e.g. `OtlpConfig`,
`Iceoryx2SubOpts`) instead of a `<Name>Connection` are fine when the domain doesn't map
cleanly to "connect to endpoint".

Add `//!` module-level doc at the top of `mod.rs` covering:

- One-line description of what the adapter does
- Setup: local Docker one-liner to start the service (omit if N/A, e.g. file-based or IPC)
- Producer section with minimal `ignore` code block
- Consumer section with minimal `ignore` code block (omit if adapter is single-direction)
- Any feature-specific sections (leases, conditional writes, etc.)

```rust
//! $ARGUMENTS adapter — <one-line description>.
//!
//! Provides two graph nodes:
//! - [`$ARGUMENTS_sub`] — producer that ...
//! - [`$ARGUMENTS_pub`] — consumer that ...
//!
//! # Setup
//!
//! ```sh
//! docker run --rm -p PORT:PORT <image>:<tag>
//! ```
//!
//! # Subscribing
//! ```ignore
//! let conn = <Name>Connection::new("http://localhost:PORT");
//! $ARGUMENTS_sub(conn, "prefix")
//!     .collapse()
//!     .for_each(|event, _| println!("{:?}", event))
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```
//!
//! # Publishing
//! ```ignore
//! constant(burst![<Name>Entry { key: "k".into(), value: b"v".to_vec() }])
//!     .$ARGUMENTS_pub(conn)
//!     .run(RunMode::RealTime, RunFor::Cycles(1))
//!     .unwrap();
//! ```
```

## 7. Sub method — `read.rs` (producer)

Choose the threading model based on the I/O library:

- **`produce_async`** — when the I/O library is async (e.g. tokio-based clients like
  `etcd-client`, `rdkafka`). Returns `Rc<dyn Stream<Burst<Event>>>`.
- **`ReceiverStream`** — when the I/O library is synchronous and poll-based (e.g. `zmq`,
  `iceoryx2`). Dedicates a real OS thread per subscriber to marshall incoming messages
  into the graph via a channel. Use this to avoid wrapping every blocking call in
  `spawn_blocking`.
- **Spin loop (`MutableNode` + `always_callback`)** — when the I/O library is synchronous,
  non-blocking, and ultra-low latency is required (e.g. `iceoryx2` in spin mode, FIX with
  `AlwaysSpin`). The node polls directly inside `cycle()` with no background thread. Lowest
  latency (~1–5 µs) but highest CPU usage on the graph thread.

An adapter may support **multiple strategies** selected at construction time via a mode enum
(see "Multiple polling strategies" below). In that case the `start()` / `cycle()` paths branch
on the chosen mode, but the public API (`Rc<dyn Stream<Burst<T>>>`) is identical regardless.

### produce_async pattern (async I/O)

```rust
#[must_use]
pub fn $ARGUMENTS_sub(conn: impl Into<<Name>Connection>, /* params */) -> Rc<dyn Stream<Burst<<Name>Event>>> {
    produce_async(move |_ctx: RunParams| async move {
        Ok(async_stream::stream! {
            // connect, snapshot, then live stream
            // yield Ok((NanoTime::now(), event))
            // yield Err(anyhow::anyhow!("...")) on fatal error
        })
    })
}
```

### ReceiverStream pattern (synchronous / poll-based I/O)

```rust
pub fn $ARGUMENTS_sub<T: Element + Send>(address: &str) -> Rc<dyn Stream<Burst<T>>> {
    let subscriber = Subscriber::new(address.to_string());
    ReceiverStream::new(
        move |channel_sender, stop_flag| subscriber.run(channel_sender, stop_flag),
        true, // real-time only
    )
    .into_stream()
}
```

The callback runs on a dedicated OS thread. Use `stop_flag: Arc<AtomicBool>` to
cooperatively shut down, and `channel_sender: ChannelSender<T>` to push
`Message::RealtimeValue(v)`, `Message::EndOfStream`, or `Message::Error(e)`.

### Spin loop pattern (non-blocking direct poll)

When ultra-low latency matters and the I/O supports non-blocking reads, poll directly inside
`cycle()` with no background thread. The graph engine calls `cycle()` on every tick because
`start()` opts in via `state.always_callback()`.

```rust
struct <Name>SubNode {
    socket: Option<TcpStream>,  // or any non-blocking I/O handle
    value: Burst<<Name>Event>,
    parse_buf: Vec<u8>,
    mode: <Name>PollMode,
}

impl MutableNode for <Name>SubNode {
    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        if state.run_mode() != RunMode::RealTime {
            anyhow::bail!("$ARGUMENTS spin mode only supports real-time");
        }
        state.always_callback(); // tell graph: call cycle() every tick, no sleep
        let mut sock = TcpStream::connect(&self.address)?;
        sock.set_nonblocking(true)?;
        self.socket = Some(sock);
        Ok(())
    }

    fn cycle(&mut self, _: &mut GraphState) -> anyhow::Result<bool> {
        self.value.clear();
        let Some(sock) = self.socket.as_mut() else { return Ok(false) };
        let mut tmp = [0u8; 4096];
        loop {
            match sock.read(&mut tmp) {
                Ok(0) => { /* EOF — emit disconnect event */ break; }
                Ok(n) => self.parse_buf.extend_from_slice(&tmp[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break, // nothing to read
                Err(e) => return Err(e.into()),
            }
        }
        // parse self.parse_buf into events, push to self.value
        Ok(!self.value.is_empty())
    }

    fn upstreams(&self) -> UpStreams { UpStreams::none() } // source node
}
```

Key points:
- `state.always_callback()` — the graph will busy-loop this node (~1–5 µs per cycle)
- Non-blocking socket: `WouldBlock` means "no data right now", cycle returns immediately
- No background thread, no channel hop — lowest possible latency
- Highest CPU usage on the graph thread — only use when latency justifies the cost
- See the FIX adapter (`FixPollMode::AlwaysSpin`) and iceoryx2 (`Iceoryx2Mode::Spin`) for
  production examples

### Multiple polling strategies

An adapter may offer several polling strategies via a mode enum, letting callers trade latency
for CPU. Define the enum in `mod.rs` and branch on it in `start()` / `cycle()`:

```rust
/// Polling strategy for the $ARGUMENTS subscriber.
#[derive(Debug, Clone, Default)]
pub enum <Name>PollMode {
    /// Non-blocking poll inside graph `cycle()` — lowest latency, highest CPU.
    #[default]
    Spin,
    /// Background thread + channel — higher latency (~10–100 µs), lower CPU.
    Threaded,
}
```

The public factory function accepts the mode and wires up the appropriate internals:

```rust
pub fn $ARGUMENTS_sub(
    conn: impl Into<<Name>Connection>,
    mode: <Name>PollMode,
) -> Rc<dyn Stream<Burst<<Name>Event>>> {
    match mode {
        <Name>PollMode::Spin     => /* return spin-loop MutableNode wrapped in Rc */,
        <Name>PollMode::Threaded => /* return ReceiverStream with background thread */,
    }
}
```

The caller sees the same `Rc<dyn Stream<Burst<T>>>` regardless of mode. Document the
latency/CPU tradeoffs on each variant (see `FixPollMode` and `Iceoryx2Mode` for examples).

**Flexible arguments via `impl Into<T>`:** for any parameter that callers might supply in
multiple forms (a URL string, a config struct, a bare value), accept `impl Into<ConfigType>`
and add `From` impls for each input shape. This keeps a single function signature while
eliminating boilerplate at the call site:

```rust
// Instead of:   fn $ARGUMENTS_sub(conn: <Name>Connection, ...)
// Prefer:        fn $ARGUMENTS_sub(conn: impl Into<<Name>Connection>, ...)
//
// Then add From impls on the config type so callers can pass a bare string,
// a pre-built config, or whatever is natural:
impl From<&str> for <Name>Connection { ... }
impl From<String> for <Name>Connection { ... }
```

**Optional / mode-switching arguments:** if an argument is optional or selects between modes
(e.g. no-op vs. discovery, no cache vs. cache config), model it as a wrapper type with `From`
impls rather than `Option<T>` or multiple overloads:

```rust
pub struct <Name>SubConfig(pub(crate) <Name>SubMode);
pub(crate) enum <Name>SubMode { Direct(String), Discover(String, Box<dyn <Name>Backend>) }

impl From<&str>  for <Name>SubConfig { /* direct address */ }
impl From<String> for <Name>SubConfig { /* direct address */ }
impl<B: <Name>Backend + 'static> From<(&str, B)> for <Name>SubConfig { /* discovery */ }
// same pattern applies for cache, auth, or any other optional config

// Result: one signature, three call-site forms:
$ARGUMENTS_sub("tcp://host:1234")              // direct string
$ARGUMENTS_sub(config)                         // pre-built config
$ARGUMENTS_sub(("service-name", my_backend))   // mode-switching
```

If the service supports a **snapshot + watch** pattern (like etcd), use watch-before-get to avoid races:
1. Open watch/subscribe first
2. Read snapshot, capture its revision/cursor
3. Emit snapshot events
4. Emit watch events, skipping any with revision <= snapshot revision

## 8. Pub method — `write.rs` (consumer)

Choose the threading model to match the I/O library (same decision as step 7):

- **`consume_async`** — when the I/O library is async. Returns `Rc<dyn Node>`.
- **`MutableNode` impl** — when the I/O library is synchronous. Implement `start` / `cycle` /
  `stop` directly on a struct, giving full control over socket lifecycle and buffering.

The same mode-enum pattern from the sub side applies here: if the adapter supports both
spin and threaded polling, the pub node should respect the same `<Name>PollMode` and
branch accordingly (e.g. flush outbound messages via non-blocking write in spin mode, or
push to a channel for a background sender thread in threaded mode).

### consume_async pattern (async I/O)

```rust
#[must_use]
pub fn $ARGUMENTS_pub(conn: <Name>Connection, upstream: &Rc<dyn Stream<Burst<<Name>Entry>>>) -> Rc<dyn Node> {
    upstream.consume_async(Box::new(move |source: Pin<Box<dyn FutStream<Burst<<Name>Entry>>>>| {
        async move {
            // connect once
            // while let Some((_time, burst)) = source.next().await { write each entry }
            Ok(())
        }
    }))
}
```

### MutableNode pattern (synchronous / poll-based I/O)

```rust
struct SenderNode<T: Element + Send> {
    src: Rc<dyn Stream<T>>,
    socket: Option<Socket>,
    // ... buffering state, config, etc.
}

impl<T: Element + Send + Serialize> MutableNode for SenderNode<T> {
    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        // bind socket, register with discovery backend, etc.
    }
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let value = self.src.peek_value();
        // serialize and send
        Ok(true)
    }
    fn stop(&mut self, _: &mut GraphState) -> anyhow::Result<()> {
        // send EndOfStream, revoke registrations, close socket
    }
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.src.clone().as_node()], vec![])
    }
}
```

Apply the same `impl Into<T>` and wrapper-type patterns from step 7. A common case is an
optional registration or side-effect (e.g. register address in a registry, or skip it):

```rust
pub struct <Name>PubConfig(pub(crate) Option<(String, Box<dyn <Name>Backend>)>);

impl From<()> for <Name>PubConfig { fn from(_: ()) -> Self { Self(None) } }
impl<B: <Name>Backend + 'static> From<(&str, B)> for <Name>PubConfig { /* Some(...) */ }

// Callers:
stream.$ARGUMENTS_pub(port, ())                       // no registration
stream.$ARGUMENTS_pub(port, ("service-name", backend)) // with registration
```

Expose a fluent extension trait so callers can chain `.$ARGUMENTS_pub(...)` on streams:

```rust
pub trait <Name>PubOperators {
    #[must_use]
    fn $ARGUMENTS_pub(self: &Rc<Self>, conn: <Name>Connection) -> Rc<dyn Node>;
}

impl <Name>PubOperators for dyn Stream<Burst<<Name>Entry>> {
    fn $ARGUMENTS_pub(self: &Rc<Self>, conn: <Name>Connection) -> Rc<dyn Node> {
        $ARGUMENTS_pub(conn, self)
    }
}

// Single-item stream: auto-wrap each value into a one-element Burst.
impl <Name>PubOperators for dyn Stream<<Name>Entry> {
    fn $ARGUMENTS_pub(self: &Rc<Self>, conn: <Name>Connection) -> Rc<dyn Node> {
        $ARGUMENTS_pub(conn, &self.map(|entry| burst![entry]))
    }
}
```

## 9. Integration tests — `integration_tests.rs`

Gate with `#[cfg(all(test, feature = "$ARGUMENTS-integration-test"))]`.

Write tests in this order (connection refused first — no container needed):

1. **`test_connection_refused`** — error propagates correctly
2. **`test_sub_snapshot`** — pre-seeded data appears in snapshot phase
3. **`test_sub_live_updates`** — events arrive after snapshot
4. **`test_pub_round_trip`** — `pub` writes → verify via direct client read
5. **`test_sub_no_race`** — concurrent write during snapshot→watch handoff not missed or duplicated (if applicable)
6. **`test_delete_events`** — delete/tombstone events handled correctly (if applicable)

Test structure — container startup is synchronous (SyncRunner); async client helpers use their own `Runtime`:

```rust
/// Start a container and return (container_guard, endpoint).
/// Hold the returned guard for the duration of the test.
fn start_container() -> anyhow::Result<(impl Drop, String)> {
    let container = GenericImage::new("vendor/image", "tag")
        .with_wait_for(WaitFor::message_on_stderr("ready"))
        .with_env_var("KEY", "value")
        .start()?;
    let port = container.get_host_port_ipv4(DEFAULT_PORT)?;
    Ok((container, format!("http://127.0.0.1:{port}")))
}

/// Seed data via the async client using a throwaway runtime.
fn seed_data(endpoint: &str, pairs: &[(&str, &str)]) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut client = SomeClient::connect(&[endpoint], None).await?;
        for (k, v) in pairs {
            client.put(*k, *v).await?;
        }
        Ok(())
    })
}

#[test]
fn test_sub_snapshot() -> anyhow::Result<()> {
    let (_container, endpoint) = start_container()?;
    seed_data(&endpoint, &[("/prefix/key", "val")])?;

    let conn = <Name>Connection::new(&endpoint);
    let collected = $ARGUMENTS_sub(conn, "/prefix/").collapse().collect();
    collected.clone().run(RunMode::RealTime, RunFor::Cycles(1))?;

    assert_eq!(collected.peek_value().len(), 1);
    Ok(())
}
```

## 10. Example — `wingfoil/examples/$ARGUMENTS/`

Create two files:

**`main.rs`** — realistic end-to-end use: seed data → `sub` → transform → `pub` → verify.

Register in `wingfoil/Cargo.toml`:
```toml
[[example]]
name = "$ARGUMENTS"
required-features = ["$ARGUMENTS"]
```

**`README.md`** — follows the KDB+ README pattern:

```markdown
# <Name> Adapter Example

<One paragraph describing what the example demonstrates.>

## Setup

```sh
docker run --rm -p PORT:PORT <image>:<tag>
```

## Run

```sh
cargo run --example $ARGUMENTS --features $ARGUMENTS
```

## Code

<Full source listing of main.rs>

## Output

<Expected console output>
```

### Register in the examples index

The "Core concepts" / "I/O adapters" tables live in **two** files:

- `/README.md` (top-level project README) — tables only, with absolute
  `https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/...`
  links so the tables render correctly on crates.io, docs.rs, etc.
- `wingfoil/examples/README.md` — same tables with relative links, plus
  per-adapter snippet sections lower down.

Three edits are required:

1. **Add a row to the "I/O adapters" table in `/README.md`** (or "Core
   concepts" if the example is not an I/O adapter). Use an **absolute**
   GitHub URL. Keep the description to one line:

   ```markdown
   | [`$ARGUMENTS`](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/$ARGUMENTS/) | <one-line description — what the adapter does and what the example demonstrates>. |
   ```

2. **Add the same row to the "I/O adapters" table in
   `wingfoil/examples/README.md`** with a **relative** link:

   ```markdown
   | [`$ARGUMENTS`](./$ARGUMENTS/) | <one-line description>. |
   ```

3. **Add a short snippet section further down in
   `wingfoil/examples/README.md`** with the same ~15-line minimal example the
   module-level doc in `mod.rs` uses, followed by a
   `[Full example.](./$ARGUMENTS/)` link. Match the format of the existing
   `### Kafka`, `### Fluvio`, `### etcd` sections.

Do **not** add the snippet to `/README.md` — only the one-row table entry
goes there. If the adapter is so significant that it warrants a flagship
section on the front page (like Order Book), flag it for the user rather
than silently adding it there.

## 11. CLAUDE.md — `wingfoil/src/adapters/$ARGUMENTS/CLAUDE.md`

Document:
- Module structure
- Key design decisions (especially any snapshot/watch race prevention)
- Pre-commit requirements (integration test command, fmt, clippy)
- Any gotchas (API version pins, type constraints, etc.)

## 12. CI — standalone workflow + register in integration-tests hub

### a. Create `.github/workflows/$ARGUMENTS-integration.yml`

Follow the etcd pattern exactly — `workflow_call` makes it callable from the hub,
`workflow_dispatch` allows manual runs, and `push` path trigger runs it on every
change to the adapter:

```yaml
name: $ARGUMENTS Integration Tests

on:
  workflow_call:
  workflow_dispatch:
  push:
    paths:
      - 'wingfoil/src/adapters/$ARGUMENTS/**'

env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: 0

jobs:
  $ARGUMENTS-integration:
    name: $ARGUMENTS Integration Tests
    runs-on: ubuntu-latest

    steps:
      - uses: actions/checkout@v4

      - name: Install Rust Toolchain
        uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
          override: true

      - name: Cache Rust Build Artifacts
        uses: Swatinem/rust-cache@v2
        with:
          shared-key: integration

      - name: Install system dependencies  # e.g. protobuf-compiler for gRPC clients; omit if not needed
        run: sudo apt-get install -y <pkg>

      - name: Run $ARGUMENTS integration tests
        run: |
          cargo test --features $ARGUMENTS-integration-test -p wingfoil \
            -- --test-threads=1 --nocapture
        env:
          RUST_LOG: INFO

      # --- Python bindings ---
      #
      # Include this block if the adapter ships Python bindings. It starts a
      # long-lived service container (the Rust tests above start their own
      # ephemeral containers via testcontainers, so Python needs a separate
      # one bound to the default host port the test file expects), builds the
      # bindings with maturin, and runs the pytest selection for this adapter's
      # marker. If the service isn't reachable the pytest step fails loudly.
      - name: Start $ARGUMENTS container for Python tests
        run: |
          docker run -d --name $ARGUMENTS-py -p PORT:PORT <image>:<tag>
          for i in $(seq 1 30); do
            nc -z localhost PORT && echo "$ARGUMENTS ready" && break
            echo "Waiting... ($i/30)"
            sleep 1
          done
          nc -z localhost PORT || (echo "$ARGUMENTS never became ready" && exit 1)

      - name: Set up Python
        uses: actions/setup-python@v5
        with:
          python-version: "3.11"
          cache: "pip"
          cache-dependency-path: wingfoil-python/pyproject.toml

      - name: Install maturin and build Python bindings
        run: |
          python -m venv wingfoil-python/.venv
          wingfoil-python/.venv/bin/pip install maturin pytest
          cd wingfoil-python && .venv/bin/maturin develop

      - name: Run Python $ARGUMENTS integration tests
        run: |
          cd wingfoil-python && .venv/bin/pytest -m requires_$ARGUMENTS tests/test_$ARGUMENTS.py -v

      - name: Dump $ARGUMENTS logs on failure
        if: failure()
        run: docker logs $ARGUMENTS-py

      - name: Stop $ARGUMENTS container
        if: always()
        run: docker stop $ARGUMENTS-py && docker rm $ARGUMENTS-py
```

### b. Register in `.github/workflows/integration-tests.yml`

Add a job alongside the existing adapters:

```yaml
  $ARGUMENTS-integration:
    name: $ARGUMENTS Integration Tests
    uses: ./.github/workflows/$ARGUMENTS-integration.yml
    secrets: inherit
```

This is how `all-tests.yml` → `integration-tests.yml` → `$ARGUMENTS-integration.yml`
chains together. Do **not** add directly to `release.yml`.

## 13. Python bindings — `wingfoil-python/`

### a. Feature flag — `wingfoil-python/Cargo.toml`

Add the adapter's feature to the wingfoil dependency:

```toml
wingfoil = { path = "../wingfoil", features = ["kdb", "zmq", "$ARGUMENTS"] }
```

### b. Binding module — `wingfoil-python/src/py_$ARGUMENTS.rs`

Create a file with two functions:

- **`py_$ARGUMENTS_sub`** — `#[pyfunction]` that calls the Rust `$ARGUMENTS_sub` and maps output types to Python objects.
- **`py_$ARGUMENTS_pub_inner`** — not `#[pyfunction]`; called from the `.$ARGUMENTS_pub()` stream method. Extracts Python objects from `PyElement`, converts to the native entry type, and calls the Rust `$ARGUMENTS_pub`.

Type conversion pattern:
- **Rust → Python**: map inside `Python::attach(|py| { ... })`, build `PyDict`/`PyList`/`PyBytes` etc., wrap results in `PyElement::new(...)`
- **Python → Rust**: inside `Python::attach`, call `elem.as_ref().bind(py)` then `downcast::<PyDict>()` etc. to extract fields

```rust
//! Python bindings for $ARGUMENTS adapter.

use crate::py_element::PyElement;
use crate::py_stream::PyStream;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::rc::Rc;
use wingfoil::adapters::$ARGUMENTS::{<Name>Connection, <Name>Entry, <Name>EventKind, $ARGUMENTS_pub, $ARGUMENTS_sub};
use wingfoil::{Burst, Node, Stream, StreamOperators};

/// Subscribe to <service> keys matching a prefix.
///
/// Each tick yields a `list` of event dicts:
/// `{"kind": "...", "key": str, "value": bytes, ...}`
#[pyfunction]
pub fn py_$ARGUMENTS_sub(endpoint: String, prefix: String) -> PyStream {
    let conn = <Name>Connection::new(endpoint);
    let stream = $ARGUMENTS_sub(conn, prefix);
    let py_stream = stream.map(|burst| {
        Python::attach(|py| {
            let items: Vec<Py<PyAny>> = burst
                .into_iter()
                .map(|event| {
                    let dict = PyDict::new(py);
                    // populate dict fields from event
                    dict.into_any().unbind()
                })
                .collect();
            PyElement::new(PyList::new(py, items).unwrap().into_any().unbind())
        })
    });
    PyStream(py_stream)
}

/// Inner implementation for the `.$ARGUMENTS_pub()` stream method.
pub fn py_$ARGUMENTS_pub_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    endpoint: String,
    // adapter-specific params
) -> Rc<dyn Node> {
    let conn = <Name>Connection::new(endpoint);
    let burst_stream: Rc<dyn Stream<Burst<<Name>Entry>>> = stream.map(move |elem| {
        Python::attach(|py| {
            let obj = elem.as_ref().bind(py);
            if let Ok(dict) = obj.downcast::<PyDict>() {
                let mut burst = Burst::new();
                burst.push(dict_to_entry(dict));
                burst
            } else if let Ok(list) = obj.downcast::<PyList>() {
                list.iter()
                    .filter_map(|item| item.downcast::<PyDict>().ok().map(|d| dict_to_entry(&d)))
                    .collect()
            } else {
                log::error!("$ARGUMENTS_pub: stream value must be a dict or list of dicts");
                Burst::new()
            }
        })
    });
    $ARGUMENTS_pub(conn, &burst_stream, /* params */)
}

fn dict_to_entry(dict: &Bound<'_, PyDict>) -> <Name>Entry {
    let key = dict.get_item("key").ok().flatten()
        .and_then(|v| v.extract::<String>().ok()).unwrap_or_default();
    let value = dict.get_item("value").ok().flatten()
        .and_then(|v| v.extract::<Vec<u8>>().ok()).unwrap_or_default();
    <Name>Entry { key, value }
}
```

Note: `Burst::new()` calls `TinyVec::new()` via the type alias. For collecting iterators into a `Burst`, use `.collect::<Burst<_>>()` — `TinyVec` implements `FromIterator`.

### c. Register in module — `wingfoil-python/src/lib.rs`

```rust
mod py_$ARGUMENTS;
// inside _wingfoil():
module.add_function(wrap_pyfunction!(py_$ARGUMENTS::py_$ARGUMENTS_sub, module)?)?;
```

### d. Stream method for pub — `wingfoil-python/src/py_stream.rs`

Add a `#[pymethods]` method to `PyStream`:

```rust
/// Write this stream to <service>.
///
/// Stream values must be dicts with "key" (str) and "value" (bytes),
/// or lists of such dicts for multi-entry writes per tick.
fn $ARGUMENTS_pub(&self, endpoint: String, /* adapter-specific params */) -> PyNode {
    PyNode::new(crate::py_$ARGUMENTS::py_$ARGUMENTS_pub_inner(&self.0, endpoint, /* params */))
}
```

### e. Python aliases — `wingfoil-python/python/wingfoil/__init__.py`

```python
# User-friendly aliases for $ARGUMENTS functions
$ARGUMENTS_sub = _ext.py_$ARGUMENTS_sub
```

### f. Integration tests — `wingfoil-python/tests/test_$ARGUMENTS.py`

**Never silently skip.** Integration tests are gated by a `requires_$ARGUMENTS` pytest marker
that is deselected by default (see `wingfoil-python/pyproject.toml` under `[tool.pytest.ini_options]`).
The default `pytest` run never collects these tests — it cannot be falsely green against a service
that is not up. The adapter's own integration workflow selects them with `-m requires_$ARGUMENTS`,
and if the service is unreachable the tests fail loudly with a real `ConnectionRefused` /
deserialization error rather than an `unittest.skip`.

Register the marker in `wingfoil-python/pyproject.toml`:

```toml
[tool.pytest.ini_options]
markers = [
    "requires_$ARGUMENTS: needs <service> on localhost:PORT",
    # ... existing markers ...
]
# Add the new marker to the deselect expression so the default pytest run skips it.
addopts = "-m 'not requires_etcd and not requires_kdb and not requires_otel and not requires_iceoryx2 and not requires_$ARGUMENTS'"
```

Then write the tests — no TCP probe, no `skipUnless`, just the marker:

```python
"""Integration tests for $ARGUMENTS Python bindings.

Selected via `-m requires_$ARGUMENTS`. Without <service> on localhost:PORT
the tests will fail loudly — they do not silently skip.

Setup:
    docker run --rm -p PORT:PORT <image>:<tag>
"""

import unittest

import pytest

ENDPOINT = "http://localhost:PORT"


@pytest.mark.requires_$ARGUMENTS
class TestSub(unittest.TestCase):
    def test_sub_returns_expected_shape(self):
        from wingfoil import $ARGUMENTS_sub
        stream = $ARGUMENTS_sub(ENDPOINT, "/prefix/").collect()
        stream.run(realtime=False, duration=5.0)
        result = stream.peek_value()
        self.assertIsInstance(result, list)
        # assert dict shape of each event


@pytest.mark.requires_$ARGUMENTS
class TestPub(unittest.TestCase):
    def test_pub_round_trip(self):
        from wingfoil import constant
        constant({"key": "/test/k", "value": b"v"}).$ARGUMENTS_pub(ENDPOINT).run(
            realtime=False, cycles=1
        )
        # verify via stdlib HTTP/socket that the key was written
```

For services with an HTTP management API (e.g. etcd v3 gRPC-gateway), seed and verify data
using `urllib` + `json` + `base64` from stdlib to avoid extra Python dependencies.

**Feature-gated bindings (like iceoryx2):** if the Python binding is only exposed when
wingfoil-python is built with a non-default Cargo feature, use the marker alone —
don't skip on `hasattr(_ext, "...")`. Module-level references to feature-gated constants
(e.g. inside `@pytest.mark.parametrize(...)` decorators) must be avoided because pytest
imports the file during collection even when deselecting; parametrize with string IDs
and resolve the constants inside the test body instead.

### g. Unit-level coverage tests (no live service)

The integration tests above are deselected from the default `pytest` run, so they
contribute **nothing** to the default coverage report. For every adapter, also add
unit-level tests that run without a live service. These tests are what keep the
binding module visible in coverage (`py_<name>.rs`) and catch regressions in the
pyo3 marshaling glue.

Cover three categories in the same `tests/test_$ARGUMENTS.py` file (no pytest marker
so they run by default alongside unit tests):

1. **Construction** — that `py_$ARGUMENTS_sub` and the `.$ARGUMENTS_pub()` stream
   method each construct their stream/node without an active connection. This
   exercises argument parsing, default values, and `#[pyo3(signature = ...)]` bindings.

2. **Marshaling closures under failure** — run the pub node with an unreachable
   endpoint (e.g. `127.0.0.1:1`). The upstream value ticks before the async consumer
   attempts to connect, so the `map` closure that converts `PyElement` → native entry
   (`dict_to_record`, etc.) is exercised end-to-end; the run then fails at connect
   time, which the test asserts via `assertRaises(Exception)`. Cover each input
   shape the closure accepts (single dict, list of dicts, and the fallthrough error
   branch for unsupported types).

3. **Early validation errors** — any argument that is validated before the I/O
   begins (e.g. `start_offset < 0`, unknown codec name, empty stages list) should
   have its own test asserting the raised exception.

```python
# Unreachable endpoint: TCP reject on loopback, guaranteed never to host a service.
UNREACHABLE_ENDPOINT = "127.0.0.1:1"


class TestFluvioConstruction(unittest.TestCase):
    def test_fluvio_sub_default_partition_and_offset(self):
        from wingfoil import $ARGUMENTS_sub

        stream = $ARGUMENTS_sub(UNREACHABLE_ENDPOINT, "topic")
        self.assertIsNotNone(stream)

    def test_pub_method_constructs_node(self):
        from wingfoil import constant

        node = constant({"value": b"v"}).$ARGUMENTS_pub(UNREACHABLE_ENDPOINT, "topic")
        self.assertIsNotNone(node)


class TestFluvioUnreachable(unittest.TestCase):
    def test_pub_single_dict_marshals_then_errors(self):
        # dict_to_record runs on the upstream tick; connect then fails.
        from wingfoil import constant

        node = constant({"value": b"v", "key": "k"}).$ARGUMENTS_pub(
            UNREACHABLE_ENDPOINT, "topic"
        )
        with self.assertRaises(Exception):
            node.run(realtime=True, cycles=1)

    def test_pub_list_of_dicts_marshals_then_errors(self):
        from wingfoil import constant

        records = [{"key": "k", "value": b"v"}, {"value": b"v2"}]  # keyless path
        node = constant(records).$ARGUMENTS_pub(UNREACHABLE_ENDPOINT, "topic")
        with self.assertRaises(Exception):
            node.run(realtime=True, cycles=1)

    def test_pub_bad_value_type_marshals_empty_burst(self):
        # Fallthrough branch: neither dict nor list. Logs an error and emits
        # an empty burst; the run still fails at connect time.
        from wingfoil import constant

        node = constant("not a dict").$ARGUMENTS_pub(UNREACHABLE_ENDPOINT, "topic")
        with self.assertRaises(Exception):
            node.run(realtime=True, cycles=1)

    def test_sub_invalid_arg_errors(self):
        # If sub validates any argument eagerly (e.g. negative offset), assert it.
        from wingfoil import $ARGUMENTS_sub

        stream = $ARGUMENTS_sub(UNREACHABLE_ENDPOINT, "topic", start_offset=-1)
        with self.assertRaises(Exception):
            stream.collect().run(realtime=True, cycles=1)
```

Source-only adapters (`py_$ARGUMENTS_sub` without a pub counterpart) should still
cover construction and any eager argument validation. Codec / builder pyclasses
(e.g. `PyWebServer`) should have construction tests for every constructor option
and every rejected invalid value.

## 14. Pre-commit checklist

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
cargo test --features $ARGUMENTS-integration-test -p wingfoil -- --test-threads=1
cd wingfoil-python && maturin develop
# Unit-level coverage tests (no marker; default run):
wingfoil-python$ pytest tests/test_$ARGUMENTS.py
# Integration tests (require live service):
wingfoil-python$ pytest -m requires_$ARGUMENTS tests/test_$ARGUMENTS.py
```

All five must pass before committing. The default `pytest` run (no `-m`) deselects
`requires_$ARGUMENTS` tests and picks up the unit-level coverage tests from step 13.g.
The marker'd run exercises the live-service paths — make sure the backing service
is up locally or that step will fail loudly.

## 15. Self-review with a fresh context

Before opening a PR, do a clean-context review pass. This catches drift between
what the skill prescribes and what actually got built — missing `CLAUDE.md`,
forgotten Python alias, skipped CI registration, snippet not added to
`wingfoil/examples/README.md`, etc. — that is easy to miss after spending hours
in the implementation.

Run this as a subagent (so the parent context stays clean) with these tasks:

1. **Re-read this skill file end to end.** Then walk the diff (`git diff main...HEAD`)
   step-by-step against sections 1–14 and produce a checklist of what is present,
   what is missing, and what diverges. Flag every divergence — even intentional
   ones — so the author can confirm or fix.

2. **Validate every step's artifacts exist:**
   - Branch matches step 1
   - Both feature flags in `wingfoil/Cargo.toml` (step 2)
   - `pub mod` in `wingfoil/src/adapters/mod.rs` (step 3)
   - File layout matches step 5 (or a documented variation)
   - Module-level `//!` doc covers setup + producer + consumer (step 6)
   - Integration tests gated by `$ARGUMENTS-integration-test` (step 9)
   - Example + `README.md` + entries in **both** `/README.md` and
     `wingfoil/examples/README.md` tables, plus snippet section (step 10)
   - Adapter `CLAUDE.md` present (step 11)
   - Standalone CI workflow + entry in `integration-tests.yml` (step 12)
   - Python feature flag, binding module, `lib.rs` registration, `PyStream`
     method, `__init__.py` alias, marker registered in `pyproject.toml`, and
     both unit + integration tests (step 13)

3. **Run the pre-commit checklist from step 14** and confirm all five commands
   pass. Do not skip any.

4. **Review for quality and simplicity** (the `simplify` skill territory):
   - No `Mutex`/`RwLock` taken inside `cycle()` / `setup()` (the invariant at the
     top of this skill)
   - No speculative abstractions, dead code, or backwards-compat shims
   - No comments that just restate the code
   - No half-finished implementations

Fix any issues found before committing. Treat a clean self-review as part of
"done" — not an optional extra.
