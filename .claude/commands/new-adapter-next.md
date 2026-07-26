Implement a new I/O adapter for **wingfoil-next** named `$ARGUMENTS`, under
`next/crates/wingfoil-next/src/adapters/`. Follow these steps in order. Work
test-driven: write each test before its implementation.

Adapters in next are built **strictly on the public Op-pattern API** — sources
over [`channel`]/[`poll`], sinks over [`for_each`], compute over custom `Op`s —
never by reaching into the engine's internals. The existing adapters are the
reference implementations; read them before writing code:

- `src/adapters/lines.rs` — the smallest complete I/O edge (replay source +
  realtime `poll` tail + sink), dependency-free.
- `src/adapters/csv.rs` — the serde-typed parsing cousin (feature-gated deps,
  wiring-time `Result`, header introspection).
- `src/adapters/zmq.rs` — a live sync-streaming subscriber over a background
  thread, using [`source_at_start`] to **defer the socket connect + thread spawn
  to graph `start()`** (see the source shape in step 7); plus a status stream.
- `src/adapters/augurs.rs` — a pure-compute adapter (custom `Op`s +
  `#[op(build = ...)]` + config builder types, no I/O).
- `src/async_source.rs` — `produce_async` for async client libraries.
- `src/channel.rs` — the `Message` envelope and `ChannelSender`.

## The parity obligation (read first)

Wingfoil Next's governing design objective (see `next/README.md`) is to become
a **strict superset of legacy wingfoil**. If a classic adapter named
`$ARGUMENTS` exists under `wingfoil/src/adapters/$ARGUMENTS/`, it is your
**parity oracle**:

- Read its `mod.rs` docs, its `CLAUDE.md`, its tests, and its example first.
- Every public capability (function, config knob, mode enum, event/entry type)
  needs a next equivalent — or an explicit deviation note in the module docs
  and, if it's a capability gap, in the matrix in `next/docs/port-plan.md`.
- Port its unit tests as parity tests: identical values **and** tick times.
- Keep error-message compatibility where tests assert on messages (see how
  `csv_read` reuses the classic "failed to deserialize row" context).
- Port its example too — examples are part of the superset objective.

If no classic adapter exists, you are defining new surface: keep the naming
and layering conventions below so a future legacy backport stays mechanical.

## Feed lessons back into this skill

Adapter development keeps surfacing things this skill doesn't yet capture — a
recurring pitfall, a CI gate you didn't expect, a pattern worth codifying, a
deviation that should be a rule. **When you hit one, consider baking it into
this file** (`.claude/commands/new-adapter-next.md`), ideally in the same PR, or
flag it for a follow-up skill update. This skill is meant to grow with every
port: several rules below (credential redaction, live-source rejection, the
slicer cfg-gate reuse, the dependency-review gate) were added exactly this way
after a port hit them. Record cross-cutting classic↔next differences in
`next/docs/deviation-register.md`, and note open design items you brushed up
against (e.g. `next/docs/source-lifecycle-defer-to-start.md`,
`next/docs/runtime-ownership.md`).

## Invariants

These rules apply to every step below.

### No locks on the graph execution path

`Mutex` / `RwLock` must not be locked inside anything the engine runs per
cycle: an `Op::cycle`/`start`/`stop`/`teardown`, a `for_each`/`map` closure, or
a `poll` closure. Graph execution is single-threaded; a lock there adds
per-tick overhead and risks blocking the hot path behind a background thread.

Locks *are* acceptable in, and only in:

- **Wiring / factory functions** — `$ARGUMENTS_read(&g, ...)`, trait methods
  that open files/sockets. These run once, before the graph starts.
- **Background threads** — a subscriber thread you spawn that feeds a
  [`ChannelSender`], or the tokio task behind `produce_async`.
- **Cross-thread handles handed to user code** — an injector/publisher handle
  called from outside the graph.

Graph-thread-local mutability (a `csv::Writer`, a `BufReader`, a reconnect
counter) goes behind `RefCell`/`Rc<RefCell<...>>` — single-threaded interior
mutability, no lock (see the sinks in `lines.rs`/`csv.rs` and the `tail_lines`
poll state). To communicate between a background thread and the graph, use the
channel layer (`g.channel()` + `ChannelSender`) or `produce_async` — never a
shared `Mutex<T>` read from a closure.

When the hand-off is the *other* direction — the graph publishes a whole
current value that a background thread reads ad-hoc (not a stream of deltas the
graph consumes) — use `arc_swap::ArcSwap<T>` for a lock-free atomic pointer
swap. The sink `for_each` calls `slot.store(Arc::new(v))` per cycle (no lock on
the graph path) and the background thread `.load()`s the latest off-thread.
This is exactly the **pull-based exporter** shape (a Prometheus `/metrics`
scraper thread reading per-metric slots): see the per-slot pattern in classic
`wingfoil/src/adapters/prometheus/exporter.rs`, ported to next's
`adapters::prometheus`.

### Historical replay must be deterministic

Any source that replays historical data over the [`channel`] must:

- stamp every record with [`ChannelSender::send_at`] using **non-decreasing**
  timestamps (the historical receiver rejects out-of-order sends — sort or
  reject before sending, and turn index overflow into a clear error the way
  `replay_lines_scheduled` does with `u32::try_from`);
- deliver timestamps **at or after the run's start time**, and document that
  callers run with `RunMode::HistoricalFrom(t)` at or before the first record;
- finish with [`ChannelSender::close`] so the receiver stops collecting;
- propagate malformed input with [`ChannelSender::send_error`] — aborting the
  run with context — never `panic!` and never a silent skip.

Same-instant records must ride **one atomic `Burst`** — the channel receiver
does this grouping for you; do not pre-flatten or coalesce.

If the adapter replays a **caller-parameterised time range** from a service
(time-sliced queries, cursor replays — the classic `kdb_read`/`postgres_read`
shape), do not hand-roll window clamping or slicing: port the classic
`wingfoil/src/adapters/common.rs` helpers (`WindowFilter`,
`compute_validated_time_slices`) into a shared `adapters/common.rs` first, and
build on those. First such adapter pays the porting cost; every later one
reuses it.

**If the slicer already exists** (postgres ported it in Phase 4), you are the
*reusing* adapter: **do not duplicate it — widen its cfg gate.** postgres left
the helpers gated `#[cfg(feature = "postgres")]` (a `kdb`/your-feature cfg
didn't exist yet, and an unknown-feature cfg trips `unexpected_cfgs`), so change
that to `#[cfg(any(feature = "postgres", feature = "$ARGUMENTS"))]` and call the
same functions. The `WindowFilter` row-clamp is always-compiled; only the
*slicer* is feature-gated. postgres's reader (query every slice at wiring →
clamp each row through `WindowFilter` → feed the finite rows into
`replay_results`) is the template for yours.

### Live sources are realtime-only — reject historical at wiring

A **live, never-closing source** (a subscription / watch / consumer that
streams until the service disconnects — `etcd_sub`, `redis_sub`, `kafka_sub`)
must **reject `RunMode::HistoricalFrom` at wiring time** with a clear,
adapter-named error, and return `Result`. It cannot replay historically: the
historical channel receiver block-collects the whole stream up front, so an
unbounded live producer deadlocks the graph at `start` (this is the etcd bug
the port fixed). Only a *finite, timestamped* source (`replay_results` over
file/query rows) runs historically. State this in the module docs, and read the
run mode from the `RunParams` the factory already takes.

### Fallibility, with context

- **Wiring-time I/O** (open file, bind socket, connect) happens in the factory
  function, which returns `anyhow::Result<Stream<...>>` — errors surface at
  wiring, before the run (see `csv_read`, `sink_lines`). **Exception — a live
  socket/subscription source's connect + thread spawn belongs in `start()`, not
  the factory**: wire it with [`source_at_start`] so wiring stays pure (parse /
  registry lookup / historical rejection only) and the connect+spawn happen in
  the `setup` closure at run start (see step 7). The factory still returns
  `Result` for the pure validation; a *connection* error then surfaces at
  run-start with node context (classic-consistent) rather than at wiring. `zmq_sub`
  is the reference.
- **Run-time errors** flow through the fallible cycle: `for_each`/`try_map`
  closures return `Result`, custom `Op` lifecycle functions return `Result`,
  producer threads use `send_error`. Attach `.context("...")`/`.with_context`
  at every I/O boundary, naming the adapter and the resource
  (`"$ARGUMENTS: opening {path}"`).
- No `.unwrap()` outside `#[cfg(test)]` and doc examples (repo-wide rule).
- A closed receiver is a **normal teardown race**, not an error: `send`/`send_at`
  return `false` once the graph is gone — exit the producer loop quietly.
- **Never leak credentials into error context.** A connection string / DSN /
  URL often embeds a password or token (`password=...`, `redis://user:pass@…`),
  and `.context("connecting to {conn}")` would spill it into logs and the
  graph-abort error. Give the connection config a `redacted()` method that masks
  the secret (`password=***`) and use it at **every** `connect()` error site;
  assert in a no-service test that the raw secret never appears. (This is the
  classic postgres password-redaction fix — reproduce it for any networked
  adapter with credentials: redis, kafka, zmq, kdb, …)

### Layering: extension traits, out of the prelude

- **Sources** are free functions taking `&GraphBuilder` first:
  `$ARGUMENTS_read(&g, ...)` / `$ARGUMENTS_sub(&g, ...)`.
- **Sinks** are an extension trait on `Stream<Burst<T>>` so `use`ing it
  enables chaining: `trait <Name>SinkOps { fn $ARGUMENTS_write(&self, ...) }`.
- **Compute ops** are an extension trait on the relevant `Stream<T>`.
- Nothing goes in the [`prelude`] — users opt in per adapter with
  `use wingfoil_next::adapters::$ARGUMENTS::...;`, mirroring `stats`.
- Third-party-reachable wiring only: implement traits via [`Stream::wire`] /
  [`GraphBuilder::source`], the same primitives external crates get.

**Naming:** keep the classic verb conventions —

| Pattern | Verbs | Precedent |
|---------|-------|-----------|
| Pub/sub or event streaming | `_sub` / `_pub` | classic etcd, zmq |
| Batch/file replay | `_read` / `_write` | csv (`csv_read`/`csv_write`) |
| File tail / live follow | `replay_*` / `tail_*` | lines |
| Push-only telemetry | `_pub` / `_push` sink trait method | otlp |
| Pull-based exporter | `_gauge`/`_counter` sink trait method + an exporter handle | prometheus |
| Pure compute | domain verb on a trait | `augurs_forecast`, `ewma` |

Use `impl Into<Config>` + `From` impls for any parameter callers might supply
in several shapes (bare `&str` endpoint, prebuilt config struct, tuple) — one
signature, several natural call sites (see `AugursForecastConfig`'s
`From<(usize, usize)>`).

## 1. Branch

**All next work cuts from and merges into `next`, never `main`** (see
`next/CLAUDE.md`). Cut the feature branch from `next`:

```bash
git checkout next && git pull origin next && git checkout -b $ARGUMENTS-next
```

When you open the PR, its **base branch must be `next`** — not `main`. Only the
eventual next→main cutover PRs target `main`.

## 2. Choose the adapter shape

Pick the source/sink machinery from the I/O library's nature — this is the
load-bearing decision:

| Library / data shape | Source | Sink | Reference |
|---|---|---|---|
| File / batch replay (historical) | read fully at wiring → `send_at` per record → `close` | `for_each` + `RefCell` writer | `csv.rs`, `lines.rs` |
| Synchronous streaming client (blocking recv) | `source_at_start`: background `std::thread` feeding a `ChannelSender`, connected+spawned at graph `start()` (realtime) | `for_each` pushing into an `mpsc` drained by a writer thread | `zmq.rs`, pattern below |
| Async client library (tokio-based) | `produce_async` / `produce_async_bounded` (`async` feature) | writer task + `for_each` (as above, tokio flavour) | `async_source.rs` |
| Non-blocking poll, ultra-low latency | `g.poll(...)` busy-spin (realtime only) | non-blocking write in `for_each` | `tail_lines` |
| Push-only telemetry (no source) | n/a | `for_each` pushing each burst to the exporter/collector client | otlp (classic), step 8 |
| Pull-based exporter (scraped, no source) | n/a | `for_each` → `ArcSwap` slot read by a background HTTP thread | prometheus, step 8 |
| Pure compute (no external service) | n/a — transform ops | n/a | `augurs.rs`, step 9 |

An adapter may offer **multiple strategies behind a mode enum** (the classic
`FixPollMode`/`Iceoryx2Mode` pattern): a `#[derive(Debug, Clone, Default)]
pub enum <Name>Mode { #[default] Threaded, Spin }` selected at the factory,
returning the same `Result<Stream<Burst<T>>>` either way. Document the
latency/CPU trade-off on each variant. Note the engine-level constraints:
`poll` and `external`-fed sources are **realtime-only**; the `channel` source
works in **both** modes (that's what makes replay adapters deterministic).

**Realtime-only sinks** (exporters, servers, push telemetry — anything whose
side effect only makes sense against a live clock) should **no-op under
historical replay** so a backtest that happens to include the sink stays inert
and deterministic. A `for_each`/`register_op1` sink reads the run mode from its
`Ctx` — `ctx.run_mode()` — and returns early when it isn't `RunMode::RealTime`
(inside an island the ctx reports `RealTime`, consistent with `is_last_cycle`).
This mirrors classic's `state.run_mode()` guard in spin-mode adapters.

## 3. Feature flags — `next/crates/wingfoil-next/Cargo.toml`

Adapters with dependencies are feature-gated so the default build stays
dependency-free (the `csv`/`augurs` precedent):

```toml
[features]
$ARGUMENTS = ["dep:some-client-crate"]            # + "async" if built on produce_async
$ARGUMENTS-integration-test = ["$ARGUMENTS", "dep:testcontainers"]

[dependencies]
some-client-crate = { version = "x.y", optional = true }
testcontainers = { version = "0.27", features = ["blocking"], optional = true }
```

- Pin versions to whatever the **classic** adapter uses, so the two trees
  don't drift (see how next's `csv`/`augurs` deps mirror classic's).
- `testcontainers` goes in `[dependencies]` as optional (feature flags cannot
  gate dev-deps). Skip the `-integration-test` flag entirely for file-based
  and pure-compute adapters (Option C in step 10).
- A dependency-free adapter (like `lines`) needs no feature at all.

**The `dependency-review` gate — expect it, and prefer rolling forward.** CI's
`dependency-review` job (`.github/workflows/security-audit.yml`, fails on
`moderate`+) flags a **newly added** dependency that carries a known advisory —
**even if the classic `wingfoil` crate already ships that exact version**,
because it's new *to this PR's diff*. So pinning to classic's version can still
turn the gate red. Two fixes, in order of preference:
1. **Roll the dependency forward** to a fixed version if one exists, and note the
   deliberate divergence from classic in the dep's Cargo.toml comment (then bump
   classic to match in a follow-up, to restore lockstep). This is the real fix —
   the advisory is gone, not suppressed. (otlp did this: opentelemetry 0.28→0.32
   for GHSA-w9wp-h8wv-79jx.)
2. If you genuinely can't roll forward, **allowlist the specific advisory** with
   `allow-ghsas: GHSA-…` in the workflow and a comment explaining *why it's safe*
   (e.g. classic already ships it; the vulnerable code path is unused). A
   last resort, not the default.
Run `cargo audit` too (a separate CI job) — it catches advisories
`dependency-review` may not, and vice-versa.

**Pluggable backends behind their own feature.** If the adapter can swap an
underlying library for the *same* concern — a discovery backend, a TLS
provider, an alternative codec — gate each behind its own feature and select
with `#[cfg(feature = "...")]`, exposing a trait for the pluggable concern:

```toml
[features]
$ARGUMENTS = ["dep:primary-client"]
$ARGUMENTS-alt-backend = ["$ARGUMENTS", "dep:alt-backend-crate"]
```

```rust
pub trait <Name>Backend: Send + 'static { /* ... */ }

#[cfg(feature = "$ARGUMENTS-alt-backend")]
impl <Name>Backend for AltBackend { /* ... */ }
```

This is the classic zmq pattern — `zmq` works standalone for direct TCP
addresses, but with the `etcd` feature also enabled `EtcdRegistry` becomes a
`ZmqRegistry` discovery backend. Model the choice as an `impl Into<Config>`
wrapper with `From` impls (bare address vs `(service, backend)`), not an
`Option`, so one factory signature serves every call-site shape.

## 4. Module registration — `src/adapters/mod.rs`

Two edits, not one:

1. the gated module declaration, alphabetically ordered:
   ```rust
   #[cfg(feature = "$ARGUMENTS")]
   pub mod $ARGUMENTS;
   ```
2. a bullet in the module's `//!` doc list — one line saying what the adapter
   is, which direction(s) it covers, and its feature gate, matching the
   existing `lines`/`csv`/`augurs` bullets. This doc list is the adapters
   index for the crate; keep it complete.

## 5. File structure

- **Single file** `src/adapters/$ARGUMENTS.rs` while the adapter fits in one
  (all three existing adapters do). Order the file: module docs → shared
  helpers → value/config types → source(s) → sink trait + impl →
  `#[cfg(test)] mod tests` for pure helper functions.
- **Directory** `src/adapters/$ARGUMENTS/` (`mod.rs`, `read.rs`, `write.rs`)
  once it outgrows one file — e.g. a stateful session protocol. Keep the
  public surface re-exported from `mod.rs`.
- Tests live in `tests/` (step 10), not inline — inline `mod tests` is for
  pure helper functions only (`poll_line`, `transpose_window` precedents).

## 6. Module docs — the `//!` header

Every adapter's module docs follow the established shape (compare `lines.rs`,
`csv.rs`, `augurs.rs` — keep the section names):

```rust
//! $ARGUMENTS adapter — <one-line description>. <If porting: "It ports the
//! classic `wingfoil::adapters::$ARGUMENTS` module onto the Op model.">
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`stats`](crate::stats)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude). Bring in
//! what you need explicitly:
//!
//! - **Source** — <free function name + one line>.
//! - **Sink** — <trait name + one line>.
//!
//! # <Source semantics — e.g. "Historical replay (the burst model)">
//!
//! <How records map onto graph time; burst grouping; determinism caveats;
//! deviations from classic, called out explicitly.>
//!
//! # Sink
//!
//! <What each cycle writes/flushes; how errors propagate.>
//!
//! # Setup            <!-- only for service-backed adapters -->
//!
//! ```sh
//! docker run --rm -p PORT:PORT <image>:<tag>
//! ```
```

Doc every public item, including `# Errors` sections on fallible factories
(rustdoc lint expects them; `csv_read` is the template).

## 7. Sources

### Channel replay (file / batch / query results — both run modes)

**Use the `GraphBuilder::replay_results` primitive** — don't hand-roll the
`channel` → `send_at` loop → `close` bookkeeping. It queues a finite
`Result<(value, time)>` sequence onto a channel source, forwards a decode error
via `send_error` (then stops), and closes:

```rust
pub fn $ARGUMENTS_read<T>(g: &GraphBuilder, /* params */) -> Result<Stream<Burst<T>>>
where
    T: Clone + Default + 'static,
{
    // open + read the input at wiring time (an open error -> Err, before the run)
    let rows = read_rows(/* … */)?;   // Iterator<Item = Result<(T, NanoTime)>>, non-decreasing times
    Ok(g.replay_results(rows))
}
```

Non-decreasing timestamps are still your responsibility (sort or reject before
yielding; turn index overflow into a clear error, as `csv_read`/`replay_lines`
do). Under the hood `replay_results` is exactly the loop below — shown only so
you know what it does; call the primitive, don't copy it:

```rust
let (stream, sender) = g.channel::<T>();
for row in rows {
    match row {
        Ok((rec, t)) => { sender.send_at(rec, t); }                // non-decreasing!
        Err(e) => { sender.send_error(e.context("...")); break; }
    }
}
sender.close();   // the historical receiver needs the end-of-stream
```

### Background thread over the channel (sync streaming client — realtime)

**Use [`source_at_start`], not `g.channel()` + a wiring-time spawn.** A live
source's socket connect and background thread belong in `start()`, not the
factory: `source_at_start` allocates the channel at wiring but runs your `setup`
closure — which connects and spawns the feeder thread — at graph `start()`, and
returns the running producer as a [`StopHandle`] dropped at teardown to stop it
(the generalised `ThreadStopGuard`). That keeps wiring side-effect-free and
unit-testable (no live socket to construct the graph), and surfaces a connection
error at run-start with node context — classic-consistent. `zmq_sub` is the
reference.

```rust
pub fn $ARGUMENTS_sub<T>(g: &GraphBuilder, run_mode: RunMode, conn: impl Into<<Name>Config>)
    -> Result<Stream<Burst<T>>>
where
    T: Clone + Default + Send + 'static,
{
    // Wiring is PURE: reject historical (a live source is realtime-only, see the
    // invariant), resolve/validate config. No socket, no thread here.
    if let RunMode::HistoricalFrom(_) = run_mode {
        anyhow::bail!("$ARGUMENTS_sub: RunMode::HistoricalFrom is unsupported — run realtime");
    }
    let cfg = conn.into().resolve()?;                  // parse / registry lookup — Err before the run

    // Deferred to start(): connect + spawn the feeder. `setup` is handed a fresh
    // ChannelSender each run; whatever it returns is dropped at teardown.
    Ok(g.source_at_start::<T, _>(move |sender| {
        let cfg = cfg.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_stop = stop.clone();
        std::thread::Builder::new()
            .name("$ARGUMENTS-sub".into())
            .spawn(move || {
                let client = match connect(&cfg) {     // connect on the thread → error at run-start
                    Ok(c) => c,
                    Err(e) => { sender.send_error(e.context("$ARGUMENTS: connecting")); return; }
                };
                loop {
                    if thread_stop.load(std::sync::atomic::Ordering::Relaxed) { return; }
                    match client.recv() {
                        // `send` returns false once the receiver is gone —
                        // a normal teardown race; stop producing.
                        Ok(msg) => if !sender.send(msg) { return; },
                        Err(e) if is_end_of_stream(&e) => { sender.close(); return; }
                        Err(e) => { sender.send_error(anyhow::Error::new(e).context("...")); return; }
                    }
                }
            })
            .context("$ARGUMENTS: spawning subscriber thread")?;
        // Its Drop flips `stop`, so the thread exits on its next loop turn.
        Ok(StopHandle::new(ThreadStopGuard(stop)))
    }))
}
```

Each `send` wakes the kernel; values arriving between cycles group into one
`Burst`. Realtime only — do not offer this path for historical runs.
`source_at_start` builds on `channel`, so a graph containing it is **single-run**
for now (the receive channel + waker are consumed by the first run), same as a
plain `channel` source. `StopHandle`/`source_at_start` are `use
wingfoil_next::interp::{...}` / the `SourceOps` trait; define `ThreadStopGuard`
as a tiny `Drop` that sets the stop flag (the zmq adapter's is the template).

> **Not on `source_at_start` yet:** the `produce_async` and plain
> `channel`/`external` source shapes still connect/spawn at wiring. If your
> adapter uses those, follow their sections as written; migrating them to
> deferred establishment is tracked in
> `next/docs/source-lifecycle-defer-to-start.md`.

### `produce_async` (async client library — `async` feature)

```rust
pub fn $ARGUMENTS_sub(
    g: &GraphBuilder,
    params: RunParams,
    conn: <Name>Config,
) -> anyhow::Result<Stream<Burst<<Name>Event>>> {
    produce_async(g, params, move |_p| async move {
        // connect; optionally snapshot-then-watch (see below)
        Ok(async_stream::stream! {
            // yield Ok((NanoTime, event)); yield Err(e) to abort the run
        })
    }) // returns Result — propagate with `?` (runtime creation is fallible)
}
```

Know the caveats (documented in `async_source.rs`): the producer task spawns
at **wiring** time, so `params` must describe the run the caller will actually
invoke — historical `start_time` mismatches are validated and abort the run;
use `produce_async_bounded` when a fast realtime producer needs `buffer_size`
back-pressure (not applied in historical mode, by design).

**Runtime ownership — the graph owns the runtime; pass no `&Handle`.** The
`GraphBuilder` owns one tokio runtime, created lazily on first async use and
dropped at teardown, shared by every async adapter in the graph
(`next/docs/runtime-ownership.md`, landed). So your factory takes **no**
`&tokio::runtime::Handle`: `produce_async` / `produce_async_bounded` /
`consume_async` pull the handle from `g` themselves and return `Result` (the
first, owned-runtime creation is the only fallible part — propagate with `?`).
For a **sink trait** (method on `Stream<…>`, no `&g` in hand), get a builder
view with `self.graph()` and, if you need the handle directly for a wiring-time
`block_on` connect, `self.graph().async_runtime_handle()?`. The graph must still
be built, run, and dropped from a **non-async thread** (the `block_on` footgun —
see the etcd/postgres module docs). A caller embeds their own runtime with
`GraphBuilder::new().with_async_runtime(handle)` (the override). `RunParams` is
still a source factory param (the producer spawns at wiring); it will fall away
only if/when the `produce_async` family also defers to `start()`
(`next/docs/source-lifecycle-defer-to-start.md`).

If the service supports **snapshot + watch** (etcd-like), use watch-before-get
to avoid races: open the watch first, read the snapshot and its
revision/cursor, emit snapshot events, then emit watch events skipping any at
or below the snapshot revision.

### Busy-spin `poll` (non-blocking I/O, ultra-low latency — realtime)

```rust
let state = Rc::new(RefCell::new(/* non-blocking handle + parse buffer */));
Ok(g.poll(move || {
    let s = &mut *state.borrow_mut();
    // non-blocking read; WouldBlock => None (quiet cycle);
    // reassemble records that straddle polls (see `poll_line` in lines.rs);
    // return Some(Burst::from([record])) when one completes
}))
```

The kernel never parks while a `poll` source exists — cycles run back-to-back
(~µs latency, one core pinned). Offer it as the `Spin` variant of a mode enum
rather than the only option, unless the adapter's whole point is latency.
Factor record-reassembly logic into a free function so it is unit-testable
without a realtime run (`poll_line` precedent).

## 8. Sinks

### Synchronous writer (files, blocking clients with cheap writes)

**Use the `StreamOps::for_each_mut` primitive** for a sink that owns a mutable
resource — don't hand-roll the `RefCell`-wrap-for-a-`Fn`-closure dance.
`for_each_mut(writer, |w, v| …)` wraps the writer, runs the closure per tick
with `&mut` access, and aborts the run on an `Err`:

```rust
pub trait <Name>SinkOps<T> {
    /// <what it writes, truncate-vs-append, header behaviour>. Returns the
    /// sink `Stream<()>`, or an error if <resource> cannot be opened.
    fn $ARGUMENTS_write(&self, /* params */) -> Result<Stream<()>>;
}

impl<T> <Name>SinkOps<T> for Stream<Burst<T>>
where
    T: /* Serialize / Display */ + Clone + Default + 'static,
{
    fn $ARGUMENTS_write(&self, /* params */) -> Result<Stream<()>> {
        let writer = open_at_wiring_time()?;          // Err before the run
        Ok(self.for_each_mut(writer, move |w, burst: &Burst<T>| {
            for record in burst.iter() {
                w.write(record).with_context(|| format!("$ARGUMENTS: writing ..."))?;
            }
            w.flush().context("$ARGUMENTS: flushing")?;
            Ok(())
        }))
    }
}
```

Need the graph time per row? Chain `with_time()` before `for_each` and take
`(NanoTime, Burst<T>)` (the `csv_write` pattern).

For ergonomics, offer a **single-value convenience impl** alongside the
`Stream<Burst<T>>` one, so callers with a plain `Stream<T>` don't wrap manually
(`impl <Name>SinkOps for Stream<T> { … self.map(|v| burst![v]).$ARGUMENTS_write() }`
— csv and etcd both do this). **Caveat:** skip it when the element type's own
trait bound (`Display`/`Serialize`) is *also* satisfied by `Burst<T>` itself —
`Burst<T>` is a `tinyvec` that implements `Display`, so a `Stream<Burst<T>>`
*is* a `Stream<T: Display>` and the two impls become ambiguous (E0283) or, as an
inherent method, silently shadow the burst form (writing `[ALPHA]` instead of
`ALPHA`). That is why `lines` stays burst-only while `csv` can offer both.

### Threaded / async writer (async clients, slow/blocking writes)

**Async client → use the `consume_async` primitive** (the sink mirror of
`produce_async`, `async` feature). It hands each burst's values to a background
tokio task over a **bounded** channel (`buffer_size` back-pressure), drains with
a **single** consumer task so write **order is preserved**, propagates write
errors back into the graph to abort the run on the next cycle, and flushes
queued writes at teardown — all off the graph thread. Wire it via `for_each`:

```rust
// `consume_async` takes the graph (not a `&Handle`) and returns `Result`; from a
// sink trait method use `&self.graph()`. Propagate with `?`.
let sink = consume_async(&self.graph(), Some(buffer_size), move |value| async move {
    client.write(value).await.context("$ARGUMENTS: writing")
})?;
Ok(self.for_each(sink))
```

This is how a networked sink avoids `handle.block_on(...)` inside `cycle`
(which stalls the single-threaded engine on I/O every burst). Note the one
place it can't help: an op that must return an `Err` *synchronously* within the
firing cycle (e.g. a conditional write under `RunFor::Cycles(1)`) — an
off-thread write reports failure only after the cycle, so keep those on a
blocking path and document why.

**Sync client, slow/blocking writes** → keep the graph thread non-blocking:
`for_each` pushes each burst into an `std::sync::mpsc` drained by a writer
thread spawned at wiring time. Propagate writer failures by having the
background writer park the error in a shared slot (`Arc<Mutex<Option<Error>>>` —
the lock is touched on the *error* path and by the background thread, and
`for_each` does a cheap `Arc<AtomicBool>` check per cycle before taking it), so
the **next** cycle aborts the run with context rather than the error vanishing.
On `stop` semantics: dropping the sender at teardown ends the writer loop; if
the protocol needs an explicit end-of-stream message, send it from the drain
thread when the channel closes.

### Server / exporter sink (scraped or push telemetry)

An exporter (Prometheus) or push-telemetry (OTLP) sink has no data *source* —
it's a `Stream<Burst<T>> → Stream<()>` that publishes the graph's current
values outward. The shape:

- An **exporter handle** built at wiring time (`PrometheusExporter` = registry
  + a hand-rolled `GET /metrics` HTTP server on a background thread; an OTLP
  collector client). Binding the socket / connecting happens here and returns
  `Result`, so failure surfaces before the run.
- A **sink trait** on `Stream<Burst<T>>` (`prometheus_gauge` / `otlp_push`)
  wired over `for_each`/`register_op1`. Each cycle it publishes the current
  value: for a *scraped* exporter, `slot.store(Arc::new(v))` into a per-metric
  `ArcSwap` the server thread reads (no lock, per the invariant above); for
  *push* telemetry, hand the burst to the collector client (or an mpsc drained
  by a sender thread if the export blocks).
- **Realtime-only**: guard on `ctx.run_mode()` and no-op under historical
  replay (see step 2) — a backtest that includes the sink stays inert.

Reference: classic `wingfoil/src/adapters/{prometheus,otlp}/`, ported to next's
`adapters::prometheus`. Both are single-direction, so there is no source
function and no `_read`/`_sub`.

## 8a. Optional: on-graph status / lifecycle streams

Adapters with a connection lifecycle (connect / disconnect / back-pressure /
close) can expose that state as a **first-class stream** alongside the data
stream, so downstream ops react to transport health on-graph — circuit
breakers, health gates, reconnect metrics — without reaching outside the graph.
Classic's Aeron adapter is the reference (`status.rs`, `status_stream.rs`).

Build it as a **parallel-additive sibling** — never change the primary
factory's signature:

1. **Status enum** — a small `#[non_exhaustive]` enum with a `#[default]`
   "disconnected" variant, `Copy` so transitions forward cheaply
   (`Connected` / `Disconnected` / `BackPressured` / `Closed`).
2. **Transition-only emission** — the status source emits **only when the value
   changes** (dedup against the last), so a downstream gate ticks on real
   transitions, not every cycle.
3. **`*_with_status` factory** — returns a tuple `(data, status)` rather than
   overloading the primary factory:
   ```rust
   pub fn $ARGUMENTS_sub_with_status(g: &GraphBuilder, /* … */)
       -> Result<(Stream<Burst<T>>, Stream<<Name>Status>)>
   ```
   Record the new status **after** a successful poll/offer (in a fixed order,
   terminal states first) so a transient I/O error doesn't register a phantom
   transition.
4. **Threaded mode** — if the sub runs a background thread, multiplex status
   transitions **in-band** with data over the one `channel` (an
   `enum Item { Data(T), Status(S) }`), so a `Connected` transition stays
   correctly ordered before the fragments that followed it; the data node
   splits the two back out.

In next this rides the existing vocabulary — the status stream is just another
`channel`/`poll` source the producer also feeds — so no engine change is
needed. Port the classic behaviour (transition-only, post-success recording)
exactly; it's the parity oracle.

## 9. Pure-compute adapters (custom `Op`s)

For a compute library (forecasting, analytics, codecs) there is no I/O edge —
the adapter is **transform ops**, the same shape as `stats`:

1. Define the op as a unit struct + `impl Op` with `#[op(build = name)]`:
   `Cfg` = resolved config (validate/floor user config at wiring time into a
   `<Name>Cfg`), `State` = the sliding window / model state (`Default`),
   `In<'a> = (&'a I,)`, `ACTIVATION = Activation::NONE`. The attribute
   generates the interpreted `Builder::name` method **and** the forwarders
   that make the op usable inside `graph!`/`compiled()` — no macro edits.
2. Return `Tick::Quiet` during warm-up (window not full), `Tick::Value(out)`
   after; heavy refits every tick are the *caller's* choice — document
   "throttle upstream if you don't need a fresh fit per tick".
3. Expose a fluent extension trait whose method resolves the config and calls
   `self.wire(|b, h| b.name(h, cfg))`.
4. Validate config **inside `cycle`** with `anyhow::bail!` (clear message,
   aborts the run) when validation needs runtime info; validate at wiring
   when it doesn't. Never panic at wiring time for bad user config.
5. Multi-input, passive-edge, or lifecycle-hook ops don't fit `#[op]`'s
   single-input scope — see "Adding an op" in `next/docs/port-plan.md` for
   the hand-written `Builder`-method route before inventing anything.

`augurs.rs` demonstrates all five, including non-`Send + Sync` error mapping
(`map_err(|e| anyhow::anyhow!(...))` when a library error can't flow through
`Context`).

## 10. Tests — `tests/$ARGUMENTS_adapter.rs`

File-level gate: `#![cfg(feature = "$ARGUMENTS")]` (omit if ungated).
Integration tests needing a live service go in a separate
`tests/$ARGUMENTS_integration.rs` with
`#![cfg(feature = "$ARGUMENTS-integration-test")]`.

Conventions (see `tests/lines_adapter.rs` / `tests/csv_adapter.rs`):

- **Historical determinism**: run with
  `RunMode::HistoricalFrom(NanoTime::ZERO)`; assert exact values **and tick
  times** via `.with_time().accumulate()` and `runner.value(&stream)`.
- **Unique temp paths** per test (pid + atomic counter) so parallel tests
  never collide.
- **Parity first**: port every classic adapter unit test, then add
  next-specific ones (burst grouping, `send_error` propagation, wiring-time
  `Err` on a missing resource).

Test order for a service-backed adapter (connection-refused first — needs no
container):

1. `test_connection_refused` — wiring or first-cycle error propagates with
   context.
2. `test_sub_snapshot` — pre-seeded data appears.
3. `test_sub_live_updates` — events arrive after the snapshot.
4. `test_pub_round_trip` — sink writes; verify via a direct client read.
5. `test_sub_no_race` — a write during the snapshot→watch handoff is neither
   missed nor duplicated (if applicable).
6. `test_delete_events` — tombstones handled (if applicable).

Container infrastructure — choose one:

- **Option A — testcontainers** (preferred for open-source services). Use the
  blocking `SyncRunner` so startup stays in a plain `#[test]`:
  ```rust
  use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};
  let container = GenericImage::new("vendor/image", "tag")
      .with_wait_for(WaitFor::message_on_stderr("ready"))
      .start()?;
  let port = container.get_host_port_ipv4(DEFAULT_PORT)?;
  ```
  Hold the container binding for the test's duration; it stops on drop.
- **Option B — external service with skip-if-unavailable**: for licensed or
  un-containerisable services, probe (`TcpStream::connect`) and
  `eprintln!("skipping ...")` + return early. Document manual setup in the
  module docs and example README.
- **Option C — no service**: file-based and pure-compute adapters need
  neither the `-integration-test` feature nor containers; fixture-file tests
  are the integration tests.

## 11. Example — `examples/`

- Single file `examples/$ARGUMENTS_adapter.rs` for a simple demonstration
  (the `csv_adapter`/`lines_adapter` precedent), or a directory
  `examples/$ARGUMENTS/{main.rs,README.md}` for a realistic end-to-end story
  (the `order_book` precedent). If the classic tree has an example for this
  adapter, port it — same scenario, same output.
- Top with a `//!` doc comment including the exact run command.
- Register in `next/crates/wingfoil-next/Cargo.toml`:
  ```toml
  [[example]]
  name = "$ARGUMENTS_adapter"          # add `path = ...` for the directory form
  required-features = ["$ARGUMENTS"]
  ```
- Directory-form README follows the classic pattern: title, one paragraph,
  `## Setup` (docker one-liner, if any), `## Run` (cargo command), `## Code`,
  `## Output`.
- If `next/README.md` or the crate docs grow an adapters index table by the
  time you land, add a row; today the canonical index is the
  `src/adapters/mod.rs` doc list from step 4.

### Optional: benchmarks (low-latency adapters)

For a latency- or throughput-sensitive adapter (a poll/spin source, an IPC
transport), add a Criterion suite under `next/crates/wingfoil-next/benches/`
and register it with `harness = false` + `required-features = ["$ARGUMENTS"]`.
Skip it when throughput is bounded by the remote service rather than the
adapter glue — benches only earn their keep where the adapter itself is on the
hot path.

## 12. CI — only for service-backed adapters

If (and only if) the adapter has `-integration-test` tests, wire them into the
existing hub exactly as the classic adapters do:

1. Create `.github/workflows/$ARGUMENTS-next-integration.yml` following the
   etcd workflow's shape (`workflow_call` + `workflow_dispatch` + `push` with
   `paths: ['next/crates/wingfoil-next/src/adapters/$ARGUMENTS**']`), with
   the test step:
   ```yaml
   - name: Run $ARGUMENTS (next) integration tests
     run: |
       cargo test --features $ARGUMENTS-integration-test -p wingfoil-next \
         -- --test-threads=1 --nocapture
   ```
2. Register it as a job in `.github/workflows/integration-tests.yml`
   (`uses: ./.github/workflows/$ARGUMENTS-next-integration.yml`,
   `secrets: inherit`). Do **not** add it to `release.yml` directly.

### Exposing the adapter to Python — `#[pyadapter]`

`wingfoil-next-python` is now the go-forward Python binding (it **supersedes**
the legacy `wingfoil-python`; see `next/docs/python-interop.md`). A next adapter
reaches Python through the `#[pyadapter]` proc macro — values erase to
`PyElement` at the boundary, the adapter's interior stays natively typed:

- **Source** — `#[pyadapter(name = $ARGUMENTS_read, source)]` on
  `impl <Name>SourceOps for GraphBuilder { fn $ARGUMENTS_read(&self, args…) ->
  Stream<T> }` emits `wingfoil_next.$ARGUMENTS_read(graph, args…) -> Stream`.
- **Sink** — `#[pyadapter(name = $ARGUMENTS_write)]` (no `source` marker) on
  `impl <Name>SinkOps for Stream<T> { fn $ARGUMENTS_write(&self, args…) ->
  Stream<()> }` emits `wingfoil_next.$ARGUMENTS_write(stream, args…)`.

Register the generated `#[pyfunction]` in the `wingfoil_next` `#[pymodule]` and
add a pytest case in `crates/wingfoil-next-python/tests/test_interop.py`; the
`ramp_source` (source) and `list_sink` (sink) demos in `src/python.rs` are the
templates, and `tests/plugin_seam.rs` shows the same from an external crate.

`#[pyadapter]` handles **burst** adapters too — the common shape, since the
layering conventions above make most real adapters `Stream<Burst<T>>`
sources/sinks. A `Stream<Burst<T>>` erases to a Python **`list`** per tick
(same-instant values grouped); on the way *in* a Python `list`/`tuple` rebuilds
a multi-value burst (else a single-element burst), so a burst source
round-trips into a burst sink. `Burst<T>` may appear as a source's return, a
sink's `Self`, or a transform's output. Templates: the `pair_source` (burst
source) and `burst_list_sink` (burst sink) demos in `src/python.rs`, and
`burst_double` in `tests/plugin_seam.rs`.

Constraints: the method's params become `#[pyfunction]` params, so they must be
`FromPyObject` (`i64`/`f64`/`String`/`Py<PyAny>`/… — a Rust-only handle like
`Rc<RefCell<…>>` can't cross); and the value type edge-converts
(`T: TryFrom<&PyElement>` in, `U: Into<PyElement>` out).

So expose your adapter via `#[pyadapter]` (source, sink, or burst) and add the
pytest case in the same PR — service-backed integration bindings follow the
same recipe.

## 13. Superset audit + roadmap bookkeeping

Before the pre-commit checklist, diff against the classic adapter one more
time (skip if none exists):

- every public function/type/config knob → equivalent or documented deviation;
- every classic test → ported parity test (or a comment naming why not);
- classic example → ported example;
- classic `CLAUDE.md` design decisions → carried into the module docs.

Then update `next/docs/port-plan.md`: mark `$ARGUMENTS` in the Phase 4 list
(✅/🟡 with a one-line summary and the test-file name), matching how `csv`
and `augurs` entries read.

## 14. Pre-commit checklist

**Run every command in the FOREGROUND and wait for it to finish. Do NOT
background `cargo lint-all` (or anything else) and move on** — it is slow
(it builds every feature), and backgrounding it then ending the turn is the
single most common way these ports strand with nothing committed. One command
at a time, blocking, until it returns.

```bash
cargo fmt --all
cargo lint                                   # default features
cargo lint-all                               # all features (needs protoc)
cargo test -p wingfoil-next --features $ARGUMENTS
# service-backed adapters only, with the service/container available:
cargo test -p wingfoil-next --features $ARGUMENTS-integration-test -- --test-threads=1
```

All must pass before committing. `cargo lint-all` is what CI runs — it is the
only lint pass that sees your feature-gated code.

**Sandbox caveat:** `cargo lint-all` is a *workspace* all-features build, so it
also compiles the classic **aeron** adapter's C library — which fails to build
in a dev sandbox without the native toolchain (`CMake "Inappropriate ioctl for
device"`), unrelated to your change. When that blocks you, run the scoped
equivalent that still lints every `wingfoil-next` feature/target:

```bash
cargo clippy -p wingfoil-next --all-features --all-targets -- -D warnings
```

That covers all of your adapter's code; the full workspace `lint-all` runs in
CI where aeron's deps are present. Note it in the PR if you substituted.

## 15. Self-review with a fresh context

Before opening a PR, run a clean-context review pass as a subagent (so the
parent context stays clean) with these tasks:

1. **Re-read this skill file end to end**, then walk `git diff next...HEAD`
   against steps 1–14 and produce a checklist: present / missing / diverged.
   Flag every divergence, even intentional ones.
2. **Validate the artifacts exist**: branch cut from `next` and the PR targets
   base `next`, not `main` (step 1); feature flags (step 3); both
   `mod.rs` edits — gate *and* doc bullet (step 4); module docs with the
   Layering section (step 6); factory returns `Result` for wiring-time I/O
   and the trait is out of the prelude (steps 7–8); a realtime-only sink
   (exporter/server/push) guards on `ctx.run_mode()` and no-ops in historical
   replay (steps 2, 8); if the adapter exposes a status stream, it's a
   `*_with_status` tuple factory emitting transition-only, leaving the primary
   signature unchanged (step 8a); tests assert values *and*
   tick times, temp paths unique, correct file-level `cfg` gates (step 10);
   example registered with `required-features` (step 11); CI workflow +
   hub registration for service adapters (step 12); port-plan updated
   (step 13).
3. **Check the invariants**: no `Mutex`/`RwLock` on the graph path (an
   ad-hoc-reader hand-off uses `ArcSwap`, not a lock); channel
   sources send non-decreasing timestamps and `close()`; a live never-closing
   source rejects `RunMode::HistoricalFrom` at wiring (returns `Result`);
   errors carry context; no `.unwrap()` outside tests; producer loops exit
   quietly when `send` returns `false`; nothing added to the prelude.
4. **Check parity**: rerun the step-13 diff against the classic adapter and
   confirm the deviations list in the module docs is complete.
5. **Run the pre-commit checklist from step 14** and confirm every command
   passes. Do not skip any.
6. **Review for quality and simplicity**: no speculative abstractions, no
   dead code, no comments restating the code, no half-finished paths.

Fix everything found before committing. A clean self-review is part of
"done" — not an optional extra.
