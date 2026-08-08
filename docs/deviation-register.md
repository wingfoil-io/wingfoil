# wingfoil → legacy: deviation register

Status: **the cutover audit has run — every 🔴 and 🟡 was ruled on 2026-08-03**
(reasoning in [`cutover-plan.md`](./cutover-plan.md) §2). This stays a living
checklist: every place wingfoil's behaviour or surface deviates from the legacy
`wingfoil` tree, collected in one place and classified, so a *new* deviation
arriving after that audit is still caught and still gets an explicit accept/fix
ruling.

**Re-swept 2026-08-04.** The standing obligation below was discharged against
the current tree: the Source-(1) grep was re-run, every adapter's module block
diffed against this register, and the engine/example/adapter inventories
compared against `legacy/` directly. **No new capability gap was found** — all
16 legacy adapters are ported (plus `lines`, which has no legacy twin), every
legacy example has a wingfoil twin, and the op catalog stays gated by
`tests/op_completeness.rs` + `tests/catalog*.rs`. The sweep did find that
**four adapters ported after the original sweep — `aeron`, `fix`, `iceoryx2`
and `web` — had never had their module blocks folded in** (`web` was not
mentioned in this file at all; `aeron`/`iceoryx2` appeared only as names inside
B2's list; `fix` only via A7). Those are now recorded as D17–D23, and `fix` has
been added to B2's enumeration. One row was also **factually stale**: A4's
parenthetical claimed `postgres_read` still connects at wiring, which
contradicted A1 in the same table and the code — `adapters/postgres.rs`
defers the connect into the `produce_async` closure. All of the newly recorded
rows classify 🟢; the sweep produced no new 🔴 or 🟡.

**Second pass, same day** (an independent adapter-surface audit run in
parallel, reconciled afterwards). It found four items the pass above did not,
recorded as **D24–D27**, and **two of them are 🟡** — so the "no new 🟡"
statement above holds for D17–D23 only:

- **D24** (🟡) — the `aeron-rs` lock on the graph thread. Legacy parity, but it
  is the tree's one sanctioned exception to the no-locks invariant and an audit
  will trip over it.
- **D25** (🟡) — FIX session validation (#704, which landed between the two
  passes). New capability, yet the one place the trees behave differently on
  identical input.
- **D26**, **D27** (🟢) — FIX sequence-number persistence; the `otlp_spans`
  argument reorder.

It also found **D6 factually stale** — B4's lazy `csv_read` had already moved
the malformed-row error mid-stream — and corrected two matching claims in
`port-plan.md` plus the `postgres_read` bullet in `adapters/mod.rs`, all three
of which still described the pre-B5 `replay_results` mechanism. The lesson is
in "Keeping this current" below: **a resolved B-row can silently strand a D-row,
and nothing flags it.**

**Sources:** (1) each ported adapter's `# Deviations from legacy` module-doc
block — regenerate with
`git grep -n "Deviations from legacy" crates/wingfoil/src/adapters/*.rs crates/wingfoil/src/adapters/*/mod.rs`.
Restrict the grep to the **module** files: since the per-adapter `CLAUDE.md`
files landed, a bare `git grep … crates/wingfoil/src/adapters` also returns
their "Canonical list:" pointer sections, which are summaries of the module
block and not themselves sources. (2) `docs/port-plan.md` (capability matrix +
Phase 4.5 "Known parity gaps");
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
| A1 | **Wiring-time I/O establishment.** Every adapter now establishes its I/O at run start, not wiring. The only "wiring-time establishment" left is the `external` + plain `channel` feeders (caller-owned producers), which is the single-run question (see A2 — now confirmed **parity**, legacy is single-run for I/O too), not a connect-at-wiring deviation. | ✅ | Legacy connects in `setup`/`start`. Causes: side-effecting/untestable wiring, a "wired but not running" window, realtime pre-run message accumulation. → defer-to-start plan. **Resolved across every I/O adapter:** the `source_at_start` primitive landed and `zmq_sub` migrated (#547); the **`produce_async` family (etcd/kafka/redis/postgres `_sub`)** spawns its producer in `start()` (deferred via `source_at_start_with_params`), `produce_async` no longer takes `RunParams`, and the `_sub` sources take only a `RunMode` (for the wiring historical rejection). **Every sink is migrated:** the streaming sinks connect lazily inside their `consume_async` consumer on the first write — **`postgres_write` (#577)**, **`redis_pub` / `redis_stream_write`** (#578), **`etcd_pub`** (connect + `lease_grant` + keepalive on first write; the lease revoke stays a teardown-time graph-thread `block_on`, the ordinary A5a footgun); **`kafka_pub`** was already compliant (`ClientConfig::create()` opens no socket; librdkafka connects lazily on first `send()`). **`postgres_read`** now defers its connect + slice queries to the run via `produce_async` (B5) — the window is still validated + sliced at wiring, a pure check. **`prometheus::serve`** is legacy-parity (synchronous pre-run bind, like legacy; the `anyhow::Result` return is **D2**). Only the re-run gap (A2) and the caller-owned `external`/`channel` feeders remain, both tracked under A2. |
| A2 | ~~**I/O sources are single-run.** A second `run()` errors; legacy re-runs.~~ | ✅ | **Not a gap — parity, verified against legacy source.** The premise ("legacy re-runs") was **wrong**: legacy is *also* single-run for I/O sources. Legacy's `AsyncProducerStream::setup` (`legacy/wingfoil/src/nodes/async_io.rs:214`) takes its `func` (an `FnOnce`) and sender with `.take().ok_or_else(\|\| "func is already taken")?`, so a second `run()` (legacy builds a fresh `Graph` over the shared nodes each call) **errors** — the etcd/kafka/redis/postgres/`produce_async` family; and `ChannelReceiverStream::setup` (`nodes/channel.rs:257`) `.take()`s its notifier and drains its receiver on the first run, so a channel/external source produces nothing on a second. Wingfoil's explicit single-run error is therefore parity — and strictly clearer than legacy's silent-nothing on the channel path. The **deterministic** subset (tickers/constants/combinators/feedback) re-runs in *both* (wingfoil via the `reset` hook, #Phase-1) — that parity is real and already delivered. Re-run of I/O sources was never a legacy capability, so nothing to port; `port-plan.md §0.4` already records I/O graphs as single-run by decision. |
| A3 | ~~**`zmq_pub` binds its socket lazily on its first cycle**, not `start()`.~~ | ✅ | **Resolved.** `zmq_pub` now binds its `PUB` socket (and runs the run-mode check) at graph `start()` via `compose_spawn_at_start`, matching legacy's `start`. This is what closes A6: the subscriber connects and propagates its subscription filter during the startup window instead of racing the first publish. `adapters/zmq.rs`. |
| A4 | **Error-surfacing timing shifted to wiring** — connection errors surface during graph construction, not at run start / first op. | ✅ | **Resolved for every I/O adapter.** `zmq_sub` (#547), the **`produce_async` `_sub` family**, and all the sinks — **`postgres_write` / `redis_pub` / `redis_stream_write` / `etcd_pub`** (each connects during the run, on the consumer's first write) and **`kafka_pub`** (never connected at wiring — librdkafka connects lazily on first `send()`): a connect/subscribe/lease failure now surfaces during the run (via `send_error` / the `consume_async` error channel), not at wiring — legacy-consistent. (The historical-mode *rejection* for the `_sub` sources still fires at wiring, a deliberate fail-fast — and for the FIX sources it is *earlier* than legacy, which checked real-time-ness at run `start()`; same for `fluvio_sub`'s negative-`start_offset` check. Both are pure validations moved wiring-ward, recorded as **D20**.) **Corrected 2026-08-04:** this row previously ended "…`postgres_read`'s connect is at wiring but that's the B5 whole-query-at-wiring item, not a live connection", which was wrong and contradicted A1. B5 moved `postgres_read`'s connect *into* the `produce_async` closure — `adapters/postgres.rs` opens the connection inside `move \|_params: RunParams\| async move { tokio_postgres::connect(..) }`, so nothing touches the network at wiring. Only the window validation + slice computation stay at wiring, and both are pure. |
| A5 | ~~**Caller-owned tokio runtime**~~ — etcd/redis/kafka/postgres/otlp took `&Handle`. | ✅ | **Resolved (#548).** The `GraphBuilder` now owns one tokio runtime (lazy, shared, dropped at teardown) with a `with_async_runtime` override; the `&Handle` param is gone from every async factory. Decision record: [`runtime-ownership.md`](./runtime-ownership.md). *Residual (A5a below) is unchanged.* |
| A5a | **"Drive from a non-async thread."** The `block_on` sinks/readers still panic if the graph is built/run/dropped inside an async context. | 🟡 | Inherent to `block_on`-on-the-graph-thread (an owned runtime doesn't change it — its workers are separate threads either way). Documented per-adapter; matches legacy's constraint. Not removed by #548. **Cutover ruling 2026-08-03: accepted as legacy-parity** (cutover-plan 2.5) — legacy drives its async adapters through `block_on` on the graph thread identically, so the constraint is inherited, not introduced. |
| A6 | ~~**channel-sub establishes slower than legacy's `ReceiverStream`** (the zmq first-message test needed a ~600 ms settle vs legacy's 200 ms).~~ | ✅ | **Resolved — mechanism pinned.** The flakiness was **A3**, not channel-sub latency: because `zmq_pub` bound its socket lazily on the *first publish* (after the test's `SUB_SETTLE` delay), the subscriber could not connect during the settle window, so the whole slow-joiner handshake was compressed into the first publish and rested on the adapter's ~50 ms post-accept margin — which lost the first few messages under CI (coverage) load. Binding at `start()` (A3 fix) makes the settle window effective; `first_message_not_dropped` now passes 25/25 under CPU load. `tests/zmq_integration.rs`. |
| A7 | ~~**`custom_node` ignored the `always` activation bit** — a busy-spin `custom_node` (`Activation::ALWAYS`) did not set the engine's `has_always` flag, so the realtime kernel parked between cycles and the node only fired on unrelated wakeups.~~ | ✅ | **Resolved (fix adapter port).** `GraphBuilder::custom_node` accepted an `Activation` but, unlike `poll`, never set `has_always`, so an `ALWAYS` custom node (a socket-polling source) was never driven each cycle. `custom_node` now sets `has_always` when `activation.always` (matching `poll`), flipping the kernel into its busy-spin loop. Surfaced by the FIX `AlwaysSpin` source (a busy-spin `custom_node` reading a non-blocking socket); guarded by `tests/fix_integration.rs::fix_same_process_spin`. `interp.rs`. |

## B. Behavioural / capability deviations — need a decision

| id | dev | class | notes / source |
|---|---|:--:|---|
| B1 | ~~**`etcd_pub` blocks the graph thread** — `Handle::block_on` per burst.~~ | ✅ | **Resolved.** `consume_async` now returns a `flush` teardown (wired via `finally`) that closes the sink, joins the consumer task, and — unlike a `Drop` — surfaces the **final** write error as the run's `Err`. This is the "teardown-hook story for synchronous-error ops" B1 was blocked on: it lets `etcd_pub`'s `force:false` conditional abort a single-cycle run at teardown (exactly as legacy's `AsyncConsumerNode::teardown` does), so the per-write `block_on` is gone and the PUTs run off the graph thread on the shared consumer task. The wiring-time connect + `LeaseGuard` revoke keep their (teardown-time, graph-thread) `block_on` — the ordinary `consume_async` footgun (A5a). The same `flush` upgrade also closes the "final-cycle write error swallowed" gap for kafka/postgres/redis/otlp. |
| B2 | **Live sources reject `RunMode::HistoricalFrom` at wiring** (etcd/redis/kafka/fluvio/postgres `_sub`, zmq_sub, kdb_sub, aeron_sub_fragment, iceoryx2_sub, **and all four FIX source factories** — `fix_connect`, `fix_accept`, `fix_connect_tls`, `fix_connect_tls_logon` — added to this enumeration in the 2026-08-04 re-sweep; each rejects at wiring with the same "real-time" message, `adapters/fix.rs`). | 🟡 | Live tails are unbounded wall-clock streams; a historical run would block-collect the whole stream and deadlock at `start`, so wingfoil rejects at wiring with a pointer to the bounded reader. **Legacy parity for postgres and kdb** — legacy `postgres_sub` already required `RunMode::RealTime` and bailed otherwise (`adapters/postgres/sub.rs`), and legacy `kdb_sub` likewise bailed unless `RunMode::RealTime` (`adapters/kdb/sub.rs`), so wingfoil's rejection is parity for both, *not* a deviation; the "legacy permitted a wall-clock historical run" gap is real only for `zmq_sub` and the etcd/redis/kafka/fluvio `_sub` sources, which had no such guard. **Split ruling (agreed plan below):** (a) **ratified reject** for live-only sources with no bounded historical twin (`zmq_sub`, `etcd_sub`, both redis sources, `kafka_sub` today); (b) for adapters that *do* have a bounded historical read alongside the live tail, replace the mode-locked `_read`/`_sub` function pair with a single mode-agnostic `<adapter>_source` that dispatches on `run_mode`. postgres did this first, and `kafka_source` / `fluvio_source` have since landed with a live half only (historical errors at wiring naming the unimplemented half, so call sites are already stable for when a bounded reader lands); **kdb keeps the legacy separate `kdb_read`/`kdb_sub` shape** (legacy parity — the two are genuinely different mechanisms, a time-sliced historical query vs a tickerplant push tail), so a unified `kdb_source` is a possible follow-up, not a parity gap. → plan §B2. **Cutover ruling 2026-08-03: the residual is accepted** (cutover-plan 2.4) — verified against legacy source, which exposes only the live `kafka_sub`/`fluvio_sub` tails and no bounded reader at all, so wingfoil's `_source` pair is a strict superset and the unimplemented historical half is new capability, not a parity gap. |
| B3 | ~~**`kafka_pub` produces sequentially** (N roundtrips/burst) vs legacy's concurrent `FuturesUnordered` (one roundtrip/burst).~~ | ✅ | **Resolved.** Added `consume_async_bursts` — a variant that hands the consumer a whole burst at a time (order preserved *across* bursts, concurrency within one left to the sink). `kafka_pub` now drains a burst's sends together via `FuturesUnordered` (~one broker roundtrip/burst), at throughput parity with legacy. `adapters/kafka.rs` sink docs. |
| B4 | ~~**csv reads the whole file up front** vs legacy's lazy row streaming.~~ | ✅ | **Resolved (bounded lazy replay).** `csv_read` moved off `replay_results` (which drained the whole file onto the channel at wiring) to a lazy `produce_async` producer (with a `buffer_size` bound): the file is opened at wiring (fail-fast) but its rows are deserialized **on demand** and delivered as the graph drains, so a huge file is never read into memory up front. Gains a `buffer_size` param for the group-aware back-pressure landed in B5 (a same-time burst of any size rides one slot, so it can't deadlock). A malformed row now aborts the run **mid-stream** as the reader reaches it (closer to legacy than the old up-front surfacing). The `csv` feature now pulls `async` (a documented dep gain, as `cache`/`kdb` do); the file I/O is synchronous on the producer task. `adapters/csv.rs`, `tests/csv_adapter.rs::csv_read_bounded_is_deterministic_and_survives_large_bursts`. **`lines` given the same treatment:** `replay_lines` / `replay_lines_scheduled` moved off `replay_results` to the same lazy `produce_async` + `buffer_size` shape (over a synchronous line iterator via `futures::stream::iter` — no `async_stream` needed), gated behind the `async` feature so the dependency-free `tail_lines` source + file sink stay in the default build. `adapters/lines.rs`, `tests/lines_adapter.rs::replay_bounded_is_deterministic_and_survives_large_bursts`. Both file readers now match the network readers' bounded-lazy model; nothing on `replay_results` reads an unbounded external resource anymore (its remaining users are finite in-memory test/example fixtures). |
| B5 | ~~**`postgres_read` queries all slices at wiring** onto `replay_results` vs legacy's lazy `produce_async`.~~ | ✅ | **Resolved, then fully closed (bounded historical back-pressure landed).** `postgres_read` first deferred its connect + slice queries to the run via `produce_async` (the A1 wiring-side-effect), but still collected the whole result set into a `Vec` up front — memory unbounded in historical, because `produce_async`'s historical path had **no back-pressure** (the permit throttle was realtime-only, a stale guard from the pre-`pump_historical` block-collect receiver). That gap is now closed: **`produce_async` applies `buffer_size` back-pressure in *both* run modes** (the interim two-function split — `produce_async` / `produce_async_bounded` — was subsequently unified back to legacy's single `produce_async(g, run, buffer_size)` signature; `None` = unbounded, `Some(n)` = bounded). The producer paces itself against the graph's incremental `pump_historical` drain via a `tokio::sync::Semaphore` — one permit per value (realtime) or per timestamp-group (historical); the passthrough adds permits back per delivered unit; the budget is floored to 2 (the receiver reads one group past `now` to close a same-time burst). The time-sliced readers (`postgres_read`, `kdb_read`, `kdb_read_cached`) were **lazified** into `async_stream` generators (legacy's `chunk_stream` shape — one query per slice, pulled only as the graph drains), so a `buffer_size` bound now gives legacy's bounded-memory, pipelined historical replay — not an up-front collection. `postgres_read` / `kdb_read` take a `buffer_size`; `kdb_read_cached` stays unbounded like legacy but streams lazily. `async_source.rs`, `adapters/postgres.rs` deviation #2, `adapters/kdb.rs` deviation #2/#5, `tests/produce_async.rs` (`produce_async_bounded_historical_is_deterministic`, `..._large_same_time_burst_no_deadlock`). **B4 (csv + lines) was then fixed the same way** — both file readers moved off `replay_results` to a lazy `produce_async` + `buffer_size` (see B4). |
| B6 | **`spawn_map` historical is lock-step by graph time** (twin of legacy `mapper()`). | 🟢🟡 | Values + tick times match legacy. Two benign artifacts: (a) the sub-graph is expected to emit a result per input instant (a filtering/delaying sub-graph desynchronises the lock-step — legacy's `graph_node` delay case likewise fails); (b) the lock-step reader spends one no-op poll cycle between instants (wingfoil's monotonic-clock re-arm), so bound runs by **duration**, not a raw cycle count. `fluent.rs::spawn_map`, `tests/spawn.rs`. **Cutover ruling 2026-08-03: accepted** (cutover-plan 2.6) — values and tick times match legacy, and legacy's `graph_node` delay case desynchronises likewise. |

## Resolved

- **Channel historical block-collect → incremental read.** The historical channel
  source previously block-collected the entire feed into memory at `start()`
  (documented deviation: unbounded memory + deadlocks a producer that depends on
  the graph's output). Replaced with an incremental, timestamp-gated one-ahead
  read (`interp.rs::pump_historical`, legacy's block-while-behind loop), giving
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
call, and duplicating downstream wiring across two source functions. Legacy
had the same split; wingfoil's superset mandate lets it improve without dropping
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
  a parity gap — legacy never offered a bounded kafka or fluvio replay, so
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

## B'. Where wingfoil is a *superset* — new capability, not a parity gap

Deviations in this direction are additive: wingfoil does something legacy does
not. They are still registered, because "identical behaviour against the same
input" no longer holds and a parity test written against legacy will disagree.

| id | dev | class | notes |
|---|---|:--:|---|
| S1 | **The FIX session validates `MsgSeqNum`, requests resend, and generates Rejects.** Legacy parses tag 34 (`legacy/wingfoil/src/adapters/fix/mod.rs`) and never compares it, so a sequence gap passes through silently — a missed ExecutionReport is undetectable. Legacy also never validates an inbound CheckSum, frames on a `\x0110=` scan rather than BodyLength (mis-framing any length-delimited payload), and never emits a `Reject`. Wingfoil: gap → one `ResendRequest` + `FixSessionStatus::SequenceGap`; low-sequence-without-`PossDup` → `Logout` + terminate, per FIX 4.4; `SequenceReset` honoured in both modes and sequence-exempt; a clean frame that is nonetheless unusable (no `MsgType`, a backwards `NewSeqNo`) → `Reject` with a `SessionRejectReason`. A *garbled* frame — one failing BodyLength or CheckSum — is dropped and resynchronised past, **not** Rejected: FIX 4.4 forbids naming a `RefSeqNum` taken from a frame whose integrity check failed, and answering one would let a corrupt stream burn an outbound sequence number per junk frame. | 🟡 | **The two trees now disagree on a malformed or out-of-sequence feed**, which legacy accepts and wingfoil does not — the whole point, but it means a legacy-oracle parity test on a corrupt fixture will diverge deliberately. There is still **no outbound message store**, so an inbound `ResendRequest` is answered with `SequenceReset`-`GapFill` (conformant for a session with nothing to replay, but your orders are not retransmitted). `adapters/fix.rs`, superset items 4–7. |
| S2 | **An outbound heartbeat timer.** Legacy advertises `HeartBtInt` in its Logon and only ever sends a `Heartbeat` in reply to a `TestRequest`, so session survival depends on the venue probing before it disconnects. Wingfoil honours the interval in both directions and declares an unresponsive counterparty (probe at 1.2×, drop at 2.4×), which `Threaded` then reconnects through. | 🟡 | Strictly more traffic on the wire than legacy — a quiet session now carries heartbeats where legacy's carried nothing. Intended; it is what the advertised interval promises. |
| S3 | **Opt-in sequence-number persistence** (`FixSeqNumStore::File` via `FixOptions`), so a reconnect resumes rather than resetting. | 🟢 | `FixSeqNumStore::Reset` — legacy's behaviour, `ResetSeqNumFlag=Y` every Logon — remains the **default**, so the out-of-the-box conversation with a venue is unchanged. Additive config only. |
| S4 | **`SendingTime` is parsed and repeating groups are addressable.** `FixMessage::sending_time` was hardcoded to `NanoTime::ZERO` with a "future work" comment in both trees; `FixMessage::field` returns only the first match, so a two-sided MarketDataSnapshot's second `MDEntryPx` was unreachable. Adds `groups` / `fields_all` / `group_count` and `FixGroup`. | 🟢 | Purely additive surface. The Python edge gains `sending_time_ns` on the message dict and a `sequence_gap` status. |

## C. Capability gaps — tracked, deferred by design

| id | gap | class | notes |
|---|---|:--:|---|
| C1 | ~~**otlp trace/span export (`OtlpSpans`) not ported.**~~ | ✅ | **Resolved.** `OtlpSpanOps::otlp_spans` is fully ported — it landed with the Phase-5 latency infrastructure (`Traced`/`HasLatency`/`latency_stages!`): one parent span per tick + one child span per stage hop, caller-supplied attributes via `OtlpAttributeBuffer`, silent skip of all-zero/backwards timestamps, same off-thread `consume_async` model as `otlp_push`. Metrics push was already at full parity. `adapters/otlp.rs` deviation #3; port-plan Phase-4 otlp "Trace/span export ✅ ported". |
| C2 | ~~**zmq cross-language interop not ported.**~~ | ✅ | **Resolved** (cutover ruling 2026-08-03, cutover-plan row 2.3). The envelope is no longer wingfoil-local: `WireMessage<T>` is now byte-compatible with legacy's `channel::Message<T>` — same variant order (bincode encodes an enum as an index into it), the same `NanoTime` type on both sides (it lives in `runtime::time` and legacy re-exports it), and `bincode::serialize` on both. Wingfoil's publisher still only emits `Value`/`EndOfStream`; the other three variants exist to *decode* a legacy peer, and `HistoricalValue` fans a same-time burst back out in order rather than collapsing it. Guarded at three tiers: golden bytes (`wire_format_matches_legacy_message`), legacy↔wingfoil over real sockets both directions (`tests/zmq_cross_engine_integration.rs`, retires with the legacy tree), and Rust↔Python (`tests/zmq_cross_lang_integration.rs`, ports legacy's four `cross_lang_tests` cases and survives the cutover). Verified the cross-engine tests fail if the variant order moves. `adapters/zmq.rs`. |
| C3 | **Structural gap — multi-output *islands*.** An `Op`/island produces a single `Out`; a compiled node wanting K outputs needs projection nodes. Interpreted-side multi-output is **not** affected: `Builder::demux` returns `(Vec<Handle<T>>, Handle<T>)` and writes N slots from one cycle. (Closed since first written: runtime graph-mutation surface — `run_dynamic` / `Extension` / `dynamic_group` / `demux` in #519; `StreamStore`, `demux_it` in #529.) | ⚪ | Deferred with the arena, which shares the slot-representation coupling. See `port-plan.md` Phase 1 multi-output note and Phase 4.5 "Arena / SoA value store". **Cutover ruling 2026-08-03: stays post-v1** (cutover-plan 2.8) — legacy has no compiled tier, so this is not a parity gap. |
| C4 | **Compiled-path IO ingestion** — busy-poll (`ALWAYS`) sources + bursts are not expressible in `compiled()`/`nitro!`. | ⚪ | Deliberate exclusion; IO stays at the interpreted boundary + compiled islands. Full design + the gating decision (wake channel vs busy-spin) in `port-plan.md` "Deferred / post-v1 work". (was #502/#503) **Cutover ruling 2026-08-03: stays post-v1** (cutover-plan 2.8) — a deliberate exclusion, and not a parity gap since legacy has no compiled tier. |
| C5 | ~~**augurs ports only `augurs_forecast` + `augurs_outlier`; `augurs_changepoint` / `augurs_seasons` / `augurs_dtw` / `augurs_cluster` are not ported.**~~ | ✅ | **Resolved.** All 6 of legacy's operators are ported: the four remaining ones landed as sliding-window transform ops in the same shape as the first two — `AugursChangepointOps::augurs_changepoint` (BOCPD, window-start index dropped), `AugursSeasonsOps::augurs_seasons` (periodogram, detector built once at wiring), `AugursDtwOps::augurs_dtw` (pairwise DTW distance matrix, Euclidean/Manhattan) and `AugursClusterOps::augurs_cluster` (DBSCAN over those distances). The `augurs` dep gained legacy's `changepoint`/`seasons`/`dtw`/`clustering` sub-features. Legacy's unit tests for all four are ported to `tests/augurs_adapter.rs`, and the example covers them. One new benign deviation, D12. `adapters/augurs.rs`; port-plan Phase-4 augurs. |
| C6 | **`Graph::export` (GML topology dump) not ported.** Legacy writes the wired topology to a GML file (`legacy/wingfoil/src/graph.rs:1207`, test `graph_export_writes_gml_file`); wingfoil's `Builder` has no export at all. | ⚪ | **Deliberately dropped, not deferred-with-intent-to-port.** Port-plan Phase 5 rules it out by name: we want a better introspection/visualization story than a one-off GML dump, designed and scoped separately, rather than a same-shape port of a debug-only helper. Nothing in the engine blocks it — `Builder` holds the full topology plus debug labels — so a future introspection surface can reintroduce it cheaply. **Cutover ruling 2026-08-03: the drop is accepted.** `export` does not come back before the swap; the migration guide names it as the one removed public API, and a designed introspection surface may reintroduce the capability later. `cutover-plan.md` row 2.1; `port-plan.md` Phase 5 "Graph export". |
| C7 | ~~**Latency ops are fluent/interpreted-only** — `stamp` / `stamp_precise` / `latency_report` have no `nitro!` / `compiled()` / `nested()` form.~~ | ✅ | **Resolved for the stamps** (cutover ruling 2026-08-03, cutover-plan row 2.2); `latency_report` reclassified as structural, not a gap. `stamp` / `stamp_precise` now work in all three expansions. The blocker was real but the stated fix — "per-op type-argument syntax in `nitro!`" — was the wrong shape: Rust wants *all* of a function's type arguments or none, and the macro never learns a forwarder's arity (it only ever sees a method-name token; never naming the op type is the design). So the stage crosses as a **value whose type carries it** — `#[op(explicit = S)]` gives each forwarder a leading `PhantomData<S>` and the emission passes `PhantomData::<the_stage>`, so inference resolves it from an argument like any other parameter. The mechanism is general: any op with a phantom type parameter can now reach the compiled tiers. **`latency_report` stays interpreted-only by structure, not by omission**: the sink's entire value is the `Rc<RefCell<LatencyStats>>` it returns, and `compiled()` is outputs-only by design, so the handle cannot escape — a compiled `latency_report` could print at teardown but never be read. Tests: `stamps_reach_the_compiled_tier` and `stamps_reach_a_nested_island` in `tests/latency.rs`. `src/latency.rs`; `port-plan.md` Phase 5 "Latency". |

## D. Cosmetic / API — deliberate-and-benign (low review priority)

| id | dev | class | notes |
|---|---|:--:|---|
| D1 | Sink-as-trait vs legacy's free fn + operator trait (every adapter). | 🟢 | Convention. |
| D2 | Factories return `anyhow::Result` (+ `.context`) vs legacy's typed `io::Error` (e.g. `prometheus::serve`). | 🟢 | Fallible-with-context convention. |
| D3 | postgres/redis sinks take a `consume_async` `buffer_size`. | 🟢 | Added back-pressure knob. |
| D4 | `DemuxMap` uses a `BTreeSet` slot pool (lowest free slot, deterministic) vs legacy's `HashSet`. | 🟢 | An *improvement* — aids backtest determinism (#529). |
| D5 | ~~**otlp pins opentelemetry 0.32; legacy still on 0.28.**~~ | 🟢 | **Resolved — legacy bumped to 0.32; no longer a deviation.** The divergence was taken for security (GHSA-w9wp-h8wv-79jx, #543) and originally filed won't-fix on the grounds that legacy retires at cutover and the churn was not worth it. The tree inversion overturned that: `dependency-review` re-scans every manifest when paths move, so the legacy-side advisory became a merge blocker rather than something that could wait for cutover. The 0.32 API was source-compatible as predicted — the bump was a version change with no code change — and both trees now resolve one `opentelemetry_sdk 0.32.1`. |
| D6 | ~~csv malformed-row error surfaces at replay start vs legacy mid-stream.~~ | ✅ | **Resolved by B4.** While `csv_read` pre-queued the whole file onto `replay_results`, a decode error surfaced up front, at the start of replay. Moving to a lazy `produce_async` producer means each row is deserialized when the graph reaches it, so the error now aborts the run **mid-stream** — legacy's behaviour. Same context string throughout. `adapters/csv.rs`; the same change gave `replay_lines` the same fix. |
| D7 | `FileCache` log messages drop the legacy "KDB " prefix. | 🟢 | The cache isn't kdb-specific in wingfoil. |
| D8 | **`print` emits per tick instead of buffering to teardown.** Legacy `PrintStream` buffers every value in a `Vec` and prints the whole buffer at `Drop`; wingfoil's `Print` op prints each value immediately in `cycle`. | 🟢 | Deliberate — the observable value stream is identical (`print` is a pass-through); only the diagnostic emission differs. Per-tick printing drops the unbounded buffer (legacy grows one entry/tick for the whole run), streams output as the run progresses, and survives a mid-run abort. Shedding the teardown hook also makes `print` a plain single-input `#[op]` (no hand-written `Builder`). `ops.rs::Print`. |
| D9 | **`logged` always wires its node; legacy skips it when the level is disabled.** Legacy `logged` short-circuits at wiring (`log_enabled!` → return the source unchanged) and reads the tick time via `bimap` + `ticked_at_elapsed`; wingfoil always wires the op, leaning on `log!`'s internal enabled-check, and reads the time off `Ctx::time`. | 🟢 | Value stream identical (a pass-through); only the diagnostic differs, and a disabled level still costs only the internal `log!` check. Always-wiring keeps interpreted and compiled identical (a wiring-time skip is inexpressible in `compiled()`). `logged` is fluent-only (its `&str` param vs `String` cfg — see `tests/op_completeness.rs`). `ops.rs::Logged`. |
| D10 | **kdb: credentials never reach an error message** — `KdbConnection::redacted()` returns `host:port` (the password is used only at the `QStream::connect` call site). | 🟢 | Credential-redaction rule (shared with postgres). Legacy put no creds in error context either; wingfoil makes it explicit + test-pinned. |
| D11 | **kdb: `kdb_read` takes legacy's `buffer_size`.** | 🟢 | `kdb_read` is historical-only. The bound is **no longer inert**: after B5 (bounded historical back-pressure + lazy `chunk_stream` slicing) `buffer_size` now gives bounded-memory, pipelined historical replay (`Some(n)` paces slice fetches against the graph drain; `None` = unbounded, legacy's default). `kdb_read_cached` stays unbounded like legacy but streams lazily. The `kdb_write` sink takes a `buffer_size` per D3. |
| D12 | **`augurs_cluster` floors its effective window at the two-sample warm-up.** Legacy's cluster node sizes its buffer for two samples but evicts against the raw `window`, so a `window` of 1 never reaches the warm-up and the node never ticks; wingfoil grows the effective window to the floor (as legacy's own `augurs_dtw` already does). | 🟢 | Only reachable for `window < 2`, where legacy's behaviour is silent-never-tick. Same "grow the window to the model's floor so the node still emits" rule the forecast/seasons ops follow in both trees. `adapters/augurs.rs`; `tests/augurs_adapter.rs::cluster_window_below_floor_still_emits`. |
| D13 | **The Python latency surface returns its stats handle, and the `_if` variants differ from legacy's.** `wingfoil.latency_report(stream, stages)` returns `(sink, LatencyStats)` where legacy `wingfoil` returned only the sink node; `latency_report_if(..., enabled=False)` returns a sink that never ticks (plus an empty stats handle) where legacy returned the *upstream* node. | 🟢 | Both follow **wingfoil's own engine**, not legacy Python: `LatencyReportOps::latency_report` already hands the shared `LatencyStats` back so a caller can read the numbers after the run, and `latency_report_if` already returns a never-ticking sink when disabled. The Python surface being the odd one out was the deviation; this removes it. The disabled variant also keeps the call's return *shape* constant, which legacy's upstream-passthrough did not. Cross-cutting because it is the first binding whose signature is widened by an engine capability legacy never had. `wingfoil-python/src/latency.rs` deviations 2 and 7. |
| D14 | **Three `apply_nodes` lifecycle spans, not legacy's four.** `instrument-apply-nodes` spans `start` / `stop` / `teardown`; legacy also spans `setup`. | 🟢 | Structural, not a dropped phase: wingfoil ops are constructed at wiring time, so there is no `setup` pass over the nodes to span. Same field name (`desc`) and span name as legacy. `interp.rs`; `tests/instrumentation.rs`. |
| D15 | **The compiled / island paths carry no engine instrumentation.** `instrument-*` covers the interpreted engine only. | 🟢 | By design, and no capability is lost against legacy — legacy has no compiled path to be at parity with. A compiled runner exists to be a monomorphized loop with no engine indirection; span guards per cycle/node would defeat exactly what it is for. Capability matrix footnote ¹⁶ in `port-plan.md`. |
| D16 | **`tracing` is an optional dependency in wingfoil; legacy takes it unconditionally.** Legacy's `logged` routes through the `tracing` event macros (its `tracing_log!` dispatcher), so the crate depends on `tracing` whatever the features say; wingfoil's `logged` emits through `log` (D9), leaving `tracing` needed only by the `instrument-*` span sites — so the whole dependency hangs off the `tracing` feature. | 🟢 | The legacy example's `tracing` mode still works unchanged: `tracing_subscriber`'s `init()` installs the `tracing-log` bridge, so `logged`'s records arrive at the subscriber *with the engine's span context attached* (visible in `examples/tracing`'s `instruments` output). A default next build now carries no tracing dependency at all. |

The rows below were folded in by the **2026-08-04 re-sweep** — they are the
adapter-specific items from `aeron`, `fix`, `iceoryx2` and `web`, whose module
blocks had never been reconciled against this register (see the note at the top
of the file). The systemic items in those same blocks were already covered:
their "source takes a `GraphBuilder`/`RunMode` and returns `Result`" bullets are
**B2** + **D2**, their sink-as-trait bullets are **D1**, and iceoryx2's
"ports created at `start()`, wiring is pure" is **A1/A4**.

| id | dev | class | notes |
|---|---|:--:|---|
| D17 | **aeron: `AeronStatusStream` has no wingfoil twin.** Legacy exposed a `MutableNode` the producer drove through `clear()`/`record()` and wired as an active downstream; wingfoil multiplexes status with data over one internal envelope and **splits** it into a `(data, status)` pair with `map_filter` — the same shape the zmq subscriber uses. The status half is an ordinary `Stream<Burst<AeronStatus>>`. | 🟢 | Observable behaviour is identical — transition-only emission, derivation order, and in-band ordering in threaded mode all preserved. This is a **removed legacy public type** (the capability survives, the type does not), so it belongs alongside **C6** in the migration guide's list of surface that does not come across verbatim; unlike C6 nothing is lost, the shape changes. One real behaviour change rides along: *spin* mode now carries status **in-band**, where legacy used a shared `Rc<RefCell<..>>`. `adapters/aeron/mod.rs` deviation 2. |
| D18 | **aeron: the mock backends are public, and `track_status` is a plain flag.** Legacy gated `MockSubscriber` / `MockPublisher` behind `#[cfg(test)]` inside the crate, and the spin node held an `Option<Rc<RefCell<AeronStatusStream>>>` whose `None` case skipped status derivation. | 🟢 | Both follow from wingfoil's test layout: adapter tests live in `tests/` and compile against the public library, so the mocks are public, always-compiled test support (tiny and dependency-free). `track_status: bool` expresses the same choice as the `Option` with no allocation. `adapters/aeron/mod.rs` deviations 4 and 5. |
| D19 | **fix: teardown costs up to one 200 ms read timeout longer.** Legacy's `Threaded` teardown had an `AlwaysSpin` socket-shutdown fast path through an `Arc<Mutex<Option<TcpStream>>>` shutdown handle; wingfoil's background session loop instead checks a stop flag against its 200 ms read timeout (the zmq pattern). | 🟢 | A deliberate trade against the **no-locks-on-the-graph-path** invariant — legacy's fast path needed a mutex reachable from the graph thread. The cost is bounded (one read timeout) and paid only at teardown, never on the run path. `adapters/fix.rs` deviation 3. |
| D20 | **fix + fluvio: pure validations moved from run `start()` to wiring.** Legacy checked FIX's real-time-ness at run `start()` and deferred fluvio's negative-`start_offset` check into the producer future; wingfoil rejects both at wiring. | 🟢 | The inverse direction to A1/A4 — but only for checks that are **pure**, so moving them earlier costs nothing and cannot do I/O. The A1/A4 rule is about *establishing I/O*, which still happens at `start()` for both adapters. Same failure, strictly earlier and with no half-built graph. `adapters/fix.rs` deviation 1; `adapters/fluvio.rs` deviation 5. |
| D21 | **web: `Complete` is emitted from the sink's teardown**, not from the consumer noticing its source ended. | 🟢 | `consume_async` hands back a `flush` teardown (the **B1** mechanism); `web_pub` chains its own `finally` that flushes every queued frame, joins the consumer, and *then* broadcasts `Complete { topic }`. The marker still arrives strictly after the last data frame, on the same broadcast channel, for both a finite `RunFor` and the end of a historical replay — so the observable protocol is unchanged. `adapters/web/mod.rs` deviation 4. |
| D22 | **web: `web_pub_bursts` is added.** Legacy could only publish an atomic same-instant array by mapping `Burst<T>` to `Vec<T>` by hand (`Burst`/`TinyVec` is not `Serialize`, so it cannot be a second impl of the same trait). | 🟢 | Superset, not a deviation from legacy behaviour: the frames are **byte-identical** and the manual `.map(\|b\| b.to_vec()).web_pub(..)` route still works. Recorded because the register is the inventory of *surface* differences in both directions. `adapters/web/mod.rs` deviation 3. |
| D23 | **iceoryx2: the publisher does not reject or no-op under historical replay.** | 🟢 | **Explicit legacy parity, deliberately kept** — and the reason it is worth a row is that it cuts *against* the house pattern: `zmq_pub` errors under historical and the telemetry exporters no-op, so the absence of a guard here reads like an omission unless it is written down. Legacy's iceoryx2 publisher publishes under either run mode, and a backtest piping its output into shared memory is a legitimate use. `adapters/iceoryx2/mod.rs` deviation 3. |
| D24 | **aeron: the `aeron-rs` backend locks on the graph thread, and the subscriber auto-downgrades `Spin`→`Threaded` to avoid it.** The crate returns its `Subscription`/`Publication` as `Arc<Mutex<…>>` shared with its own client-conductor thread, so every `poll()`/`offer()` takes that lock. | 🟡 | **Legacy parity** — legacy carries the identical warning and the identical downgrade, so nothing is introduced here. It earns a row because it is the tree's one sanctioned exception to the **no locks on the graph path** invariant, and an audit grepping for `.lock()` will land on it: the subscriber backend reports `supports_graph_thread_poll() = false` and `aeron_sub_fragment` downgrades the mode (logging a warning), so `Spin` is unreachable for it by construction. The **publisher** has no threaded mode and therefore no downgrade — its `offer()` does lock on the graph thread; the module docs say to avoid it on latency-sensitive paths and prefer the `aeron` (rusteron) backend, whose calls are genuinely lock-free. `adapters/aeron/{transport,aeron_rs_backend}.rs`; module docs "⚠️ `aeron-rs` takes a lock on the graph thread". |
| D25 | **fix: wingfoil validates the session where legacy does not — sequence numbers, CheckSum, BodyLength framing, and Reject.** Legacy parses tag 34 and never compares it (a gap passes through silently), never validates an inbound CheckSum, frames by scanning for `\x0110=` rather than using BodyLength, and never sends a Reject. | 🟡 | **wingfoil is the superset** — new capability, not a parity gap, so nothing regresses. It is flagged 🟡 rather than 🟢 because it is the one place the two trees genuinely *behave differently on the same input*: legacy accepts a malformed or out-of-sequence feed, wingfoil does not (ResendRequest + a `FixSessionStatus::SequenceGap`, or Logout-and-terminate as FIX 4.4 requires). Deliberate — silently passing a sequence gap is undetectable data loss on order flow. Both trees still *drop* a garbled frame rather than answering it. `adapters/fix.rs` deviations 4–5, 7; #704. |
| D26 | **fix: sequence-number persistence is opt-in.** Legacy is in-memory only and always sends `ResetSeqNumFlag=Y`. | 🟢 | That remains wingfoil's **default** (`FixSeqNumStore::Reset`), so the out-of-the-box conversation with a venue is unchanged; `FixSeqNumStore::File` is additive. Same row covers the smaller additions alongside it — `SendingTime` is parsed into `FixMessage::sending_time` rather than left at `NanoTime::ZERO`, and repeating groups are addressable via `FixMessage::groups`. `adapters/fix.rs` deviations 6–7; #704. |
| D27 | **otlp: `otlp_spans` takes its arguments in a different order.** wingfoil is `otlp_spans(span_name, config, attrs)`; legacy was `otlp_spans(config, span_name, attrs)`. | 🟢 | Deliberate — it groups the two `&'static str`-ish leading args before the config. Recorded rather than left implicit because it is the **only** place a ported adapter changes a legacy call's argument *order* rather than its shape, so a porting user meets it as a bare type error with nothing pointing at the cause. Every span capability is otherwise preserved. `adapters/otlp.rs` deviation 3. |
| D28 | **quotation: `func!` does not enforce closedness, and quoted closures reach ops through `Stream::with_src` rather than an `OpFn` config bound.** `docs/wired-graph-codegen-decision.md` §3–§4 specifies both: the `func!` expansion coerces through a fn pointer so a *capturing* closure fails at the call site, and every closure-config op is bound by an `OpFn` trait with a blanket impl for `Fn` plus one for `QuotedFn`, so `map` and friends accept either form through one signature. | 🟢 | **The `OpFn` bound is not implementable, and the fn-pointer coercion is not general.** rustc propagates closure *signature* inference only from `Fn`/`FnMut`/`FnOnce` bounds; behind any other trait a closure literal loses parameter-type inference *and* higher-ranked lifetime inference. Measured on this catalog: ~370 errors across 41 targets, and the residue after reverting the fluent layer to `Fn` bounds is entirely **inside `nitro!` blocks** — `compiled()` emits closure literals into forwarders whose bounds come from the op, so the macro's whole inference-rooting mechanism depends on that bound being an `Fn` bound. The coercion fails separately: `fn(&_) -> _` names an arity, so it only covers `OpFn1`-shaped ops and would leave `join` and `fold` unquotable. **Instead**: ops keep their `Fn` bounds and never see a `QuotedFn`; the quotation is unwrapped at the fluent layer (`map(q.f)`) and the source recorded against the *node* via `Stream::with_src`, read back by `Runner::describe`. One method covers the whole catalog — built-in and user ops alike — instead of a quoted twin per op, and it puts the metadata where a traversal looks. Closedness moves to the generator (#726 step 3), which is where the requirement actually bites: it is the thing about to splice a body into another scope. §3's tier-2 explicit capture lists **are** implemented — `func!([fee] move |p| p - fee)` records each capture's *value* through `EmitLiteral` and emits `{ let fee = 2.5f64; move |p| p - fee }`, which is what makes a per-instrument parameter generatable at all. An *undeclared* capture is not caught at the `func!` call site (the fn-pointer coercion §3 specifies has to name an arity, so it cannot cover `join`/`fold`) — but it no longer has to be, because **`#[wiring]` detects captures automatically** and `func!` is now the escape hatch rather than the normal way to write one. The attribute runs free-variable analysis over each closure body and renders every name it finds through `emit::Probe`, an autoref-specialisation ladder that yields `Some(literal)` when the type implements `EmitLiteral` and `None` when it does not. That softness is the design point: the attribute annotates a *whole wiring function*, most of whose nodes are never emitted, so an `EmitLiteral` bound per detected capture would make `orders.map(move |o| book.lock()..)` over an `Arc<Mutex<_>>` a hard compile error in ordinary wiring. Instead the wiring compiles and runs, and only *generation* refuses, naming the binding. Three residual classes escape detection and still fall through to pass 2, each documented on `free_vars`: a name used only inside a macro invocation, a captured *callable* invoked as `f(x)` (excluded so that calling a free helper function is not mistaken for a capture), and a name both bound and free in one closure. `crates/wingfoil/src/quote.rs`; `crates/wingfoil/src/emit.rs`; `tests/quotation.rs`; `tests/wiring_attribute.rs`; #726. |

---

## Where this stands — nothing open

This section used to list what to do next. Every entry on it has since been
closed, and it is kept as the record of how:

1. **B4** — *was* "decide whether the csv whole-file memory deviation is
   acceptable". Not a ruling in the end: it was **fixed**. `csv_read` (and
   `lines`) moved off `replay_results` to a lazy `produce_async` producer with
   a `buffer_size` bound, so rows deserialize on demand and a malformed row
   aborts mid-stream, closer to legacy than the up-front read ever was.
2. **C6** — `Graph::export` is a *public legacy API* wingfoil chose not to port,
   and the ruling that needed making was made: **accept the drop**
   (2026-08-03, cutover row 2.1). The migration guide names it as the one
   removed public API, and `Builder` still holds the topology and the debug
   labels, so a designed introspection surface can reintroduce the capability
   later.
3. **C7** — the other ⚪ carried into the cutover audit, ruled the other way:
   **close it**. `stamp` / `stamp_precise` reach all three engines; only
   `latency_report` stays interpreted-only, and that is structural (a
   `compiled()` graph is outputs-only, so the stats handle could never escape).
4. **A1–A7 are all closed**: the defer-to-start migration is complete for every
   I/O adapter, A2 turned out to be **parity** rather than a gap (legacy is
   single-run for I/O sources too, verified against its source), and A6 was
   pinned to A3 and fixed with it. **A5a** is the one residual, ruled
   *accept — legacy-parity* (cutover 2.5).

5. **The 2026-08-04 re-sweep found no new ruling to make** — but it did find
   that the standing obligation had lapsed: four adapters (`aeron`, `fix`,
   `iceoryx2`, `web`) landed after the original sweep and were never
   reconciled, and A4 carried a claim about `postgres_read` that the code
   contradicted. Both are now fixed (D17–D23; A4's parenthetical), and every
   new row classifies 🟢.

The standing obligation is the one in "Keeping this current" below: re-run the
greps after each adapter/engine PR. There is no open ruling.

**The failure mode to guard against is a lapsed sweep, not a missed ruling.**
Every open question this register has ever raised got answered; what actually
went wrong between 2026-08-03 and the re-sweep is that newly ported adapters
never got folded in at all — so nothing was *mis*classified, it was
*un*classified, and a file that says "nothing open" reads the same either way.
When adding an adapter, folding its module block in here is part of landing it,
not a later cleanup.

**The second failure mode is a stranded row.** Resolving a B-row often makes a
D-row obsolete, and nothing connects the two: B4 moved `csv_read` to a lazy
producer, which fixed the exact behaviour **D6** described, but D6 sat stale
until the second pass caught it — as did the same claim in two places in
`port-plan.md` and one in `adapters/mod.rs`. So when a row is marked ✅, grep
the *mechanism* it names (here `replay_results`) across `docs/` and the module
docs, not just this file. A resolved deviation usually has prose describing it
somewhere else.

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
a gap — legacy also throws / produces nothing on a second `run()` of an I/O
source, verified against `legacy/wingfoil/src/nodes/async_io.rs` + `channel.rs`; the
register's "legacy re-runs" was incorrect; the deterministic re-run subset was
already at parity via the Phase-1 `reset` hook); **C1** (otlp trace/span export
`OtlpSpanOps::otlp_spans` fully ported, landed with the Phase-5 latency
infrastructure — was a ⚪ capability gap, now at metrics-parity); **C5** (augurs
`changepoint` / `seasons` / `dtw` / `cluster` ported, taking the adapter to all 6
of legacy's operators — was a ⚪ capability gap, now full-parity but for the
benign D12 window floor).

## Keeping this current

Re-run the two greps in **Sources** above after each adapter/engine PR and add
any new `# Deviations from legacy` entry here with a class. Category A is a
manual audit — re-examine it whenever the source/sink lifecycle or the channel
machinery changes. At cutover, every 🔴 and 🟡 needs an explicit ruling — **all of
which have now been given (2026-08-03); see `cutover-plan.md` §2 for the
reasoning behind each.**
