# wingfoil-next → classic: deviation register

Status: **living checklist** for the eventual Phase-7 cutover audit. Every place
wingfoil-next's behaviour or surface deviates from the classic `wingfoil` tree,
collected in one place and classified so each can be given an explicit
accept/fix ruling before cutover.

**Sources:** (1) each ported adapter's `# Deviations from classic` module-doc
block — regenerate with
`git grep -n "Deviations from classic" next/crates/wingfoil-next/src/adapters`;
(2) `next/docs/port-plan.md` (capability matrix + Phase 4.5 "Known parity gaps");
(3) a manual **lifecycle/timing audit** (Category A) — the systemic engine-model
behaviours the per-adapter deviation lists do **not** capture (the wiring-time
pattern below was in *no* adapter's list, which is why it needs its own audit).

**Legend:** 🔴 potential regression / needs a decision · 🟡 deliberate deviation,
confirm acceptable for the superset claim · 🟢 cosmetic / benign · ⚪ tracked
capability gap (deferred by design) · ✅ **resolved** since this register was
written (kept for the audit trail, no ruling needed).

---

## A. Systemic lifecycle & timing

Engine-model behaviours, pervasive across adapters — **not** in the per-adapter
deviation lists. The lifecycle items (A1–A4) are the subject of
[`source-lifecycle-defer-to-start.md`](./source-lifecycle-defer-to-start.md); A5
has its own decision record in
[`runtime-ownership.md`](./runtime-ownership.md).

| id | dev | class | notes |
|---|---|:--:|---|
| A1 | **Wiring-time I/O establishment.** Remaining eager: `external` + plain `channel` feeders (caller-owned producers — really re-run, see A2); sinks (`etcd_pub`, redis, `kafka_pub`, `postgres_write`) connect eagerly at wiring; `postgres_read` runs its whole query at wiring (B5); `prometheus::serve` binds at wiring. | 🟡🔴 | Classic connects in `setup`/`start`. Causes: side-effecting/untestable wiring, a "wired but not running" window, realtime pre-run message accumulation. → defer-to-start plan. **Partially resolved:** the `source_at_start` primitive landed and `zmq_sub` migrated (#547); the **`produce_async` family (etcd/kafka/redis/postgres `_sub`)** now spawns its producer in `start()` too, deferred via `source_at_start_with_params` — connect/subscribe happen at run start, wiring is side-effect-free (`produce_async` still takes `RunParams`; deriving from the run is a follow-on). The sinks + `postgres_read` + `prometheus::serve` are the remaining migrations. |
| A2 | **I/O sources are single-run.** A second `run()` errors; classic re-runs. | 🔴 | A *consequence* of A1 (the channel/waker/thread is consumed). Real superset gap. Still open: `source_at_start` (#547) defers establishment but inherits the single-run channel, so even `zmq_sub` is not yet re-runnable — re-run is the tracked follow-on (port-plan §0.4 reopened). → plan. |
| A3 | **`zmq_pub` binds its socket lazily on its first cycle**, not `start()`. | 🟡 | Another "when does I/O happen" quirk; interacts with the slow-joiner window. |
| A4 | **Error-surfacing timing shifted to wiring** — connection errors surface during graph construction, not at run start / first op. | 🟡 | Still applies to the eager-connect **sinks** in A1. **Resolved for `zmq_sub`** (#547) and the **`produce_async` `_sub` family**: their producers now establish in `start()`, so a connect/subscribe failure surfaces during the run (via `send_error`), not at wiring — classic-consistent. (The historical-mode *rejection* for those `_sub` sources still fires at wiring, a deliberate fail-fast.) Deferring the remaining sinks moves them the same way. |
| A5 | ~~**Caller-owned tokio runtime**~~ — etcd/redis/kafka/postgres/otlp took `&Handle`. | ✅ | **Resolved (#548).** The `GraphBuilder` now owns one tokio runtime (lazy, shared, dropped at teardown) with a `with_async_runtime` override; the `&Handle` param is gone from every async factory. Decision record: [`runtime-ownership.md`](./runtime-ownership.md). *Residual (A5a below) is unchanged.* |
| A5a | **"Drive from a non-async thread."** The `block_on` sinks/readers still panic if the graph is built/run/dropped inside an async context. | 🟡 | Inherent to `block_on`-on-the-graph-thread (an owned runtime doesn't change it — its workers are separate threads either way). Documented per-adapter; matches classic's constraint. Not removed by #548. |
| A6 | **channel-sub establishes slower than classic's `ReceiverStream`** (the zmq first-message test needed a ~600 ms settle vs classic's 200 ms). | ❓ | Behavioural difference; **mechanism not pinned** (see the zmq fix PR #542 discussion). Worth a measured investigation. |

## B. Behavioural / capability deviations — need a decision

| id | dev | class | notes / source |
|---|---|:--:|---|
| B1 | ~~**`etcd_pub` blocks the graph thread** — `Handle::block_on` per burst.~~ | ✅ | **Resolved.** `consume_async` now returns a `flush` teardown (wired via `finally`) that closes the sink, joins the consumer task, and — unlike a `Drop` — surfaces the **final** write error as the run's `Err`. This is the "teardown-hook story for synchronous-error ops" B1 was blocked on: it lets `etcd_pub`'s `force:false` conditional abort a single-cycle run at teardown (exactly as classic's `AsyncConsumerNode::teardown` does), so the per-write `block_on` is gone and the PUTs run off the graph thread on the shared consumer task. The wiring-time connect + `LeaseGuard` revoke keep their (teardown-time, graph-thread) `block_on` — the ordinary `consume_async` footgun (A5a). The same `flush` upgrade also closes the "final-cycle write error swallowed" gap for kafka/postgres/redis/otlp. |
| B2 | **Live sources reject `RunMode::HistoricalFrom` at wiring** (etcd/redis/kafka/postgres `_sub`, zmq_sub). | 🟡 | Live tails are unbounded wall-clock streams; a historical run would block-collect the whole stream and deadlock at `start`, so next rejects at wiring with a pointer to the bounded reader. **Classic parity for postgres** — classic `postgres_sub` already required `RunMode::RealTime` and bailed otherwise (`adapters/postgres/sub.rs`), so next's rejection is parity, *not* a deviation there; the "classic permitted a wall-clock historical run" gap is real only for `zmq_sub` and the etcd/redis/kafka `_sub` sources, which had no such guard. **Split ruling (agreed plan below):** (a) **ratified reject** for live-only sources with no bounded historical twin (`zmq_sub`, `etcd_sub`, both redis sources, `kafka_sub` today); (b) for adapters that *do* have a bounded historical read alongside the live tail, replace the mode-locked `_read`/`_sub` function pair with a single mode-agnostic `<adapter>_source` that dispatches on `run_mode`. → plan §B2. |
| B3 | ~~**`kafka_pub` produces sequentially** (N roundtrips/burst) vs classic's concurrent `FuturesUnordered` (one roundtrip/burst).~~ | ✅ | **Resolved.** Added `consume_async_bursts` — a variant that hands the consumer a whole burst at a time (order preserved *across* bursts, concurrency within one left to the sink). `kafka_pub` now drains a burst's sends together via `FuturesUnordered` (~one broker roundtrip/burst), at throughput parity with classic. `adapters/kafka.rs` sink docs. |
| B4 | **csv reads the whole file up front** vs classic's lazy row streaming. | 🟡 | Unbounded-memory for a huge file under `RunFor::Forever`. `adapters/csv.rs` "consequences of using the channel source". |
| B5 | **`postgres_read` queries all slices at wiring** onto `replay_results` vs classic's lazy `produce_async`. | 🟢🟡 | "Identical for a bounded historical run," but buffers the whole result set (memory). Same family as A1. `adapters/postgres.rs` deviation #2. |
| B6 | **`spawn_map` historical is lock-step by graph time** (twin of classic `mapper()`). | 🟢🟡 | Values + tick times match classic. Two benign artifacts: (a) the sub-graph is expected to emit a result per input instant (a filtering/delaying sub-graph desynchronises the lock-step — classic's `graph_node` delay case likewise fails); (b) the lock-step reader spends one no-op poll cycle between instants (next's monotonic-clock re-arm), so bound runs by **duration**, not a raw cycle count. `fluent.rs::spawn_map`, `tests/spawn.rs`. |

## Resolved

- **Channel historical block-collect → incremental read.** The historical channel
  source previously block-collected the entire feed into memory at `start()`
  (documented deviation: unbounded memory + deadlocks a producer that depends on
  the graph's output). Replaced with an incremental, timestamp-gated one-ahead
  read (`interp.rs::pump_historical`, classic's block-while-behind loop), giving
  bounded memory and unblocking concurrent producer↔consumer (this is what makes
  `spawn_map` historical possible). B4/B5 (csv/postgres reading everything at
  wiring) are now adapter-side pre-queueing, no longer amplified by the channel.
  The transport itself is still unbounded by default, but opt-in back-pressure is
  available — `SourceOps::channel_bounded` / `spawn_bounded` / `spawn_map_bounded`
  take `Option<usize>` (`None` = unbounded, `Some(n)` = `sync_channel(n)`). Not for
  producers that fill the buffer before the run starts (`replay_results`, csv,
  `postgres_read`) — those stay on the unbounded path.

### B2 — agreed plan: unified `<adapter>_source`

**Why.** `RunMode` is a *run-time* choice: build the graph once, pick real-time
vs historical at `run()`. Mode-agnostic sources (`ticker`, `constant`,
`channel`) honour that, but the mode-locked `_read`/`_sub` split forces the
switch into *wiring* — flipping modes means editing the graph, not the run
call, and duplicating downstream wiring across two source functions. Classic
had the same split; next's superset mandate lets it improve without dropping
the primitives.

**Design.** For any adapter with **both** a bounded historical read and a live
tail, add a single mode-agnostic `<adapter>_source(cfg)` that inspects
`params.run_mode` at wiring time and constructs the right mechanism:

- `RunMode::HistoricalFrom` → the bounded, time-sliced replay (the `_read`
  mechanism — deterministic, no deadlock).
- `RunMode::RealTime` → the `LISTEN`/`NOTIFY`-style live tail (the `_sub`
  mechanism).

`cfg` carries both halves, each **optional**: a historical windowed-query spec
and a live-tail spec. Supply both → a fully mode-agnostic graph (flip the mode
at `run()`, downstream wiring unchanged); supply one → works in that mode and
errors in the other with a message naming the missing half. This strictly
dominates today, where the two modes can't even be *expressed* without swapping
function names. Keep `_read` / `_sub` as low-level primitives; `_source` wires
through them.

**Feasibility.** `run_mode` is already available to source builders at wiring
(`params.run_mode`), so the dispatch is a wiring-time branch. The deadlock that
motivated the reject only ever required two *mechanisms*, not two public
*functions*.

**Scope — where `_source` applies.**

- **postgres** — the only adapter with both forms today (`postgres_read` +
  `postgres_sub`). ✅ **Landed:** `postgres_source(cfg)` with a
  `PostgresSourceConfig` carrying an optional historical half and an optional
  live half, dispatching on `params.run_mode` at wiring; the two primitives stay
  public underneath. `adapters/postgres.rs`.
- **kafka** — candidate *once* it gains a bounded offset-range replay reader
  (the durable log makes this feasible); only `kafka_sub` exists now, so it
  stays under ruling (a) until then.
- **Live-only, reject ratified as-is:** `zmq_sub`, `etcd_sub`, and both redis
  sources (`redis_sub` Pub/Sub and `redis_stream_read` are live/unbounded with
  no historical timeline). Nothing to dispatch to under historical, so the
  wiring-time reject is the honest behaviour and B2 is accepted for these.

## C. Capability gaps — tracked, deferred by design

| id | gap | class | notes |
|---|---|:--:|---|
| C1 | **otlp trace/span export (`OtlpSpans`) not ported.** | ⚪ | Needs the `Traced`/`HasLatency`/`latency_stages!` infra (Phase 5). Metrics push is at full parity. `adapters/otlp.rs` deviation #3. |
| C2 | **zmq cross-language interop not ported.** | ⚪ | The `bincode` wire envelope is next-local; not wire-compatible with a classic/Python peer. Deferred with the Python bindings (Phase 6). `adapters/zmq.rs`. |
| C3 | **Structural gaps** — multi-output islands; runtime graph-mutation surface; (closed: `StreamStore`, `demux_it` in #529). | ⚪ | See `port-plan.md` Phase 4.5 "Known parity gaps" and Phase 1 multi-output note. |
| C4 | **Compiled-path IO ingestion** — busy-poll (`ALWAYS`) sources + bursts are not expressible in `compiled()`/`graph!`. | ⚪ | Deliberate exclusion; IO stays at the interpreted boundary + compiled islands. Full design + the gating decision (wake channel vs busy-spin) in `port-plan.md` "Deferred / post-v1 work". (was #502/#503) |

## D. Cosmetic / API — deliberate-and-benign (low review priority)

| id | dev | class | notes |
|---|---|:--:|---|
| D1 | Sink-as-trait vs classic's free fn + operator trait (every adapter). | 🟢 | Convention. |
| D2 | Factories return `anyhow::Result` (+ `.context`) vs classic's typed `io::Error` (e.g. `prometheus::serve`). | 🟢 | Fallible-with-context convention. |
| D3 | postgres/redis sinks take a `consume_async` `buffer_size`. | 🟢 | Added back-pressure knob. |
| D4 | `DemuxMap` uses a `BTreeSet` slot pool (lowest free slot, deterministic) vs classic's `HashSet`. | 🟢 | An *improvement* — aids backtest determinism (#529). |
| D5 | **otlp pins opentelemetry 0.32; classic still on 0.28.** | 🟢 | **Won't-fix — moot at cutover.** Deliberate divergence for security (GHSA-w9wp-h8wv-79jx, #543). Classic is retired wholesale at cutover, so next's 0.32 is the surviving version and the classic-side advisory disappears with the legacy tree; bumping classic to restore lockstep is not worth the churn. |
| D6 | csv malformed-row error surfaces at replay start vs classic mid-stream. | 🟢 | Same run-failure outcome + context string; documented. |
| D7 | `FileCache` log messages drop the classic "KDB " prefix. | 🟢 | The cache isn't kdb-specific in next. |
| D8 | **`print` emits per tick instead of buffering to teardown.** Classic `PrintStream` buffers every value in a `Vec` and prints the whole buffer at `Drop`; next's `Print` op prints each value immediately in `cycle`. | 🟢 | Deliberate — the observable value stream is identical (`print` is a pass-through); only the diagnostic emission differs. Per-tick printing drops the unbounded buffer (classic grows one entry/tick for the whole run), streams output as the run progresses, and survives a mid-run abort. Shedding the teardown hook also makes `print` a plain single-input `#[op]` (no hand-written `Builder`). `ops.rs::Print`. |
| D9 | **`logged` always wires its node; classic skips it when the level is disabled.** Classic `logged` short-circuits at wiring (`log_enabled!` → return the source unchanged) and reads the tick time via `bimap` + `ticked_at_elapsed`; next always wires the op, leaning on `log!`'s internal enabled-check, and reads the time off `Ctx::time`. | 🟢 | Value stream identical (a pass-through); only the diagnostic differs, and a disabled level still costs only the internal `log!` check. Always-wiring keeps interpreted and compiled identical (a wiring-time skip is inexpressible in `compiled()`). `logged` is fluent-only (its `&str` param vs `String` cfg — see `tests/op_completeness.rs`). `ops.rs::Logged`. |

---

## Recommended priorities

1. **A1 / A2 (+ B5)** — the defer-to-start plan. The primitive (`source_at_start`)
   and the `zmq_sub` migration landed (#547); finish it by migrating the
   `produce_async` family + the plain `external`/`channel` feeders, then reopen
   §0.4 to make I/O sources **re-runnable** (A2) — the remaining structural win.
2. **B4** — decide whether the csv whole-file memory deviation matters for the
   "strict superset" claim, or is an acceptable documented trade-off.
3. **A6** — measure the channel-sub startup latency to either explain or close it.

**Resolved / ratified since this register was written:** **A5** (graph-owned
runtime, #548); **A1/A4 for `zmq_sub`** (deferred to `start()` via
`source_at_start`, #547); **B1** (`consume_async` `flush` teardown surfaces
final-cycle write errors, so `etcd_pub` moved off the graph thread — also closes
the swallowed-final-error gap for kafka/postgres/redis/otlp); **B2** (split
ruling, #557: reject ratified for live-only sources; adapters with a bounded
historical twin move to a unified mode-dispatching `<adapter>_source` — see
plan §B2, postgres first); **B3** (`consume_async_bursts` restores kafka's
concurrent per-burst publish).

## Keeping this current

Re-run the two greps in **Sources** above after each adapter/engine PR and add
any new `# Deviations from classic` entry here with a class. Category A is a
manual audit — re-examine it whenever the source/sink lifecycle or the channel
machinery changes. At cutover, every 🔴 and 🟡 needs an explicit ruling.
