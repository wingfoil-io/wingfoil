# Review: `docs/port-plan.md` + the `wingfoil-next` implementation

Date: 2026-07-20. Scope: the port plan (`docs/port-plan.md`) and the
`wingfoil-next` / `wingfoil-next-macros` crates as of this branch. All
findings below were verified against the actual code paths (and, where noted,
reproduced with probe tests); classic parity claims were checked against
`wingfoil/src/nodes/*`, `wingfoil/src/adapters/statistics.rs`,
`wingfoil/src/graph.rs`, and `wingfoil/src/codegen/kernel.rs`.

**TLDR:** The plan is genuinely good — right strategy (parallel port behind a
facade, parity-oracle testing, fallibility first), honest capability matrix,
and its Phase 4.5 characterization of the classic engine is accurate (classic
really is a layer-ordered dirty-list: `dirty_nodes_by_layer` in `graph.rs`).
The implementation is clean and all tests/lints pass. But the review found
**real, verified drift between the three execution paths** — exactly the
class of bug the plan claims is impossible by construction — plus a handful
of semantic-parity gaps vs classic, and panics where the repo's own error
rules require `Result`. The plan's "the engines cannot drift" claim holds for
`Op::cycle` semantics but **not for engine-owned initialization and
evaluation timing**, which is where every confirmed divergence sits.

## Highest-priority implementation bugs

1. **Fold value-slot seeding drifts between engines.** Interpreted seeds the
   output slot with `init.clone()` (`wingfoil-next/src/interp.rs:753`); the
   macro's compiled/nested emission seeds every slot with
   `Default::default()` (`wingfoil-next-macros/src/lib.rs:1247`). A fold with
   `init != Default` read before its first tick (via `sample`/`join`/passive
   edge) returns `init` interpreted but `0` compiled — reproduced with a
   probe. Fix: add a value-seed field to `OpInfo` and emit a clone of the
   state for Fold; add a parity test with a non-default init.

2. **`delay(0)` diverges from classic.** Classic special-cases zero delay and
   emits inline (`wingfoil/src/nodes/delay.rs:27-30`); next always schedules
   `time + 0`, which pops on the *next* cycle — so under `RunFor::Cycles(4)`
   classic yields 1,2,3,4 and next yields 1,2 with half the budget burned on
   empty wakeups.

3. **`Delay` loses classic's first-value seeding.** Classic stores the first
   upstream value without ticking, so passive readers never see
   `T::default()` before the delay elapses. The `Tick` contract has no way to
   say "update value, don't tick" — this is a **contract-level gap**, not
   just a delay bug. Decide: add a `Tick::Silent(T)` variant (or equivalent)
   or document the deviation. Right now it is silent drift with no test.

4. **Historical channel `start` block-collects the entire stream**
   (`interp.rs:332-371`). Classic deliberately goes non-blocking to avoid
   deadlocking a producer that depends on graph output
   (`wingfoil/src/nodes/channel.rs:152-193`); next's version can deadlock,
   holds the whole feed in memory, silently *sorts* out-of-order timestamps
   where classic errors, and a `send_at` earlier than `start_time` rewinds
   the run clock (the kernel schedules it verbatim, so the first cycle runs
   *before* `HistoricalFrom(start)`). Clamp/reject pre-start times and
   document (or fix) the blocking collect.

5. **Realtime `close()` is a no-op.** No `finished` flag (classic sets one);
   the run only ends when every `ChannelSender` clone is dropped. Keep a
   clone alive, call `close()`, and the run hangs to its bound. The existing
   test passes only because the producer thread drops its sender.
   `Checkpoint` is likewise documented but discarded on both receive paths.

6. **Panics on reachable user error from `Result`-returning APIs.**
   `Runner::run` uses `assert!` for "external/poll graph run historically"
   and `.expect` for a second realtime run (`interp.rs:1209-1228`);
   `compat::Signal` panics on peek-before-run, and a second `run()` silently
   runs a 0-node graph then panics out-of-bounds on `peek_value`
   (`compat.rs:107-124`). Per CLAUDE.md's error-handling rules these should
   `bail!` — none are unreachable invariants.

7. **Compiled path re-evaluates non-literal closure args every cycle.**
   `count.map(make_f())` runs the factory once at wiring interpreted, once
   *per due cycle* compiled (`lib.rs:1345-1354`) — reproduced with a
   side-effecting factory. Also means `move` closures capturing non-Copy
   locals compile via `wire()` but fail inside the emitted loop. Hoist the
   closure expression into a setup local (a zero-capture closure local still
   preserves the `cycle_owned_cfg` inference-deferral trick).

8. **The macro accepts inputs it emits broken code for**: a bare alias
   `let out = acc;` used as an output, and returning a bare ticker
   (`Stream<()>`), both work interpreted but emit `__v_out`-not-found errors
   compiled/nested (`lib.rs:914-918`, `lib.rs:1390-1396`, `lib.rs:1428-1429`).
   User streams named `anon1` collide with generated intermediate names
   (`lib.rs:475-478`). There are zero trybuild compile-fail tests pinning any
   of the macro's ~20 error paths.

9. **`produce_async` takes caller-invented `RunParams`, unchecked against the
   actual run** (`async_source.rs:52-57`). Classic derives them from the
   graph's own run; here nothing verifies them against `run(run_mode,
   run_for)`, and classic's `buffer_size` bounded-channel backpressure is
   missing (the mpsc is unbounded).

## Documentation drift (cheap, fix now)

- `channel.rs:15-17` says channel is "realtime only … latest-wins" — both
  claims contradict the implementation, the tests, and the crate docs
  (both-modes bursts is the headline feature).
- `tests/busy_loop.rs:6,18` still claims `external` is latest-wins; it drains
  to a `Burst`.
- The "named fields make a half-filled row a compile error" claim
  (`port-plan.md`, "Adding an op"; `lib.rs:188-190`) is defeated by the
  `..base` struct-update in every `OpKind::info()` arm — an omitted
  `callback_activated: true` on a new scheduling op compiles clean and
  silently never fires compiled/nested. Drop `..base` and spell all fields
  per arm.
- `channel.rs:30`: historical `Message::Value` is delivered at `start_time`,
  not "the current time".

## Design concerns

- **Sample's passive edge is a hard-coded special case outside the op table**
  (`lib.rs:343-349`): passive-edge topology is the one op fact not in
  `OpInfo`, and it must agree with `interp.rs:790` — three sites to touch for
  the next passive-edge op, silent scheduling drift if one is missed. Move
  active/passive edge shape into `OpInfo`.
- **Cross-graph `Handle` misuse is unguarded** (`interp.rs:1301-1309`): a
  handle from a different `GraphBuilder` with a colliding index and type
  silently returns the wrong node's value. A cheap builder-id in `Handle`
  plus a `debug_assert` would catch it.
- **`GraphBuilder::build(&self)` + `mem::take`** (`fluent.rs:99-104`): a
  second `build()` silently returns an empty `Runner`; wiring from a retained
  `Stream` afterwards panics out-of-bounds deep inside `slot()`. Consume
  `self` or poison explicitly.
- **`FeedbackSink` has no public `send`** — classic's is user-callable from
  custom nodes; only the `feedback_send` pass-through wiring exists. (The
  `+1` scheduling, pop-all-keep-last, `TimeQueue` dedup, and `PartialEq`
  bound all correctly match classic.)
- **No EWMA alpha validation** (`ops.rs:529`, `stats.rs:43`); classic
  `debug_assert!`s `[0,1]`. Alpha > 1 silently diverges.
- **~10 hand-written `Builder` adapter closures repeat the same pattern**
  (ticker/constant/throttle/window/…). `register_op1` exists; a
  `register_op0` (sources) and `register_op2` would collapse most of it.
- **`#[op]` is crate-internal by construction** (`lib.rs:1097` emits
  `impl crate::interp::Builder`) — fine, but worth a doc note since the
  fluent layer advertises third-party op traits.
- **`mentions_graph_ident` is token-textual** (`lib.rs:389-407`): any
  identifier equal to a stream name in a passthrough statement triggers the
  "wiring must be straight-line" error, which doesn't name the offending
  identifier.
- **`Window::start` anchors at `ctx.time()` (= ZERO during start), not
  `start_time`** — bug-for-bug identical to classic, so parity holds; a
  shared quirk both engines should fix together, not unilaterally.

## Test-suite blind spots

The parity suites are well-designed structurally but miss exactly where the
real drift lives:

- No fold with `init != Default` (hides bug 1).
- Merge tie-break never tested with both inputs ticking in one cycle — the
  compiled tick-pair ordering (`lib.rs:1613-1617`) could be swapped and tests
  stay green (`odds_evens` streams are disjoint by construction).
- No ports of classic `zero_delay_works` (under `RunFor::Cycles`) or
  `delay_initializes_to_first_value` (would catch bugs 2 and 3).
- EWMA half-life only tested on a constant stream, which passes for *any*
  decay math — port classic's real-decay 3.125 assertion.
- No island with two inner scheduling ops (ticker + delay), so the private
  queue demux (`lib.rs:1526-1528`) is never stressed with multiple pending
  keys; no island containing merge/filter/constant/statistics.
- `compiled_parity.rs` is a hand-written expansion that will silently diverge
  from what the macro actually emits — cross-assert against a macro-emitted
  `compiled()` or regenerate it.
- No test for realtime `close()` with a live sender, checkpoint semantics,
  pre-start timestamps, or the out-of-order-timestamp policy.
- No trybuild compile-fail tests anywhere.
- Misc: `ticked_at_elapsed` untested; `rolling_sum(0)`/`rolling_mean(0)`
  clamp untested; `window` only tested from `HistoricalFrom(ZERO)`; no
  compat double-`run` or peek-before-run test (both currently panic).

## Parity confirmed (checked value-by-value and tick-by-tick against classic)

Filter (including re-emission when only the condition ticks), Merge2
earliest-supplied-wins, Throttle, Window cycle logic, Buffer, Distinct
(Option-state so a default-equal first value still ticks), Limit, Difference,
Fold, Sample (trigger-active/source-passive), Ewma (identical math including
the explicit `initialised` flag and `1 − 2^(−Δt/half_life)`),
RollingSum/RollingMean (including the `max(1)` window clamp), feedback `+1`
timing (1, 11, 111… and 1, 12, 123… reproduced), and historical channel burst
grouping/timestamps. `Delay`'s pop-all-keep-last lossiness also matches
classic (same behavior, so not drift).

## Plan-level improvements

1. **Stale status header.** The plan opens with "draft plan — no porting work
   has started" while the body has ✅-landed spikes and started phases.
   Update it (and the prototype-branch reference, now that the crates live on
   the reviewed branch).
2. **Phase 4.5 sequencing is a rework trap.** The arena/SoA value store
   changes the slot representation that every `Builder` registration and
   macro emission captures today (`Rc<RefCell<T>>` inside closures). Porting
   the ~40-node catalog and adapters first, then landing 4.5, means touching
   all of it twice. Either land 4.5 before the Phase 2/4 volume, or freeze
   the slot API boundary now so registrations survive the arena unchanged.
   "Can land any time before Phase 6" understates the coupling.
3. **The 0.4 single-run decision collides with Gate 6.** Re-run is deferred
   "until a use case demands it" — but the compat facade *is* the use case:
   classic streams re-run, `compat::Signal` already breaks on it (bug 6), and
   wingfoil-python's pytest suite is the gate. Move the reset hook into
   Phase 1 contract work rather than discovering it at the facade.
4. **Add "engine-owned init / evaluation-timing drift" to the risk register
   and testing strategy.** The macro crate's three emission paths are the
   biggest drift surface in the codebase (the fold and closure-factory bugs
   prove it), yet the register only lists op-semantics-level risks.
   Concretely: a table-driven parity test file with one micro-graph per
   macro-supported combinator asserting interpreted == compiled == nested —
   this also behaviorally guards the activation table.
5. **Stop planning the completeness test — add it.** It is acknowledged as
   "recommended, not yet added" in two places. Cheapest shape: a
   `supported_ops!()` function-like macro emitting the same list the
   parse-match and error message use, diffed in a `wingfoil-next` test
   against the fluent trait surface with an explicit "not expressible"
   allowlist. (The reverse direction — `graph!`-but-not-fluent — is already
   guarded by construction, since `wire()` compiles verbatim.)
6. **Move the `Tick::Silent` question into Phase 1.** Bug 3 shows the
   contract can't express "update value without ticking," which classic
   uses. That is a Phase-1 contract decision, not a delay-porting detail —
   deciding it late risks retrofitting every emitter, the exact mistake the
   plan avoided with fallibility.

## Suggested order of attack

Fix the fold seeding + a non-default-init parity test first (silent wrong
values), then the delay pair (zero-delay + first-value, with the
`Tick::Silent` decision), then convert the panics to errors, then the channel
lifecycle issues, then the doc drift — all before more catalog porting, since
each is a drift class that would otherwise be copied into every new op.
