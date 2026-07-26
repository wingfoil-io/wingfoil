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
| A1 | **Wiring-time I/O establishment.** Sources still eager: `produce_async` (etcd/kafka/redis/postgres `_sub`), `external`, plain `channel` feeders spawn their thread/task + connect at wiring; sinks (`etcd_pub`, redis, `kafka_pub`, `postgres_write`) connect eagerly at wiring; `postgres_read` runs its whole query at wiring; `prometheus::serve` binds at wiring. | 🟡🔴 | Classic connects in `setup`/`start`. Causes: side-effecting/untestable wiring, a "wired but not running" window, realtime pre-run message accumulation. → defer-to-start plan. **Partially resolved:** the `source_at_start` primitive landed and `zmq_sub` now establishes its socket + thread in `start()`, not wiring (#547); the sources/sinks above are the remaining migrations. |
| A2 | **I/O sources are single-run.** A second `run()` errors; classic re-runs. | 🔴 | A *consequence* of A1 (the channel/waker/thread is consumed). Real superset gap. Still open: `source_at_start` (#547) defers establishment but inherits the single-run channel, so even `zmq_sub` is not yet re-runnable — re-run is the tracked follow-on (port-plan §0.4 reopened). → plan. |
| A3 | **`zmq_pub` binds its socket lazily on its first cycle**, not `start()`. | 🟡 | Another "when does I/O happen" quirk; interacts with the slow-joiner window. |
| A4 | **Error-surfacing timing shifted to wiring** — connection errors surface during graph construction, not at run start / first op. | 🟡 | Still applies to the eager-connect sources/sinks in A1. **Resolved for `zmq_sub`:** a `source_at_start` setup error now aborts at run-start with node context (classic-consistent) (#547). Deferring the rest moves them the same way. |
| A5 | ~~**Caller-owned tokio runtime**~~ — etcd/redis/kafka/postgres/otlp took `&Handle`. | ✅ | **Resolved (#548).** The `GraphBuilder` now owns one tokio runtime (lazy, shared, dropped at teardown) with a `with_async_runtime` override; the `&Handle` param is gone from every async factory. Decision record: [`runtime-ownership.md`](./runtime-ownership.md). *Residual (A5a below) is unchanged.* |
| A5a | **"Drive from a non-async thread."** The `block_on` sinks/readers still panic if the graph is built/run/dropped inside an async context. | 🟡 | Inherent to `block_on`-on-the-graph-thread (an owned runtime doesn't change it — its workers are separate threads either way). Documented per-adapter; matches classic's constraint. Not removed by #548. |
| A6 | **channel-sub establishes slower than classic's `ReceiverStream`** (the zmq first-message test needed a ~600 ms settle vs classic's 200 ms). | ❓ | Behavioural difference; **mechanism not pinned** (see the zmq fix PR #542 discussion). Worth a measured investigation. |

## B. Behavioural / capability deviations — need a decision

| id | dev | class | notes / source |
|---|---|:--:|---|
| B1 | **`etcd_pub` blocks the graph thread** — `Handle::block_on` per burst. The `consume_async` reland was deferred. | 🔴 | The one place next puts blocking network I/O on the single-threaded engine. `adapters/etcd.rs` "Why `etcd_pub` still blocks the graph thread (deferred follow-up)". Needs a teardown-hook story for synchronous-error ops (the general problem `consume_async` couldn't cover — the `force:false` conditional write). |
| B2 | **Live sources reject `RunMode::HistoricalFrom` at wiring** (etcd/redis/kafka/postgres `_sub`, zmq_sub). | 🟡✅ | **Ratified — accepted deviation.** Classic *technically permitted* a historical run with wall-clock timestamps, but a live tail has no deterministic timeline to replay: wall-clock-stamped rows give neither reproducible values nor reproducible tick times, so the classic path was an unguarded footgun, not a real capability. Deterministic historical replay is served by the paired time-sliced `_read` sources (e.g. `postgres_read`). next rejects at wiring with a clear message pointing to the `_read` source — a strictly better failure mode than the block-collect deadlock at `start`. Wall-clock historical is **not** restored. |
| B3 | **`kafka_pub` produces sequentially** (single ordered `consume_async` consumer, N roundtrips/burst) vs classic's concurrent `FuturesUnordered` (one roundtrip/burst). | 🟡 | Order-preserving but a throughput deviation. `adapters/kafka.rs` deviation #4. |
| B4 | **csv reads the whole file up front** vs classic's lazy row streaming. | 🟡 | Unbounded-memory for a huge file under `RunFor::Forever`. `adapters/csv.rs` "consequences of using the channel source". |
| B5 | **`postgres_read` queries all slices at wiring** onto `replay_results` vs classic's lazy `produce_async`. | 🟢🟡 | "Identical for a bounded historical run," but buffers the whole result set (memory). Same family as A1. `adapters/postgres.rs` deviation #2. |

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
| D5 | **otlp pins opentelemetry 0.32; classic still on 0.28.** | 🟡 | Deliberate divergence for security (GHSA-w9wp-h8wv-79jx, #543). **Drift** — bump classic to match to restore lockstep + fix the advisory there. |
| D6 | csv malformed-row error surfaces at replay start vs classic mid-stream. | 🟢 | Same run-failure outcome + context string; documented. |
| D7 | `FileCache` log messages drop the classic "KDB " prefix. | 🟢 | The cache isn't kdb-specific in next. |

---

## Recommended priorities

1. **A1 / A2 (+ B5)** — the defer-to-start plan. The primitive (`source_at_start`)
   and the `zmq_sub` migration landed (#547); finish it by migrating the
   `produce_async` family + the plain `external`/`channel` feeders, then reopen
   §0.4 to make I/O sources **re-runnable** (A2) — the remaining structural win.
2. **B1** — get `etcd_pub` off the graph thread. Blocked on a teardown-hook story
   for synchronous-error ops; solving it generalises `consume_async`.
3. **B2** — ratify historical-rejection for the cutover superset claim (accept as
   documented, or restore wall-clock historical for live sources).
4. **D5** — bump classic's opentelemetry to restore lockstep with next.
5. **B3 / B4** — decide whether the throughput (kafka) / memory (csv) deviations
   matter for the "strict superset" claim, or are acceptable documented trade-offs.
6. **A6** — measure the channel-sub startup latency to either explain or close it.

**Resolved since this register was written:** **A5** (graph-owned runtime, #548);
**A1/A4 for `zmq_sub`** (deferred to `start()` via `source_at_start`, #547).

## Keeping this current

Re-run the two greps in **Sources** above after each adapter/engine PR and add
any new `# Deviations from classic` entry here with a class. Category A is a
manual audit — re-examine it whenever the source/sink lifecycle or the channel
machinery changes. At cutover, every 🔴 and 🟡 needs an explicit ruling.
