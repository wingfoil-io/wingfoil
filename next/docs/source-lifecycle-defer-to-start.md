# Deferring I/O source establishment to `start()`

Status: **proposal / not started.** This is a design plan for a handover — it
describes a change to the wingfoil-next *source lifecycle model*, not a bug fix.
Read `port-plan.md` §0.4 (the single-run decision) and §1 (the `reset` hook)
first; this proposal revisits both.

## One-paragraph summary

Today every background I/O source in wingfoil-next (`produce_async`, `external`,
`channel`-fed adapters like `zmq_sub`) spawns its producer thread/task and
connects its socket at **wiring time** — i.e. inside the factory function, while
the graph is still being *constructed*, before `Runner::run`. This proposal is
to **defer the I/O establishment to `start()`** (graph run time) instead, while
keeping pure config validation at wiring. The payoff: wiring becomes
side-effect-free and unit-testable, the lifecycle becomes easier to reason
about (nothing happens until `run()`), and — the big one — I/O sources become
**re-runnable**, closing a real parity gap with classic wingfoil (whose sources
connect in `setup`/`start` and re-run cleanly). The cost is a source-model
engine change plus a rework of every background source, so it should be done
**spike-first**.

## Motivation

### 1. Testability of wiring logic
Constructing the graph currently has real side effects: `zmq_sub(&g, …)` opens a
socket and spawns a thread the moment it's called. You cannot unit-test the
wiring/config logic (address parsing, registry resolution, historical-mode
rejection, graph shape) without triggering live I/O. Deferring I/O to `start()`
makes the factory a pure "register intent" step: assert the graph builds, the
registry resolves, historical mode is rejected — all with zero threads/sockets.

### 2. Easier to reason about
Spawn-at-wiring creates an ambiguous **"wired but not running"** window: the
source is live, its thread runs and messages accumulate, yet no graph cycles
execute. Deferring collapses that window — nothing happens until `run()`;
`run()` starts everything; teardown stops everything. This is classic wingfoil's
model and simply has fewer states to hold in your head.

### 3. Re-run (the deep one)
next's I/O sources are single-run **as a consequence of** spawn-at-wiring: the
producer channel / waker / thread is created once and consumed by the first run
(`interp.rs` documents exactly this — see the single-run note around the
`Runner` docs, ~`interp.rs:327`). That is *not* a fundamental limit. If sources
establish their I/O in `start()`, then `start()` can re-establish on each run,
lifting the single-run restriction for I/O sources and matching classic.

Given next's governing objective — a **strict superset of classic** — and that
classic re-runs these sources, this is a genuine parity gap, not merely a
missing convenience.

### Two extra wins (not obvious up front)
- **Kills the `produce_async` `RunParams` validation dance.** Because
  `produce_async` spawns at wiring, callers pass the run's params *at wiring* and
  the engine must validate they match the actual `run()` later. That entire
  class of "wiring params ≠ run params" mismatch bugs exists *only* because of
  eager spawn. Defer to `start()` and params come straight from the run.
- **Connection errors move from wiring → run-start**, which is *more* consistent
  with classic (I/O failures surface when the run begins, with node context),
  not less.

## Current design (what exists today)

Grounding references (all on the `next` branch):

- **`produce_async` / `produce_async_bounded`** — `next/crates/wingfoil-next/src/async_source.rs`.
  The module doc states outright the task "is spawned at *wiring* time, before
  `Runner::run`"; the spawn is `handle.spawn(async move { … })` inside the
  factory (~`async_source.rs:205`, and ~`:330` for the bounded variant). Used by
  etcd / kafka / redis / postgres.
- **`GraphBuilder::channel` / `external` / `poll` / `source`** —
  `next/crates/wingfoil-next/src/fluent.rs:209–258` (+ `source` at `:59`,
  `wire` at `:325`). `channel()`/`external()` return `(Stream<Burst<T>>, Sender)`;
  the *adapter* spawns the feeder thread and owns the socket.
- **`ChannelSender` / `Message`** — `next/crates/wingfoil-next/src/channel.rs`
  (`Message` enum `:32`, `ChannelSender` `:79`, `send`/`send_at`/`send_error`
  `:110`/`:119`/`:125`).
- **`zmq_sub`** — `next/crates/wingfoil-next/src/adapters/zmq.rs` (~`:247–258`):
  `let (events, sender) = g.channel(); std::thread::Builder::new().spawn(move ||
  run_subscriber(&address, &sender, &stop))?;` — the thread (and the socket
  connect inside `run_subscriber`) happen in the factory, at wiring. Stop *is*
  already lifecycle-bound: a `ThreadStopGuard` (a `Drop`) carried through a
  passthrough op signals the thread at teardown. Only the *spawn* is eager.
- **The `reset` hook** — `next/crates/wingfoil-next/src/interp.rs` (`ResetFn`,
  `set_reset` `:643`, the `register_op1` reset closure `:727–729`). Phase 1
  landed this so the deterministic historical subset (tickers/constants/
  combinators/feedback) re-runs; it restores per-node state to its wiring-time
  initial value. I/O sources are explicitly excluded from re-run today.
- **The single-run decision** — `next/docs/port-plan.md` §0.4 ("decided
  (single-run v1)") and the capability-matrix "Re-run" row (note ⁸). This
  proposal is a deliberate revisit of that decision for I/O sources.

## Proposed design

### The new primitive: a source with a `start` hook
Add an engine primitive that registers a source whose producer is established in
`start()` rather than at construction. Sketch (name TBD — `source_at_start`,
`channel_at_start`, or a `start`-callback parameter on the existing `channel`):

```rust
// wiring: pure — captures config, returns the stream; spawns NOTHING yet.
pub fn source_at_start<T, Setup, Feeder>(
    &self,
    setup: Setup,   // FnMut() -> Result<Feeder>  — runs in start(), returns the running producer
) -> Stream<Burst<T>>
where
    Setup: FnMut(ChannelSender<T>) -> anyhow::Result<StopHandle> + 'static,
    // ...
```

Semantics:
- **Wiring**: allocate the channel + waker slot, store `setup`. No thread, no
  socket, no connect.
- **`start()`**: call `setup(sender)` → connect + spawn the feeder; return a
  `StopHandle`. A `start` error aborts the run with node context (classic
  parity). This is where re-establishment happens on a re-run.
- **`stop`/`teardown`**: signal the `StopHandle` (the existing `ThreadStopGuard`
  pattern generalises here).
- **`reset`** (for re-run): drop the consumed channel/waker and re-allocate so
  the *next* `start()` gets a fresh pipe. This is the interlock that makes I/O
  sources re-runnable (see "the hard part").

### What stays at wiring vs moves to start
| Concern | Where |
|---|---|
| Address/DSN parse, registry `lookup`, `RunMode::HistoricalFrom` rejection, config validation | **wiring** (pure, fail-fast, still returns `Result`) |
| Socket connect / bind, thread/task spawn, subscription, live receive | **`start()`** |
| Stop signalling, revoke registration, flush | **`stop`/`teardown`** (already there) |
| Channel/waker recreation | **`reset`** (new) |

## The hard part: the re-run / reset / channel-recreation interlock

Everything above except re-run is mechanical. Re-run is where the real design
work is:

1. **Channel + waker are consumed by a run.** Today the producer channel and the
   `KernelWaker` (`interp.rs:61`, `waker_channel`) are created at wiring and
   consumed. For re-run, `reset` must drop and **re-create** them so the next
   `start()` hands the feeder a live sender wired to a fresh waker slot at the
   same node index.
2. **`reset` currently only restores per-node *state*** (`interp.rs:727–729`),
   not engine plumbing. Extend it (for source nodes only) to also re-arm the
   channel/waker. Keep the existing behaviour for combinator nodes untouched.
3. **Realtime re-run is fresh data, not deterministic replay** — set
   expectations: re-running a *realtime* source re-subscribes to the live feed;
   it does not replay. Deterministic re-run already works for the
   tickers/constants/combinators subset. The win here is "a long-lived service
   or a test runs the same realtime graph N times," which classic supports and
   next currently rejects on the second `run()`.
4. **`produce_async` historical path**: today the async task produces timestamped
   values collected and replayed on the graph clock. Moving the spawn to
   `start()` should *simplify* this (collection happens per run), but verify the
   historical determinism tests still pass — this is the highest-risk migration.

## Migration order

Do it **spike-first**, smallest surface first:

1. **Spike** — add the `source_at_start` primitive and convert **one** source.
   Recommended: the bare `external`/`channel` source, or `zmq_sub` (self-
   contained, has a real socket + the `ThreadStopGuard` already). Prove:
   (a) the factory is now side-effect-free and unit-testable without I/O;
   (b) a two-run test actually re-subscribes and produces on both runs;
   (c) teardown still stops the thread. Write the short design note off the back
   of the spike.
2. **`external`** and the plain `channel` feeders.
3. **`produce_async` / `produce_async_bounded`** (highest risk — the historical
   determinism + `RunParams` simplification). Migrate etcd/kafka/redis/postgres
   as they ride on it.
4. **`zmq_sub`** (if not the spike) and any remaining adapter feeders.
5. **Flip the capability matrix**: `port-plan.md` "Re-run" row for I/O sources,
   and rewrite §0.4's single-run ruling to reflect the new model.

## Decisions to ratify (call these out to the reviewer)

- **Reopening the Phase 0.4 single-run decision** for I/O sources. This was a
  deliberate v1 scoping call; this proposal changes it. Confirm it's wanted now
  vs. documented-for-later.
- **`poll` sources** (busy-spin, realtime-only): do they also defer to `start()`?
  They currently establish in a `start`-ish path already; audit for consistency.
- **Eager-connect-for-fail-fast** deviation (e.g. `etcd_pub` connects at wiring
  "so a connection error surfaces before the run"): accept that these errors now
  surface at run-start instead. Confirm that's acceptable (it matches classic).
- **Naming** of the primitive and whether it's a new method or a `start`-hook
  parameter on the existing `channel`/`external`.

## Acceptance criteria (for the spike, then each migration)

- Calling the source factory spawns **no** thread and opens **no** socket
  (assert via a unit test that constructs the graph and never runs it).
- Config errors (bad address, historical mode) still fail at **wiring** with the
  same messages.
- I/O + connect happen in `start()`; a `start` error aborts the run with node
  context.
- Teardown stops the producer (existing `ThreadStopGuard` behaviour preserved).
- **Re-run**: a test that calls `runner.run(RealTime, …)` twice re-subscribes and
  produces on both runs (no "already run" error for the migrated source).
- No regression in the existing parity suites, especially `produce_async`'s
  historical determinism tests and the zmq integration suite.
- `cargo fmt` / `cargo lint` / `cargo lint-all` (or the scoped
  `cargo clippy -p wingfoil-next --all-features` if aeron's C lib can't build in
  the sandbox) / `cargo test -p wingfoil-next --all-features` all green.

## Risks & open questions

- **`produce_async` historical determinism** is the migration most likely to
  break; treat it as its own PR with the determinism tests as the gate.
- **Waker/index stability across re-run** — the re-created channel must bind to
  the same node index/waker slot; get this wrong and a re-run wakes the wrong
  node. Cover with a re-run test that asserts values *and* the producing node.
- **Islands / nested graphs** own a private interior with no reset hook
  (`interp.rs` single-run note) — leave them single-run; this proposal is about
  interpreted-graph I/O sources only.
- Is the extra `reset`-recreates-channel complexity worth it for realtime-only
  re-run? The testability + reason-about wins land even without re-run; re-run
  is the additional, higher-cost payoff. It's legitimate to land steps 1–2
  (defer + testability) first and treat re-run (the channel-recreation
  interlock) as a follow-on.
