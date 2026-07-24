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

### Fallibility, with context

- **Wiring-time I/O** (open file, bind socket, connect) happens in the factory
  function, which returns `anyhow::Result<Stream<...>>` — errors surface at
  wiring, before the run (see `csv_read`, `sink_lines`).
- **Run-time errors** flow through the fallible cycle: `for_each`/`try_map`
  closures return `Result`, custom `Op` lifecycle functions return `Result`,
  producer threads use `send_error`. Attach `.context("...")`/`.with_context`
  at every I/O boundary, naming the adapter and the resource
  (`"$ARGUMENTS: opening {path}"`).
- No `.unwrap()` outside `#[cfg(test)]` and doc examples (repo-wide rule).
- A closed receiver is a **normal teardown race**, not an error: `send`/`send_at`
  return `false` once the graph is gone — exit the producer loop quietly.

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
| Pure compute | domain verb on a trait | `augurs_forecast`, `ewma` |

Use `impl Into<Config>` + `From` impls for any parameter callers might supply
in several shapes (bare `&str` endpoint, prebuilt config struct, tuple) — one
signature, several natural call sites (see `AugursForecastConfig`'s
`From<(usize, usize)>`).

## 1. Branch

```bash
git checkout main && git pull origin main && git checkout -b $ARGUMENTS-next
```

## 2. Choose the adapter shape

Pick the source/sink machinery from the I/O library's nature — this is the
load-bearing decision:

| Library / data shape | Source | Sink | Reference |
|---|---|---|---|
| File / batch replay (historical) | read fully at wiring → `send_at` per record → `close` | `for_each` + `RefCell` writer | `csv.rs`, `lines.rs` |
| Synchronous streaming client (blocking recv) | background `std::thread` feeding a `ChannelSender` (realtime) | `for_each` pushing into an `mpsc` drained by a writer thread | pattern below |
| Async client library (tokio-based) | `produce_async` / `produce_async_bounded` (`async` feature) | writer task + `for_each` (as above, tokio flavour) | `async_source.rs` |
| Non-blocking poll, ultra-low latency | `g.poll(...)` busy-spin (realtime only) | non-blocking write in `for_each` | `tail_lines` |
| Pure compute (no external service) | n/a — transform ops | n/a | `augurs.rs`, step 9 |

An adapter may offer **multiple strategies behind a mode enum** (the classic
`FixPollMode`/`Iceoryx2Mode` pattern): a `#[derive(Debug, Clone, Default)]
pub enum <Name>Mode { #[default] Threaded, Spin }` selected at the factory,
returning the same `Result<Stream<Burst<T>>>` either way. Document the
latency/CPU trade-off on each variant. Note the engine-level constraints:
`poll` and `external`-fed sources are **realtime-only**; the `channel` source
works in **both** modes (that's what makes replay adapters deterministic).

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

```rust
pub fn $ARGUMENTS_read<T, F>(g: &GraphBuilder, /* params */) -> Result<Stream<Burst<T>>>
where
    T: Clone + Default + 'static,
    F: Fn(&T) -> NanoTime,
{
    // 1. open + read the input at wiring time (error -> Err, before the run)
    let (stream, sender) = g.channel::<T>();
    for record in records {
        match record {
            Ok(rec) => { sender.send_at(rec, get_time(&rec)); }     // non-decreasing!
            Err(e) => { sender.send_error(anyhow::Error::new(e).context("...")); break; }
        }
    }
    sender.close();   // the historical receiver needs the end-of-stream
    Ok(stream)
}
```

### Background thread over the channel (sync streaming client — realtime)

```rust
pub fn $ARGUMENTS_sub<T>(g: &GraphBuilder, conn: impl Into<<Name>Config>) -> Result<Stream<Burst<T>>>
where
    T: Clone + Default + Send + 'static,
{
    let client = connect(conn.into())?;               // wiring-time: Err before the run
    let (stream, sender) = g.channel::<T>();
    std::thread::Builder::new()
        .name("$ARGUMENTS-sub".into())
        .spawn(move || {
            loop {
                match client.recv() {
                    Ok(msg) => {
                        // `send` returns false once the receiver is gone —
                        // a normal teardown race; stop producing.
                        if !sender.send(msg) { return; }
                    }
                    Err(e) if is_end_of_stream(&e) => { sender.close(); return; }
                    Err(e) => { sender.send_error(anyhow::Error::new(e).context("...")); return; }
                }
            }
        })
        .context("$ARGUMENTS: spawning subscriber thread")?;
    Ok(stream)
}
```

Each `send` wakes the kernel; values arriving between cycles group into one
`Burst`. Realtime only — do not offer this path for historical runs.

### `produce_async` (async client library — `async` feature)

```rust
pub fn $ARGUMENTS_sub(
    g: &GraphBuilder,
    handle: &tokio::runtime::Handle,
    params: RunParams,
    conn: <Name>Config,
) -> Stream<Burst<<Name>Event>> {
    produce_async(g, handle, params, move |_p| async move {
        // connect; optionally snapshot-then-watch (see below)
        Ok(async_stream::stream! {
            // yield Ok((NanoTime, event)); yield Err(e) to abort the run
        })
    })
}
```

Know the caveats (documented in `async_source.rs`): the producer task spawns
at **wiring** time, so `params` must describe the run the caller will actually
invoke — historical `start_time` mismatches are validated and abort the run;
use `produce_async_bounded` when a fast realtime producer needs `buffer_size`
back-pressure (not applied in historical mode, by design).

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
        let writer = RefCell::new(writer);            // Fn closure needs &mut
        Ok(self.for_each(move |burst: &Burst<T>| {
            let mut w = writer.borrow_mut();
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
`(NanoTime, Burst<T>)` (the `csv_write` pattern). Single-value streams wrap
into bursts at the call site: `stream.map(|v| burst![v.clone()])`.

### Threaded writer (async clients, slow/blocking writes)

Keep the graph thread non-blocking: `for_each` pushes each burst into an
`std::sync::mpsc` (or tokio channel) drained by a writer thread/task spawned
at wiring time. Propagate writer failures by having the background writer park
the error in a shared slot (`Arc<Mutex<Option<anyhow::Error>>>` — the lock is
touched on the *error* path and by the background thread, and `for_each` does
a cheap `Arc<AtomicBool>` check per cycle before taking it), so the **next**
cycle aborts the run with context rather than the error vanishing. On `stop`
semantics: dropping the sender at teardown ends the writer loop; if the
protocol needs an explicit end-of-stream message, send it from the drain
thread when the channel closes.

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

No Python step: `wingfoil-python` binds the **legacy** crate only. Bindings
for next arrive with the facade/cutover phase (`next/docs/port-plan.md`,
Phase 6) — do not add per-adapter bindings now, and say so in the PR
description so reviewers don't flag it as missing.

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

## 15. Self-review with a fresh context

Before opening a PR, run a clean-context review pass as a subagent (so the
parent context stays clean) with these tasks:

1. **Re-read this skill file end to end**, then walk `git diff main...HEAD`
   against steps 1–14 and produce a checklist: present / missing / diverged.
   Flag every divergence, even intentional ones.
2. **Validate the artifacts exist**: feature flags (step 3); both
   `mod.rs` edits — gate *and* doc bullet (step 4); module docs with the
   Layering section (step 6); factory returns `Result` for wiring-time I/O
   and the trait is out of the prelude (steps 7–8); tests assert values *and*
   tick times, temp paths unique, correct file-level `cfg` gates (step 10);
   example registered with `required-features` (step 11); CI workflow +
   hub registration for service adapters (step 12); port-plan updated
   (step 13).
3. **Check the invariants**: no `Mutex`/`RwLock` on the graph path; channel
   sources send non-decreasing timestamps and `close()`; errors carry
   context; no `.unwrap()` outside tests; producer loops exit quietly when
   `send` returns `false`; nothing added to the prelude.
4. **Check parity**: rerun the step-13 diff against the classic adapter and
   confirm the deviations list in the module docs is complete.
5. **Run the pre-commit checklist from step 14** and confirm every command
   passes. Do not skip any.
6. **Review for quality and simplicity**: no speculative abstractions, no
   dead code, no comments restating the code, no half-finished paths.

Fix everything found before committing. A clean self-review is part of
"done" — not an optional extra.
