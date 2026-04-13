Implement a new I/O adapter for wingfoil named `$ARGUMENTS`. Follow these steps in order. Work test-driven: write each test before its implementation.

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

## 3. Module registration — `wingfoil/src/adapters/mod.rs`

```rust
#[cfg(feature = "$ARGUMENTS")]
pub mod $ARGUMENTS;
```

## 4. Docker image / container setup

Choose an official or well-maintained image for the service. Use `SyncRunner` (blocking) so container startup stays in a plain `#[test]` function without a wrapping async runtime:

```rust
// In integration_tests.rs
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};

// Option A — if testcontainers-modules has a module for this service:
use testcontainers_modules::some_service::SomeService;
let container = SomeService::default().start()?;
let port = container.get_host_port_ipv4(DEFAULT_PORT)?;

// Option B — GenericImage (most common; use this when no module exists):
let container = GenericImage::new("vendor/image", "tag")
    .with_wait_for(WaitFor::message_on_stderr("ready to serve"))
    .with_env_var("KEY", "value")
    .start()?;
let port = container.get_host_port_ipv4(DEFAULT_PORT)?;
let endpoint = format!("http://127.0.0.1:{port}");
```

The container is stopped automatically when dropped. Hold the container in a binding for the duration of the test (`let _container = ...`). No `docker-compose.yml` needed.

## 5. File structure

```
wingfoil/src/adapters/$ARGUMENTS/
  mod.rs               # Connection config, public types, re-exports
  read.rs              # sub function (producer)
  write.rs             # pub function (consumer)
  integration_tests.rs # gated by $ARGUMENTS-integration-test feature
  CLAUDE.md            # documents design decisions and pre-commit requirements
```

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

Add `//!` module-level doc at the top of `mod.rs` covering:

- One-line description of what the adapter does
- Setup: local Docker one-liner to start the service
- `# Subscribing` section: minimal `ignore` code block showing `$ARGUMENTS_sub`
- `# Publishing` section: minimal `ignore` code block showing `$ARGUMENTS_pub`
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

      - name: Install system dependencies  # e.g. protobuf-compiler for gRPC clients; omit if not needed
        run: sudo apt-get install -y <pkg>

      - name: Run $ARGUMENTS integration tests
        run: |
          cargo test --features $ARGUMENTS-integration-test -p wingfoil \
            -- --test-threads=1 --nocapture
        env:
          RUST_LOG: INFO
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
wingfoil = { path = "../wingfoil", features = ["kdb", "zmq-beta", "$ARGUMENTS"] }
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

Follow the KDB+ test pattern: check if the service is available, skip if not, use stdlib HTTP/socket to seed and verify data without extra Python dependencies.

For services that expose an HTTP management API (e.g. etcd v3 gRPC-gateway), use `urllib` + `json` + `base64` from stdlib to issue puts/gets.

```python
import socket
import unittest

ENDPOINT = "http://localhost:PORT"

def service_available():
    try:
        with socket.create_connection(("localhost", PORT), timeout=1):
            return True
    except OSError:
        return False

SERVICE_AVAILABLE = service_available()

@unittest.skipUnless(SERVICE_AVAILABLE, "<service> not running on localhost:PORT")
class TestSub(unittest.TestCase):
    def test_sub_returns_expected_shape(self):
        from wingfoil import $ARGUMENTS_sub
        stream = $ARGUMENTS_sub(ENDPOINT, "/prefix/").collect()
        stream.run(realtime=False, duration=5.0)
        result = stream.peek_value()
        self.assertIsInstance(result, list)
        # assert dict shape of each event

@unittest.skipUnless(SERVICE_AVAILABLE, "<service> not running on localhost:PORT")
class TestPub(unittest.TestCase):
    def test_pub_round_trip(self):
        from wingfoil import constant
        constant({"key": "/test/k", "value": b"v"}).$ARGUMENTS_pub(ENDPOINT).run(
            realtime=False, cycles=1
        )
        # verify via stdlib HTTP/socket that the key was written
```

## 14. Pre-commit checklist

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
cargo test --features $ARGUMENTS-integration-test -p wingfoil -- --test-threads=1
cd wingfoil-python && maturin develop && pytest tests/test_$ARGUMENTS.py
```

All four must pass before committing.
