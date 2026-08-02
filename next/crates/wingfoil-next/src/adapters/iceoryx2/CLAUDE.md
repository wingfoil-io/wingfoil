# iceoryx2 Adapter (wingfoil-next)

Zero-copy inter-process (and intra-process) publish/subscribe over shared
memory. Ports classic `wingfoil::adapters::iceoryx2` onto the Op model.

Synchronous and poll-based, so — like classic, and unlike the networked async
adapters — the feature deliberately does **not** pull in `async`/tokio.

## Layout

```
adapters/
  iceoryx2/
    mod.rs        # module docs, contracts, Iceoryx2ServiceVariant / Iceoryx2Mode,
                  #   Iceoryx2SubOpts / Iceoryx2PubOpts / Iceoryx2PubSliceOpts,
                  #   FixedBytes, Iceoryx2Error; re-exports read::* / write::*
    read.rs       # iceoryx2_sub / _sub_with / _sub_opts / _sub_slice / _sub_slice_opts
    write.rs      # Iceoryx2SinkOps, Iceoryx2SliceSinkOps
    CLAUDE.md     # this file
```

## Feature gating

```toml
iceoryx2 = ["dep:iceoryx2", "dep:thiserror", "wingfoil/iceoryx2"]
iceoryx2-integration-test = ["iceoryx2"]     # cross-process Ipc tests; no container
```

`wingfoil/iceoryx2` brings in the **classic** crate's `ZeroCopySend` impl for
`Traced<T, L>`, so latency-stamped payloads cross an iceoryx2 hop — the classic
adapter's latency round-trip test relies on the same impl, and so do the
`latency_pub`/`latency_sub` examples.

## Entry points

| Item | Kind | Payload |
|---|---|---|
| `iceoryx2_sub` / `iceoryx2_sub_with` / `iceoryx2_sub_opts` | source | typed `T: ZeroCopySend` |
| `iceoryx2_sub_slice` / `iceoryx2_sub_slice_opts` | source | `[u8]` slices |
| `Iceoryx2SinkOps::iceoryx2_pub` / `_pub_with` / `_pub_opts` | sink trait on `Stream<Burst<T>>` | typed |
| `Iceoryx2SliceSinkOps::iceoryx2_pub_slice` / `_slice_with` / `_slice_opts` | sink trait on `Stream<Burst<Vec<u8>>>` | slices |

The `_with` / `_opts` family mirrors classic's constructor ladder: bare name →
name + `Iceoryx2ServiceVariant` → name + full options.

## What to know before changing it

- **Three polling modes** (`Iceoryx2Mode`), same trade-off classic offered,
  all returning the same `Stream<Burst<T>>`:
  - `Spin` (default) — a busy-spin `custom_node` polling the subscriber port on
    the graph thread every cycle. Lowest latency (~1–5 µs), highest CPU (the
    kernel never parks while it exists).
  - `Threaded` — a background thread polls with a 10 µs yield and feeds the
    `channel` layer. Lower CPU, one channel hop.
  - `Signaled` — event-driven, blocking on a `WaitSet` attached to the
    service's `<name>.signal` Event service, which the publisher notifies.
    Lowest CPU, highest latency.
  Samples between cycles group into one `Burst` — for `Spin` because the cycle
  drains the port into a burst, for the others because the channel layer groups
  same-instant values.
- **All three sources are realtime-only, rejected at wiring** (register B2).
  Classic silently ran the poll loop against the fast-forwarded historical
  clock.
- **The sink does *not* reject or no-op under historical replay — deliberate
  classic parity.** Unlike [`zmq_pub`](../zmq/CLAUDE.md) (which errors) and the
  telemetry exporters (which no-op), classic's iceoryx2 publisher publishes
  under either run mode, and a backtest piping its output into shared memory is
  a legitimate use. Do not "fix" this to match the other adapters.
- **Ports are created at graph `start()`**, as in classic, so wiring is pure: a
  bad service name or a contract mismatch aborts the *run* with node context,
  not graph construction (register A1/A4).
- **`history_size` and `subscriber_max_buffer_size` are service configuration —
  every participant opening or creating the same service must agree.** A
  mismatch surfaces as `Iceoryx2Error::ServiceConfigMismatch` (or
  `ServiceOpenFailed`, depending on what iceoryx2 reports) carrying the service
  name, variant and both sizes. `Iceoryx2ServiceContract` /
  `Iceoryx2SliceContract` make the derived values inspectable at wiring — use
  them when debugging a "service won't open" report.
- **Zero-copy constraints on typed payloads:** `ZeroCopySend`, `#[repr(C)]`,
  self-contained (no heap allocations, no pointers to external data). For
  variable-length bytes use the slice API, or `FixedBytes<N>` to carry them
  through the typed API.
- **The publisher notifies the Event listener after a non-empty burst** so a
  `Signaled` subscriber wakes; connections are refreshed every tenth cycle
  (`update_connections`), matching classic. A loan/send failure aborts the run
  with context.
- `Iceoryx2ServiceVariant::Local` is in-process and heap-backed — it needs no
  shared memory at all, which is why the default test tier uses it.

## Deviations from classic

Canonical list: the `# Deviations from classic` block in `iceoryx2/mod.rs` —
four items: sources take a `GraphBuilder` + `RunMode` and return `Result`
(wiring-time historical rejection, B2); the sinks are extension traits
returning `Stream<()>` rather than free `iceoryx2_pub*` functions (D1); the
sink deliberately does **not** reject or no-op historically (parity); and ports
are created at `start()` with pure wiring (A1/A4). Every classic capability —
typed and slice payloads, all three polling modes, `Ipc`/`Local` variants, the
option/`_with`/`_opts` constructor family, the service contracts, `FixedBytes`,
and the typed `Iceoryx2Error` — is preserved.

Classic's Criterion benches are ported: `benches/iceoryx2.rs` and
`benches/iceoryx2_modes.rs`, both gated on the `iceoryx2` feature and run with
`cargo bench --bench iceoryx2[_modes]`. They need shared memory, so they are
compiled in CI but not run; benches are deliberately not a CI gate (criterion
wall-clock is too noisy on shared runners). See `benches/README.md`.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/iceoryx2_adapter.rs` | `#![cfg(feature = "iceoryx2")]` | nothing — in-process `Local` variant |
| `tests/iceoryx2_integration.rs` | `#![cfg(feature = "iceoryx2-integration-test")]` | real `/dev/shm`, cross-process |

`iceoryx2_adapter.rs` is the parity port of classic's `local_tests.rs` (end to
end over the `Local` variant, no shared memory, no subprocesses) plus the
wiring-time guards. `iceoryx2_integration.rs` ports classic's
`integration_tests.rs` — cross-process `Ipc` over real shared memory, **no
container** (which is why the feature adds no `testcontainers`).

```bash
cargo test -p wingfoil-next --features iceoryx2 --test iceoryx2_adapter
cargo test -p wingfoil-next --features iceoryx2-integration-test -- --test-threads=1
```

Setup: iceoryx2 needs writable shared memory — `/dev/shm` on Linux, normally
available out of the box. Stale segments left by a crashed process may need
manual cleanup under `/dev/shm/`.

**Workflow:** `.github/workflows/iceoryx2-next-integration.yml` (in
`integration-tests.yml`), Rust leg + `pytest -m requires_iceoryx2` Python leg.

## Examples

All `required-features = ["iceoryx2"]`; **run the subscriber first**:

- `examples/iceoryx2/pub.rs` / `sub.rs` → `iceoryx2_pub` / `iceoryx2_sub`
- `examples/latency/pub.rs` / `sub.rs` → `latency_pub` / `latency_sub` — the
  cross-process demonstration of the Phase-5 latency infrastructure
  (`latency_stages!` + `Traced<T, L>` + `.stamp::<Stage>()` + `latency_report`)
  over a shared-memory hop.

## Python

`wingfoil-next-python` feature
`iceoryx2 = ["wingfoil-next/iceoryx2", "_common"]`.

**In `all-adapters` — but NOT in the maturin wheel.** It is pure Rust, so its
tests run in the normal `next-python-test.yml` job; it stays out of the wheel's
default features because it is **Linux/POSIX-only** and a wheel carrying it
cannot be built for the platforms that would otherwise work. (Contrast
[`aeron`](../aeron/CLAUDE.md), which is out of *both*.) Legacy
`wingfoil-python` draws the line in exactly the same place.

- Entry points, `#[pyadapter]` in `src/adapters/iceoryx2.rs`: `iceoryx2_sub`,
  `iceoryx2_pub` — the **slice** pair. Mode and service-variant selectors are
  **strings**, not `#[pyclass]` enums (legacy used `Iceoryx2ServiceVariant` /
  `Iceoryx2Mode` pyclasses).
- Both take an optional **`stages`** list (legacy's latency-tracing path). With
  it, a sample is a `[u64; len(stages)]` little-endian stamp header followed by
  the payload, and the Python value is a `TracedBytes` carrying a `Latency`
  rather than `bytes` — the same layout a Rust peer's `latency_stages!` record
  has, so a Python subscriber reads a Rust publisher's stamps. The split/pack
  goes through the **transport seam** in `wingfoil-next-python/src/latency.rs`
  (`STAMP_BYTES`, `PyLatency::create_from_bytes` / `header_bytes` /
  `stages_ref`, `check_stages`), not a local decoder. Because a `#[pyadapter]`
  fn has one return type, the shape branch cannot live at the erasure seam —
  the subscriber returns `Stream<Burst<PyElement>>` on both paths and decodes
  in a node of its own. Three deviations from legacy, all fail-loudly: a short
  frame aborts (legacy returned the whole frame as payload with an all-zero
  record), a record whose stage list disagrees with the wired one aborts, and
  `stages` publishes bursts (legacy took a single value).
- `src/latency.rs` is in this adapter's workflow `paths:` triggers, since the
  traced round trip is the only thing exercising the header over a real hop.
- Because it is out of the wheel, the workflow's Python leg builds it
  explicitly with `maturin develop -F …,iceoryx2` and asserts the symbol is
  present before running `pytest -m requires_iceoryx2`.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test -p wingfoil-next --features iceoryx2
cargo test -p wingfoil-next --features iceoryx2-integration-test -- --test-threads=1
```
