# Execution layer: from off-graph tests to on-graph coverage

**Status:** plan, not started. Tracking issue: to be opened from this page.

## The problem

`src/adapters/execution/` is tested almost entirely off-graph. The numbers,
as of `32bdb87` (#1004):

| Where | Tests | Touch `GraphBuilder` |
|---|---|---|
| `#[cfg(test)]` beside the pure modules (`oms`, `position`, `order`, `algo`, `fix`, `kill_switch`, `reconcile`, `rate_limit`, `edge`, `exec_id`) | 189 | 0 |
| `#[cfg(test)]` in the node modules (`oms/node`, `algo/node`, `fix/node`, `venue`, `ceiling`, `testing`) | 29 | ~16 |
| `tests/execution_fix_venue.rs` | 11 | 2 |
| `examples/adapters/execution/*` | 8 | **0** |

Three things follow from that shape, and each is a real gap rather than a
style point:

1. **The rules the module argues are rules about instants, and nothing
   exercises them at instants.** `execution/CLAUDE.md` is largely a list of
   ordering and timing claims — reports before trading before cancel-all
   before desireds before the diff, within one instant; a refused level held
   until a desired with a newer `as_of`; `max_desired_age` judged at the
   reader's engine time; distinct ids defusing `TimeQueue` dedup on the
   feedback cut; a cross `retake` apart; executions before requests in the
   replace chain so a same-instant fill is in the `CumQty`. A test that
   calls `Oms::diff` by hand with a `now` it chose cannot fail on any of
   them. Only the two `closed_loop` tests in `execution_fix_venue.rs` can.
2. **Every example does the thing `CLAUDE.md` forbids.** "A caller must not
   hold an `Oms` in a `RefCell` and call it by hand from a graph" — and all
   eight examples hold one and call it by hand from `main`, advancing a
   fake clock in a `for` loop. They teach the API shape the docs say not to
   use, and none of their wiring could be pointed at a live venue.
3. **Half the layer has no graph surface to test on.** Only `oms`, `algo`
   and `fix` have a node and an `*Ops` trait. `position::Book`, the
   `reconcile::Reconciler`, `kill_switch::Switch`, `rate_limit` and the
   `ceiling::Judge` are pure types with no op, so the loop the roadmap
   describes — fills folding into a book, the book read against holdings, a
   mismatch latching the switch, the latch pulling the quotes — cannot be
   wired at all today, let alone tested.

The pure unit tests are not the problem and are not to be removed: `ExecId`
capacity, `Order::validate`, `Book`'s arithmetic, `OrderRate`'s token maths
are arithmetic and belong where they are. What is missing is the layer above
them.

## The rule this plan installs

> **A behaviour the module docs argue in terms of instants, order within an
> instant, or the feedback cut is tested on a graph, under
> `RunMode::HistoricalFrom`, asserting values *and* tick times.** The pure
> methods keep their unit tests for arithmetic; they do not stand in for
> the graph test. Every execution example wires a graph.

It goes into `execution/CLAUDE.md` under *Tests* in the first PR.

## Phase 1 — a shared on-graph harness (no new ops)

The `Script` / `closed_loop` / `Loop` trio in
`tests/execution_fix_venue.rs:420-483` is the right shape: rows replayed at
engine instants via `replay_results`, one `GraphBuilder::feedback` cut,
`testing::SimVenue`, every output `with_time().accumulate()`. It is private
to one test file. Promote it:

- `testing::Scripted<I>` (behind `execution-testing`): touches, desireds,
  cancel-alls, trading states, holdings, account, each as `(row, instant)`
  rows; `fn wire(&self, g: &GraphBuilder) -> Wired<I>` that builds the
  loop and hands back the streams, **not** a run — so a test or example can
  hang more nodes on it before `g.build()`. A `run()` convenience returns
  the flat `(NanoTime, Vec<_>)` rows for assertion.
- Keep `SimVenue` as the venue; `Scripted` composes `Capped` around it when
  a cap is given.
- `tests/execution_fix_venue.rs` keeps its hand-driven `Rig` tests (they
  test the chain's bookkeeping) and moves its two graph tests to the new
  file below, using the promoted harness.

New `tests/execution_graph.rs`, `#![cfg(feature = "execution-testing")]`,
one test per instant-level rule the OMS docs state. First tranche, all
wireable today:

| Rule (from `execution/CLAUDE.md` / `oms` docs) | Script |
|---|---|
| In-instant order: reports, trading, cancel-all, desireds, diff | Report, `Closed`, cancel-all and a desired all at one instant; assert the one burst that leaves |
| A refused place is held until a newer `as_of`, not re-sent next instant | Touch crosses the bid; the reject wakes the fold; the same desired restated at the same `as_of` emits nothing; a newer `as_of` emits |
| A refused amend marks the level answered, never spent | Amend rejected; resting order stays; nothing re-sent |
| `max_desired_age` is judged at the reader's engine time | Decision at 10, nothing after; at `10 + age` the OMS cancels on the sweep |
| Distinct ids defuse `TimeQueue` dedup on the cut | Two identical orders on two instruments in one burst both land |
| A cross is one-shot and `retake` apart | Cross IOC killed; restated before `retake` emits nothing; after, emits |
| The budget is spent across cycles and re-planned, never queued | Burst of five places over a four-token budget; the fifth goes out when a token returns, re-planned against the then-current ladder |
| A closed book takes cancels only, held places go out on open | `Closed` at 20 with a changed desired; `Open` at 30 sends the place |
| The `Ratio` holds amends, never a cancel | Ratio exhausted; amend held; cancel-all still pulls |
| Replace chain: a same-instant fill is in the replace's `CumQty` | Partial fill and an amend at one instant through `SimVenue` |
| Parent orders: iceberg restated every beat, on the instrument | `AlgoOps` parent above the OMS, both on the graph |
| Replay determinism | Run the same script twice; the two `Loop`s are equal, request order included |

Each asserts the exact `(instant, burst)` rows the way the existing two
tests do. Expected rows are captured once and pinned, the
`engine_semantics.rs` way.

## Phase 2 — nodes for the half of the layer that has none

Per `/new-op`, under feature `execution`, each with the same shape as
`oms/node.rs` (an `Op` with `#[op(build = ..)]`, an `*Ops` extension trait,
in-instant order argued in the node's module docs):

- **`position::node` / `PositionOps`** — `fills` (the OMS's reports filtered
  to fills, plus `Session::settlements`), `cashflows`, and a marks stream in;
  a `Reading<R>` out, the `oms_reading` pattern, so a graph reads
  `Book::pnl` / `net` after the fold and never holds the `Book`. Order within
  an instant: settlements, fills, cashflows, then the reading.
- **`reconcile::node` / `ReconcileOps`** — the fold node's book plus
  `Session::holdings` in; `Burst<Mismatch<I>>` out; `Fold::rebase` happens
  inside, which is the only place `CLAUDE.md` allows it. Grace is engine
  time.
- **`kill_switch::node` / `KillSwitchOps`** — a caller-supplied assessment
  closure over the readings it chooses, plus an operator `clear` stream in;
  a `Halt` stream (latched / stood / cleared, with `Breaches<L>`) out. The
  latch is the node's state; `snapshot` / `restore` are exposed through the
  op's `start`.
- **`rate_limit`** needs nothing: it is spent inside the OMS. **`ceiling`**
  needs nothing: `Capped` is already a `Venue`.

Each lands with its unit tests *and* its graph tests in
`tests/execution_graph.rs` (second tranche): fills folding across instants
with a mark moving between them; a mismatch past grace re-basing and
latching; `clear` refused on the graph while a breach stands; a restart
(`snapshot` into a second run's `restore`) keeping the latch; and the loop
closed end to end — **latch → cancel-all into `OmsOps` → the venue reports
the cancels → the book goes flat**, as one graph.

## Phase 3 — every example wires a graph

Rewrite seven of the eight examples on the harness or on the nodes directly.
Target names do not change (`execution_oms_quote` stays
`execution_oms_quote`); directory layout and README house style stay as
they are. Outputs are regenerated by running the example, per the README
rule, and stay deterministic because every one runs `HistoricalFrom`.

| Example | Becomes |
|---|---|
| `oms_quote` | `OmsOps` fed by a replayed decision stream, `SimVenue` behind the cut, `print()` on requests and reports |
| `message_ratio` | same loop, `Config::ratio` set, the held amend visible in the row timing |
| `replace_chain` | `FixOps` on the graph, `SimVenue` on the profile with no post-only |
| `exchange_day` | the trading-state stream driving `OmsOps` through the session |
| `position_book` | `PositionOps` folding a replayed fill stream, a reading printed per tick |
| `reconcile` | `ReconcileOps` over the fold, holdings replayed, mismatches printed |
| `latch_and_cap` | `KillSwitchOps` over the fold's reading, `Capped` around the venue, the run aborting on the cap |
| `order_edge` | **stays off-graph.** It teaches constructors and `validate`, which is vocabulary, not behaviour. Say so in its README. |

Plus one new example, `examples/adapters/execution/strategy_loop/` (target
`execution_strategy_loop`): the whole chain in one graph — a toy quoter
emitting `Desired`s off the touch, `OmsOps`, `Capped(SimVenue)`, fills into
`PositionOps`, `ReconcileOps`, `KillSwitchOps` latching on a daily-loss cap
and pulling the quotes. It is the example the `README.md` table opens with,
and the one a venue integration copies.

The `accumulate()` rule holds: examples print as the run goes; only tests
accumulate.

## Phase 4 — docs and the gate

- `execution/CLAUDE.md` *Tests*: the rule above, the harness, the new test
  file, and "an example that calls `Oms::diff` by hand is the anti-pattern".
- `examples/adapters/execution/README.md`: new table, `strategy_loop` first.
- `docs/adding-an-op.md` touch-point table: the execution nodes are
  ordinary ops and follow it; nothing new, but say so.
- Compiled tier: `oms/node.rs` says the op is public so `nitro!` can reach
  it. Add one `tests/execution_graph.rs` case running the quote script
  under `compiled()` and asserting the rows equal the interpreted run; if
  the ops are not yet `#[op(build = ..)]`-emittable, that is a finding to
  file, not to paper over.

## Sequencing

Four PRs, each green on the full pre-commit checklist
(`cargo lint-all`, `cargo test -p wingfoil --all-features`, example docs
check):

1. **Harness + first tranche** — Phase 1, the `CLAUDE.md` rule, the two
   moved tests. No API change beyond `testing::Scripted`.
2. **Position / reconcile / kill-switch nodes** — Phase 2 with its graph
   tests. Minor version: new public ops.
3. **Examples on the graph** — Phase 3's seven rewrites, outputs re-run.
4. **`strategy_loop` + docs** — the end-to-end example, the README tables,
   the compiled-tier case.

PR 1 and PR 2 are independent and can go in either order; 3 needs both;
4 needs 3.

## What this does not do

- Does not delete or weaken a single pure unit test.
- Does not touch `oms.rs`'s diff semantics; if a graph test disagrees with
  a pure test, the graph test is the finding and the fix is a separate PR.
- Does not add a venue, a queue to `FixVenue`, or a second cut
  (`CLAUDE.md`'s harness rules stand).
