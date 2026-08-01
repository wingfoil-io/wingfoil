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
| A1 | **Wiring-time I/O establishment.** Every adapter now establishes its I/O at run start, not wiring. The only "wiring-time establishment" left is the `external` + plain `channel` feeders (caller-owned producers), which is the single-run question (see A2 — now confirmed **parity**, classic is single-run for I/O too), not a connect-at-wiring deviation. | ✅ | Classic connects in `setup`/`start`. Causes: side-effecting/untestable wiring, a "wired but not running" window, realtime pre-run message accumulation. → defer-to-start plan. **Resolved across every I/O adapter:** the `source_at_start` primitive landed and `zmq_sub` migrated (#547); the **`produce_async` family (etcd/kafka/redis/postgres `_sub`)** spawns its producer in `start()` (deferred via `source_at_start_with_params`), `produce_async` no longer takes `RunParams`, and the `_sub` sources take only a `RunMode` (for the wiring historical rejection). **Every sink is migrated:** the streaming sinks connect lazily inside their `consume_async` consumer on the first write — **`postgres_write` (#577)**, **`redis_pub` / `redis_stream_write`** (#578), **`etcd_pub`** (connect + `lease_grant` + keepalive on first write; the lease revoke stays a teardown-time graph-thread `block_on`, the ordinary A5a footgun); **`kafka_pub`** was already compliant (`ClientConfig::create()` opens no socket; librdkafka connects lazily on first `send()`). **`postgres_read`** now defers its connect + slice queries to the run via `produce_async` (B5) — the window is still validated + sliced at wiring, a pure check. **`prometheus::serve`** is classic-parity (synchronous pre-run bind, like classic; the `anyhow::Result` return is **D2**). Only the re-run gap (A2) and the caller-owned `external`/`channel` feeders remain, both tracked under A2. |
| A2 | ~~**I/O sources are single-run.** A second `run()` errors; classic re-runs.~~ | ✅ | **Not a gap — parity, verified against classic source.** The premise ("classic re-runs") was **wrong**: classic is *also* single-run for I/O sources. Classic's `AsyncProducerStream::setup` (`wingfoil/src/nodes/async_io.rs:214`) takes its `func` (an `FnOnce`) and sender with `.take().ok_or_else(\|\| "func is already taken")?`, so a second `run()` (classic builds a fresh `Graph` over the shared nodes each call) **errors** — the etcd/kafka/redis/postgres/`produce_async` family; and `ChannelReceiverStream::setup` (`nodes/channel.rs:257`) `.take()`s its notifier and drains its receiver on the first run, so a channel/external source produces nothing on a second. next's explicit single-run error is therefore parity — and strictly clearer than classic's silent-nothing on the channel path. The **deterministic** subset (tickers/constants/combinators/feedback) re-runs in *both* (next via the `reset` hook, #Phase-1) — that parity is real and already delivered. Re-run of I/O sources was never a classic capability, so nothing to port; `port-plan.md §0.4` already records I/O graphs as single-run by decision. |
| A3 | ~~**`zmq_pub` binds its socket lazily on its first cycle**, not `start()`.~~ | ✅ | **Resolved.** `zmq_pub` now binds its `PUB` socket (and runs the run-mode check) at graph `start()` via `compose_spawn_at_start`, matching classic's `start`. This is what closes A6: the subscriber connects and propagates its subscription filter during the startup window instead of racing the first publish. `adapters/zmq.rs`. |
| A4 | **Error-surfacing timing shifted to wiring** — connection errors surface during graph construction, not at run start / first op. | ✅ | **Resolved for every I/O adapter.** `zmq_sub` (#547), the **`produce_async` `_sub` family**, and all the sinks — **`postgres_write` / `redis_pub` / `redis_stream_write` / `etcd_pub`** (each connects during the run, on the consumer's first write) and **`kafka_pub`** (never connected at wiring — librdkafka connects lazily on first `send()`): a connect/subscribe/lease failure now surfaces during the run (via `send_error` / the `consume_async` error channel), not at wiring — classic-consistent. (The historical-mode *rejection* for the `_sub` sources still fires at wiring, a deliberate fail-fast; `postgres_read`'s connect is at wiring but that's the B5 whole-query-at-wiring item, not a live connection.) |
| A5 | ~~**Caller-owned tokio runtime**~~ — etcd/redis/kafka/postgres/otlp took `&Handle`. | ✅ | **Resolved (#548).** The `GraphBuilder` now owns one tokio runtime (lazy, shared, dropped at teardown) with a `with_async_runtime` override; the `&Handle` param is gone from every async factory. Decision record: [`runtime-ownership.md`](./runtime-ownership.md). *Residual (A5a below) is unchanged.* |
| A5a | **"Drive from a non-async thread."** The `block_on` sinks/readers still panic if the graph is built/run/dropped inside an async context. | 🟡 | Inherent to `block_on`-on-the-graph-thread (an owned runtime doesn't change it — its workers are separate threads either way). Documented per-adapter; matches classic's constraint. Not removed by #548. |
| A6 | ~~**channel-sub establishes slower than classic's `ReceiverStream`** (the zmq first-message test needed a ~600 ms settle vs classic's 200 ms).~~ | ✅ | **Resolved — mechanism pinned.** The flakiness was **A3**, not channel-sub latency: because `zmq_pub` bound its socket lazily on the *first publish* (after the test's `SUB_SETTLE` delay), the subscriber could not connect during the settle window, so the whole slow-joiner handshake was compressed into the first publish and rested on the adapter's ~50 ms post-accept margin — which lost the first few messages under CI (coverage) load. Binding at `start()` (A3 fix) makes the settle window effective; `first_message_not_dropped` now passes 25/25 under CPU load. `tests/zmq_integration.rs`. |
| A7 | ~~**`custom_node` ignored the `always` activation bit** — a busy-spin `custom_node` (`Activation::ALWAYS`) did not set the engine's `has_always` flag, so the realtime kernel parked between cycles and the node only fired on unrelated wakeups.~~ | ✅ | **Resolved (fix adapter port).** `GraphBuilder::custom_node` accepted an `Activation` but, unlike `poll`, never set `has_always`, so an `ALWAYS` custom node (a socket-polling source) was never driven each cycle. `custom_node` now sets `has_always` when `activation.always` (matching `poll`), flipping the kernel into its busy-spin loop. Surfaced by the FIX `AlwaysSpin` source (a busy-spin `custom_node` reading a non-blocking socket); guarded by `tests/fix_integration.rs::fix_same_process_spin`. `interp.rs`. |

## B. Behavioural / capability deviations — need a decision

| id | dev | class | notes / source |
|---|---|:--:|---|
| B1 | ~~**`etcd_pub` blocks the graph thread** — `Handle::block_on` per burst.~~ | ✅ | **Resolved.** `consume_async` now returns a `flush` teardown (wired via `finally`) that closes the sink, joins the consumer task, and — unlike a `Drop` — surfaces the **final** write error as the run's `Err`. This is the "teardown-hook story for synchronous-error ops" B1 was blocked on: it lets `etcd_pub`'s `force:false` conditional abort a single-cycle run at teardown (exactly as classic's `AsyncConsumerNode::teardown` does), so the per-write `block_on` is gone and the PUTs run off the graph thread on the shared consumer task. The wiring-time connect + `LeaseGuard` revoke keep their (teardown-time, graph-thread) `block_on` — the ordinary `consume_async` footgun (A5a). The same `flush` upgrade also closes the "final-cycle write error swallowed" gap for kafka/postgres/redis/otlp. |
| B2 | **Live sources reject `RunMode::HistoricalFrom` at wiring** (etcd/redis/kafka/fluvio/postgres `_sub`, zmq_sub, kdb_sub, aeron_sub_fragment, iceoryx2_sub). | 🟡 | Live tails are unbounded wall-clock streams; a historical run would block-collect the whole stream and deadlock at `start`, so next rejects at wiring with a pointer to the bounded reader. **Classic parity for postgres and kdb** — classic `postgres_sub` already required `RunMode::RealTime` and bailed otherwise (`adapters/postgres/sub.rs`), and classic `kdb_sub` likewise bailed unless `RunMode::RealTime` (`adapters/kdb/sub.rs`), so next's rejection is parity for both, *not* a deviation; the "classic permitted a wall-clock historical run" gap is real only for `zmq_sub` and the etcd/redis/kafka/fluvio `_sub` sources, which had no such guard. **Split ruling (agreed plan below):** (a) **ratified reject** for live-only sources with no bounded historical twin (`zmq_sub`, `etcd_sub`, both redis sources, `kafka_sub` today); (b) for adapters that *do* have a bounded historical read alongside the live tail, replace the mode-locked `_read`/`_sub` function pair with a single mode-agnostic `<adapter>_source` that dispatches on `run_mode`. postgres did this first, and `kafka_source` / `fluvio_source` have since landed with a live half only (historical errors at wiring naming the unimplemented half, so call sites are already stable for when a bounded reader lands); **kdb keeps the classic separate `kdb_read`/`kdb_sub` shape** (classic parity — the two are genuinely different mechanisms, a time-sliced historical query vs a tickerplant push tail), so a unified `kdb_source` is a possible follow-up, not a parity gap. → plan §B2. |
| B3 | ~~**`kafka_pub` produces sequentially** (N roundtrips/burst) vs classic's concurrent `FuturesUnordered` (one roundtrip/burst).~~ | ✅ | **Resolved.** Added `consume_async_bursts` — a variant that hands the consumer a whole burst at a time (order preserved *across* bursts, concurrency within one left to the sink). `kafka_pub` now drains a burst's sends together via `FuturesUnordered` (~one broker roundtrip/burst), at throughput parity with classic. `adapters/kafka.rs` sink docs. |
| B4 | ~~**csv reads the whole file up front** vs classic's lazy row streaming.~~ | ✅ | **Resolved (bounded lazy replay).** `csv_read` moved off `replay_results` (which drained the whole file onto the channel at wiring) to a lazy `produce_async` producer (with a `buffer_size` bound): the file is opened at wiring (fail-fast) but its rows are deserialized **on demand** and delivered as the graph drains, so a huge file is never read into memory up front. Gains a `buffer_size` param for the group-aware back-pressure landed in B5 (a same-time burst of any size rides one slot, so it can't deadlock). A malformed row now aborts the run **mid-stream** as the reader reaches it (closer to classic than the old up-front surfacing). The `csv` feature now pulls `async` (a documented dep gain, as `cache`/`kdb` do); the file I/O is synchronous on the producer task. `adapters/csv.rs`, `tests/csv_adapter.rs::csv_read_bounded_is_deterministic_and_survives_large_bursts`. **`lines` given the same treatment:** `replay_lines` / `replay_lines_scheduled` moved off `replay_results` to the same lazy `produce_async` + `buffer_size` shape (over a synchronous line iterator via `futures::stream::iter` — no `async_stream` needed), gated behind the `async` feature so the dependency-free `tail_lines` source + file sink stay in the default build. `adapters/lines.rs`, `tests/lines_adapter.rs::replay_bounded_is_deterministic_and_survives_large_bursts`. Both file readers now match the network readers' bounded-lazy model; nothing on `replay_results` reads an unbounded external resource anymore (its remaining users are finite in-memory test/example fixtures). |
| B5 | ~~**`postgres_read` queries all slices at wiring** onto `replay_results` vs classic's lazy `produce_async`.~~ | ✅ | **Resolved, then fully closed (bounded historical back-pressure landed).** `postgres_read` first deferred its connect + slice queries to the run via `produce_async` (the A1 wiring-side-effect), but still collected the whole result set into a `Vec` up front — memory unbounded in historical, because `produce_async`'s historical path had **no back-pressure** (the permit throttle was realtime-only, a stale guard from the pre-`pump_historical` block-collect receiver). That gap is now closed: **`produce_async` applies `buffer_size` back-pressure in *both* run modes** (the interim two-function split — `produce_async` / `produce_async_bounded` — was subsequently unified back to legacy's single `produce_async(g, run, buffer_size)` signature; `None` = unbounded, `Some(n)` = bounded). The producer paces itself against the graph's incremental `pump_historical` drain via a `tokio::sync::Semaphore` — one permit per value (realtime) or per timestamp-group (historical); the passthrough adds permits back per delivered unit; the budget is floored to 2 (the receiver reads one group past `now` to close a same-time burst). The time-sliced readers (`postgres_read`, `kdb_read`, `kdb_read_cached`) were **lazified** into `async_stream` generators (classic's `chunk_stream` shape — one query per slice, pulled only as the graph drains), so a `buffer_size` bound now gives legacy's bounded-memory, pipelined historical replay — not an up-front collection. `postgres_read` / `kdb_read` take a `buffer_size`; `kdb_read_cached` stays unbounded like classic but streams lazily. `async_source.rs`, `adapters/postgres.rs` deviation #2, `adapters/kdb.rs` deviation #2/#5, `tests/produce_async.rs` (`produce_async_bounded_historical_is_deterministic`, `..._large_same_time_burst_no_deadlock`). **B4 (csv + lines) was then fixed the same way** — both file readers moved off `replay_results` to a lazy `produce_async` + `buffer_size` (see B4). |
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
  producers that fill the buffer before the run starts — the only ones left are
  `replay_results`' finite in-memory fixtures, which stay on the unbounded path.
  csv, lines, `postgres_read`, and `kdb_read` moved **off** that pre-queue model
  to a lazy `produce_async` + `buffer_size` producer (B4/B5), so they no longer
  fill the buffer up front.

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
- **kafka**, **fluvio** — ✅ **the `_source` entry point has landed for both**
  (`kafka_source` + `KafkaSourceConfig`, `fluvio_source` + `FluvioSourceConfig`),
  carrying a **live half only**. Under `RunMode::RealTime` they dispatch to
  `kafka_sub` / `fluvio_sub`; under `RunMode::HistoricalFrom` they error at
  wiring naming the *unimplemented* historical half — deliberately **not**
  pointing at a `.historical(..)` builder that does not exist (pinned by a
  negative assertion in each adapter's tests). The point is call-site stability:
  the mode choice already lives at `run()`, so when the bounded reader lands it
  becomes a `.historical(..)` builder and no existing call site changes. Both
  take the full `RunParams` (only `run_mode` is read today) so even the signature
  is already the one a historical half needs. `adapters/kafka.rs`,
  `adapters/fluvio.rs`.

  The **bounded reader itself is still open**, and is superset work rather than
  a parity gap — classic never offered a bounded kafka or fluvio replay, so
  nothing regresses if it never lands. Each needs three things, not one:
  (i) a terminating offset/time-range reader on the lazy `async_stream` +
  `buffer_size` shape B5 gave `postgres_read`; (ii) a **record timestamp** on
  `KafkaEvent` / `FluvioEvent` — both today are stamped `NanoTime::now()` at
  yield, which is useless for a monotonic replay; (iii) for kafka only, a
  timestamp-ordered **merge across partitions** (postgres never had this — one
  connection, one `ORDER BY time`) plus explicit partition assignment instead of
  consumer-group subscription. Fluvio is roughly half the work (start offset
  exists, single-partition surface, no consumer-group semantics to defeat), so
  it is the cheaper proof point. Note the determinism caveat when it does land:
  a log replay is only reproducible while retention and compaction leave the
  range intact — weaker than a time-sliced query over an append-only table.
- **Live-only, reject ratified as-is:** `zmq_sub`, `etcd_sub`, both redis
  sources (`redis_sub` Pub/Sub and `redis_stream_read` are live/unbounded with
  no historical timeline), `aeron_sub_fragment` (a shared-memory/UDP
  subscription with no addressable past), and the iceoryx2 subscribers (a
  shared-memory subscription has no addressable past beyond the publisher's
  small retained history). Nothing to dispatch to under historical, so the
  wiring-time reject is the honest behaviour and B2 is accepted for these.

## C. Capability gaps — tracked, deferred by design

| id | gap | class | notes |
|---|---|:--:|---|
| C1 | ~~**otlp trace/span export (`OtlpSpans`) not ported.**~~ | ✅ | **Resolved.** `OtlpSpanOps::otlp_spans` is fully ported — it landed with the Phase-5 latency infrastructure (`Traced`/`HasLatency`/`latency_stages!`): one parent span per tick + one child span per stage hop, caller-supplied attributes via `OtlpAttributeBuffer`, silent skip of all-zero/backwards timestamps, same off-thread `consume_async` model as `otlp_push`. Metrics push was already at full parity. `adapters/otlp.rs` deviation #3; port-plan Phase-4 otlp "Trace/span export ✅ ported". |
| C2 | **zmq cross-language interop not ported.** | ⚪ | The `bincode` wire envelope is next-local; not wire-compatible with a classic/Python peer. Deferred with the Python bindings (Phase 6). `adapters/zmq.rs`. |
| C3 | **Structural gaps** — multi-output islands; runtime graph-mutation surface; (closed: `StreamStore`, `demux_it` in #529). | ⚪ | See `port-plan.md` Phase 4.5 "Known parity gaps" and Phase 1 multi-output note. |
| C4 | **Compiled-path IO ingestion** — busy-poll (`ALWAYS`) sources + bursts are not expressible in `compiled()`/`graph!`. | ⚪ | Deliberate exclusion; IO stays at the interpreted boundary + compiled islands. Full design + the gating decision (wake channel vs busy-spin) in `port-plan.md` "Deferred / post-v1 work". (was #502/#503) |
| C5 | ~~**augurs ports only `augurs_forecast` + `augurs_outlier`; `augurs_changepoint` / `augurs_seasons` / `augurs_dtw` / `augurs_cluster` are not ported.**~~ | ✅ | **Resolved.** All 6 of classic's operators are ported: the four remaining ones landed as sliding-window transform ops in the same shape as the first two — `AugursChangepointOps::augurs_changepoint` (BOCPD, window-start index dropped), `AugursSeasonsOps::augurs_seasons` (periodogram, detector built once at wiring), `AugursDtwOps::augurs_dtw` (pairwise DTW distance matrix, Euclidean/Manhattan) and `AugursClusterOps::augurs_cluster` (DBSCAN over those distances). The `augurs` dep gained classic's `changepoint`/`seasons`/`dtw`/`clustering` sub-features. Classic's unit tests for all four are ported to `tests/augurs_adapter.rs`, and the example covers them. One new benign deviation, D12. `adapters/augurs.rs`; port-plan Phase-4 augurs. |

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
| D10 | **kdb: credentials never reach an error message** — `KdbConnection::redacted()` returns `host:port` (the password is used only at the `QStream::connect` call site). | 🟢 | Credential-redaction rule (shared with postgres). Classic put no creds in error context either; next makes it explicit + test-pinned. |
| D11 | **kdb: `kdb_read` takes classic's `buffer_size`.** | 🟢 | `kdb_read` is historical-only. The bound is **no longer inert**: after B5 (bounded historical back-pressure + lazy `chunk_stream` slicing) `buffer_size` now gives bounded-memory, pipelined historical replay (`Some(n)` paces slice fetches against the graph drain; `None` = unbounded, classic's default). `kdb_read_cached` stays unbounded like classic but streams lazily. The `kdb_write` sink takes a `buffer_size` per D3. |
| D12 | **`augurs_cluster` floors its effective window at the two-sample warm-up.** Classic's cluster node sizes its buffer for two samples but evicts against the raw `window`, so a `window` of 1 never reaches the warm-up and the node never ticks; next grows the effective window to the floor (as classic's own `augurs_dtw` already does). | 🟢 | Only reachable for `window < 2`, where classic's behaviour is silent-never-tick. Same "grow the window to the model's floor so the node still emits" rule the forecast/seasons ops follow in both trees. `adapters/augurs.rs`; `tests/augurs_adapter.rs::cluster_window_below_floor_still_emits`. |
| D9 | **`logged` always wires its node; classic skips it when the level is disabled.** Classic `logged` short-circuits at wiring (`log_enabled!` → return the source unchanged) and reads the tick time via `bimap` + `ticked_at_elapsed`; next always wires the op, leaning on `log!`'s internal enabled-check, and reads the time off `Ctx::time`. | 🟢 | Value stream identical (a pass-through); only the diagnostic differs, and a disabled level still costs only the internal `log!` check. Always-wiring keeps interpreted and compiled identical (a wiring-time skip is inexpressible in `compiled()`). `logged` is fluent-only (its `&str` param vs `String` cfg — see `tests/op_completeness.rs`). `ops.rs::Logged`. |

---

## Recommended priorities

1. **B4** — decide whether the csv whole-file memory deviation matters for the
   "strict superset" claim, or is an acceptable documented trade-off.
2. **A6 is closed** (pinned to A3); **A2 is closed** (parity — classic is
   single-run for I/O sources too, verified against classic source). The
   defer-to-start plan (A1) is complete. What's left is the non-code rulings
   (B4, and the cutover-audit sweep of any remaining 🟡).

**Resolved / ratified since this register was written:** **A5** (graph-owned
runtime, #548); **A1/A4 for `zmq_sub`** (deferred to `start()` via
`source_at_start`, #547); **B1** (`consume_async` `flush` teardown surfaces
final-cycle write errors, so `etcd_pub` moved off the graph thread — also closes
the swallowed-final-error gap for kafka/postgres/redis/otlp); **B2** (split
ruling, #557: reject ratified for live-only sources; adapters with a bounded
historical twin move to a unified mode-dispatching `<adapter>_source` — see
plan §B2, postgres first); **B3** (`consume_async_bursts` restores kafka's
concurrent per-burst publish); **A3 / A6** (`zmq_pub` binds at `start()`, which
pins and fixes the `first_message_not_dropped` slow-joiner flakiness); **A1 /
A4** (defer-to-start complete for every I/O adapter — all `_sub` sources, all
sinks, and `postgres_read` (B5) now establish I/O at run start, so no adapter
touches the network at wiring); **A2** (single-run I/O sources is **parity**, not
a gap — classic also throws / produces nothing on a second `run()` of an I/O
source, verified against `wingfoil/src/nodes/async_io.rs` + `channel.rs`; the
register's "classic re-runs" was incorrect; the deterministic re-run subset was
already at parity via the Phase-1 `reset` hook); **C1** (otlp trace/span export
`OtlpSpanOps::otlp_spans` fully ported, landed with the Phase-5 latency
infrastructure — was a ⚪ capability gap, now at metrics-parity); **C5** (augurs
`changepoint` / `seasons` / `dtw` / `cluster` ported, taking the adapter to all 6
of classic's operators — was a ⚪ capability gap, now full-parity but for the
benign D12 window floor).

## Keeping this current

Re-run the two greps in **Sources** above after each adapter/engine PR and add
any new `# Deviations from classic` entry here with a class. Category A is a
manual audit — re-examine it whenever the source/sink lifecycle or the channel
machinery changes. At cutover, every 🔴 and 🟡 needs an explicit ruling.
