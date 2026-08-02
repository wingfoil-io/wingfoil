# Aeron Adapter (wingfoil-next)

The Aeron IPC/UDP low-latency message transport: a typed-parser subscription
**source** (with an optional lifecycle-status side-channel) and a publication
**sink**. Ports classic `wingfoil::adapters::aeron` onto the Op model.

Synchronous and poll-based, so — like classic, and unlike the networked async
adapters — the feature deliberately does **not** pull in `async`/tokio.

## Layout

```
adapters/
  aeron/
    mod.rs                 # module docs, AeronMode, AeronSubOptions, the two source fns, re-exports
    read.rs                # spin + threaded subscriber sources
    write.rs               # AeronSinkOps (publisher; realtime check at graph start())
    transport.rs           # AeronSubscriberBackend / AeronPublisherBackend traits + MockSubscriber/MockPublisher
    buffer.rs              # FragmentBuffer / FragmentHeader / ClaimBuffer (zero-copy commit-or-abort)
    channel.rs             # ChannelUri builders — hand-written URIs fail silently in the driver
    error.rs               # TransportError
    status.rs              # AeronStatus
    rusteron_backend.rs    # `aeron` feature — C++ FFI (AeronHandle)
    aeron_rs_backend.rs    # `aeron-rs` feature — pure Rust (AeronRsHandle)
    CLAUDE.md              # this file
```

## Feature gating — enable exactly one backend

```toml
aeron        = ["dep:rusteron-client"]        # C++ FFI, recommended for production
aeron-rs     = ["dep:aeron-rs"]               # pure Rust, EXPERIMENTAL
aeron-driver = ["aeron", "dep:rusteron-media-driver"]   # embeds a media driver in-process
aeron-integration-test = ["aeron", "aeron-rs", "dep:testcontainers", "dep:libc"]
```

The module is gated `#[cfg(any(feature = "aeron", feature = "aeron-rs"))]`.

The `aeron` (rusteron) backend **builds the Aeron C library from source** and
needs `cmake >= 3.30`, `clang`, `uuid-dev`, `libbsd-dev` on the build machine.
`aeron-rs` needs none of that. This build cost is why aeron is excluded from
several roll-ups (see Python, below) and why a local `cargo lint-all` often
fails in a sandbox — substitute
`cargo clippy -p wingfoil-next --all-features --all-targets -- -D warnings`
and say so in the PR.

## Entry points

| Item | Kind | Notes |
|---|---|---|
| `aeron_sub_fragment(g, run_mode, subscriber, parser, opts)` | source | `Result<Stream<Burst<T>>>` |
| `aeron_sub_fragment_with_status(g, run_mode, subscriber, parser, opts)` | source | `Result<(Stream<Burst<T>>, Stream<Burst<AeronStatus>>)>` |
| `AeronSinkOps::aeron_pub` / `aeron_pub_with_status` | sink trait on `Stream<Burst<T>>` | |

Handles come from a backend: `AeronHandle::connect()` (rusteron) or
`AeronRsHandle` (aeron-rs), then `.subscription(uri, stream_id, timeout)` /
`.publication(...)`.

## What to know before changing it

- **⚠️ `aeron-rs` takes a lock on the graph thread.** It hands back
  `Arc<Mutex<…>>` subscription/publication handles shared with its own
  background client-conductor thread, so the backend locks on every
  `poll()`/`offer()`. To keep that off the graph cycle the subscriber backend
  reports `supports_graph_thread_poll() == false` and the source factories
  **automatically downgrade a requested `AeronMode::Spin` to
  `AeronMode::Threaded`** for it (with a warning). `Spin` is unreachable for
  the `aeron-rs` subscriber by construction. The `aeron-rs` **publisher** has
  no threaded mode and always locks on the calling (graph) thread — there is no
  automatic downgrade for the publish side. Use the rusteron backend on any
  latency-sensitive path. Two unit tests pin this
  (`non_graph_thread_backend_downgrades_spin_to_threaded`,
  `lock_free_backend_keeps_spin`).
- **Two polling modes** via `AeronSubOptions`:
  - `Spin` (primary) — polls inside the cycle on the graph thread, through a
    busy-spin `custom_node`. Zero thread-crossing latency, one core burned.
    Ticks only when fragments arrive (or a status transition is observed), so
    downstream stays reactive.
  - `Threaded` (secondary) — a dedicated thread polls with exponential idle
    back-off and feeds the `channel` layer. One channel hop, realtime only.
  `fragment_limit` caps fragments per `poll()`; `DEFAULT_FRAGMENT_LIMIT` = 256
  (unit test `default_fragment_limit_is_256`).
- **Sources reject `RunMode::HistoricalFrom` at wiring** (register B2). Classic's
  spin subscriber silently ran against the fast-forwarded historical clock; the
  threaded mode rides the channel layer and would deadlock at `start`. The
  **publisher** keeps classic's behaviour exactly: its realtime check fires at
  graph `start()` and aborts the run.
- **Status is transition-only and derived in a fixed order** after each
  successful poll/offer: `is_closed()` → `Closed` (terminal, checked first),
  `is_connected()` → `Connected`, else `Disconnected` — so a transient I/O
  failure never registers a phantom transition. In threaded mode status rides
  **in-band** with the data so a `Connected` transition stays ordered before
  the fragments that followed it. `_with_status` factories are
  **parallel-additive**: never change the primary signature to add status.
- **`ChannelUri` exists because typos are silent.** The media driver accepts a
  malformed `aeron:udp?endpoint=…` and it surfaces only as a
  never-connecting publication. Prefer the builders.
- **A live media driver is required to construct a subscription/publication** —
  there is no offline construction path. That is why the adapter's own tests
  drive `MockSubscriber` / `MockPublisher`, and why those mocks are **public**
  (see deviations).
- `ClaimBuffer` has an explicit commit-or-abort lifecycle — the zero-copy path.
  Don't let one escape without either.

## Deviations from classic

Canonical list: the `# Deviations from classic` block in `aeron/mod.rs` — five
items:

1. Sources take a `GraphBuilder` + `RunMode` and return `Result` (wiring-time
   historical rejection, register B2); the publisher keeps classic's
   `start()`-time check.
2. **The status side-channel is a plain stream, not a node type.** Classic's
   `AeronStatusStream` (a `MutableNode` driven through `clear()`/`record()`)
   has **no next twin**: next multiplexes status with data over one internal
   envelope and splits it out with `map_filter`, the same shape as
   [`zmq`](../zmq/CLAUDE.md). Observable behaviour is identical. This also
   makes *spin* mode carry status in-band where classic used a shared
   `Rc<RefCell<..>>`.
3. The sink is an extension trait returning `Stream<()>`, not `Rc<dyn Node>`
   (register D1).
4. **The mock backends are public.** Classic gated `MockSubscriber` /
   `MockPublisher` behind `#[cfg(test)]` inside the crate; next's adapter tests
   live in `tests/` and compile against the public library. They are tiny and
   dependency-free.
5. Plain `aeron_sub_fragment` never derives status — classic held an
   `Option<Rc<RefCell<AeronStatusStream>>>` and skipped derivation when `None`;
   next passes the same choice as a `track_status` flag. Same behaviour, no
   allocation.

Classic's Criterion benches are ported, all four gated on the `aeron` feature:
`aeron_publication_latency`, `aeron_subscription_throughput`,
`aeron_transceiver`, and `aeron_allocation_tracking` (which also wants
`dhat-heap`), over the shared `benches/aeron/common/mod.rs`. They need a live
media driver, so they are compiled but not run; benches are deliberately not a
CI gate. Building them needs CMake ≥ 3.30 for `rusteron-client`'s bundled
Aeron C build — see the repo-root `CLAUDE.md`. See `benches/README.md`.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/aeron_adapter.rs` | `#![cfg(any(feature = "aeron", feature = "aeron-rs"))]` | nothing — mock backends |
| `tests/aeron_integration.rs` | `#![cfg(feature = "aeron-integration-test")]` | a live media driver |

`aeron_adapter.rs` is the parity port of classic's node-level unit tests
(`mod.rs`, `sub_fragment_node.rs`, `pub_node.rs`), driven by the mocks.
`aeron-integration-test` enables **both** backends so their handle-construction
paths are covered, and runs against a testcontainers `aeronmd` bind-mounting
`/dev/shm`.

```bash
cargo test -p wingfoil-next --features aeron --test aeron_adapter
cargo test -p wingfoil-next --features aeron-integration-test -- --test-threads=1
```

Standalone driver for local work:

```sh
java -cp aeron-all-*.jar io.aeron.driver.MediaDriver
```

**Workflow:** `.github/workflows/aeron-next-integration.yml` (in
`integration-tests.yml`) — it installs the C toolchain, starts a driver
container, runs the Rust leg, then a Python leg with
`pytest -m requires_aeron`. `aeron_integration` is deliberately **excluded**
from `rust-test.yml`'s `test-next` run (`-E 'not binary(/_integration$/)'`):
without a driver it only exercises connection timeouts, slowly — it once spent
1m46s of a 3m17s run sleeping.

## Examples

Both `required-features = ["aeron"]`, both needing a live media driver:

- `examples/aeron/main.rs` → `aeron_adapter`
- `examples/aeron/status_circuit_breaker.rs` → `aeron_status_circuit_breaker`
  (the status stream driving an on-graph circuit breaker)

## Python

`wingfoil-next-python` feature `aeron = ["wingfoil-next/aeron", "_common"]`.

**Out of `all-adapters` AND out of the maturin wheel** — the only adapter
excluded from both. `rusteron-client` builds the Aeron C library from source
(clang, libuuid, CMake >= 3.30), and `next-python-test.yml` installs only
`protobuf-compiler` and `patchelf`. Consequences to remember:

- Its Rust `#[cfg(test)]` marshaling tests do **not** run in
  `next-python-test.yml` (`--features all-adapters`); the aeron workflow's
  Python leg is their only home.
- Opt in with `maturin develop -F extension-module,aeron` — **`-F` replaces**
  the `pyproject.toml` feature list rather than adding to it, so
  `extension-module` must be spelled out.
- `libaeron.so` lives in the cargo build directory and is not on the loader
  path, so the pytest step needs `LD_LIBRARY_PATH` pointed at it or the
  extension fails to *import*.

Entry points, `#[pyadapter]` in `src/adapters/aeron.rs`: `aeron_sub`,
`aeron_sub_with_status`, `aeron_pub`, `aeron_pub_with_status`. Mode selectors
are **strings**, not `#[pyclass]` enums (legacy used an `AeronMode` pyclass) —
and the binding validates its own arguments **before** contacting the media
driver, so a typo in `mode=` doesn't report itself as a driver timeout.
Tests: `tests/test_aeron.py`, `@pytest.mark.requires_aeron` (needs a running
driver via `AERON_DIR`).

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo clippy -p wingfoil-next --all-features --all-targets -- -D warnings   # if lint-all can't build the C lib
cargo test -p wingfoil-next --features aeron
```
