# Wingfoil Codebase Improvement Plan

*Produced from a full-codebase review (core engine, node operators, all 18 adapters,
language bindings, CI/docs/testing) on 2026-07-08, at version 7.0.0
(commit `6be91d4`). File/line references are to that commit.*

## Executive summary

The codebase is in good shape overall: the scheduler core is clean and
allocation-free in steady state, the `#[node]` macro keeps operators small and
declarative, `produce_async`/`consume_async` gives adapters a genuinely shared
backbone, unit-test density is strong (554 tests across `wingfoil/src`), and the
CI workflows are unusually well commented. The error-handling policy in
CLAUDE.md is followed almost everywhere.

The review nevertheless found a small number of **high-severity correctness
bugs** (silent data loss in iterator sources, a broken time-epoch contract, a
release pipeline that publishes "prod" to Test PyPI), a **security-sensitive
cluster** (q-code injection in `kdb_write`, credentials in Postgres error
messages), and a large amount of **consolidation opportunity** (duplicated
operator pairs, per-adapter reinvention of demux/retry/realtime-guard patterns,
copy-pasted CI workflows).

The plan is organized into five phases, ordered by risk-adjusted value. Each
item cites the evidence so it can be turned into an issue directly.

---

## Phase 0 — Release-blocking and data-loss bugs (do first)

These are silent-failure bugs that affect correctness of shipped artifacts or
user data. All are small, contained fixes.

### 0.1 PyPI "prod" releases go to Test PyPI
`.github/workflows/pypi-publish.yml:84` reads
`${{ github.event.inputs.pypi-target }}`, which is **empty when invoked via
`workflow_call`** — and `release.yml:104` invokes it exactly that way with
`pypi-target: prod`. The `else` branch wins, so production releases upload to
Test PyPI with the test token. Fix: use `${{ inputs.pypi-target }}` (populated
for both `workflow_call` and `workflow_dispatch`).

### 0.2 Iterator sources drop their final burst
`IteratorStream::cycle` / `TryIteratorStream::cycle`
(`wingfoil/src/nodes/iterator_stream.rs:30-47,105-124`) consume items at the
current engine time into `self.value`, then return `add_callback(...)` — which
is `Ok(false)` when the iterator is exhausted. The node therefore reports "did
not tick" on its last cycle and downstream never sees the final burst. The unit
tests work around this with sentinel rows (`iterator_stream.rs:217-219`).
Since CSV and other adapters build on these streams, the last row of any file
is at risk. Fix: decouple "ticked" from "has more items" — return
`Ok(!self.value.is_empty())` and schedule the next callback separately. Remove
the test sentinels so the tests pin the fix.

### 0.3 `NanoTime::now()` violates its epoch contract
`NanoTime` is documented as "nanoseconds since the unix epoch"
(`wingfoil/src/time.rs:17`), but `now()` (`time.rs:45-47`) reads
`quanta::Clock::now()` — a **monotonic** clock (nanoseconds since boot). In
`RunMode::RealTime`, engine time is therefore boot-relative:
`to_kdb_timestamp()`, `From<NanoTime> for NaiveDateTime`, and persisted
realtime timestamps are wrong as absolute times, and cross-host latency
stamping (`latency.rs`) is meaningless. The latency test at
`latency.rs:823-826` asserts only `> 1s`, masking this. Fix: snap
`SystemTime::now()` against `Clock::now()` once at startup and apply the
offset in `now()` (keeps quanta's cheap reads, restores the epoch anchor).
Also upgrade `quanta` 0.9.3 → 0.12.x (calibration fixes). Add a test asserting
`now()` is within some tolerance of `SystemTime::now()`.

### 0.4 kdb write path: q-code injection and doc mismatch
`format_kdb_column_q` (`wingfoil/src/adapters/kdb/write.rs:199-213`) splices
symbol values into q source as `` enlist`{sym} `` with no escaping — a symbol
containing a space, backtick, or `;` breaks the query or executes arbitrary q.
Floats render via `{}` so `NaN`/`inf` produce invalid q literals
(`write.rs:219-221`). The module doc (`write.rs:37`) claims a K-object
functional form is used — make the implementation match the doc: build
`(insert; `t; data)` as K objects instead of formatting q text. Also fix the
ragged-row panic path at `write.rs:136-155` (return an error for
non-compound/ragged rows instead of `unwrap_or_default` + index panic).

### 0.5 Postgres error context leaks credentials
All three entry points embed the full libpq conn string — including
`password=...` — in `anyhow` context: `postgres/read.rs:126`,
`postgres/write.rs:69`, `postgres/sub.rs:107`. These reach logs and graph-abort
errors. Fix: add a `redacted()` display to the connection config and use it in
all error context (see 3.4 for the shared helper).

### 0.6 Async channel path panics instead of erroring
`AsyncChannelSender::send_message` and `into_async`
(`wingfoil/src/channel/kanal_chan.rs:240,273-283`) call `.unwrap()` on send /
notify / take — a receiver dropped during shutdown (a normal teardown race)
panics the sender task instead of surfacing `SendNodeError::ChannelClosed` like
the sync path (`kanal_chan.rs:194-208`). Direct policy violation; propagate
errors like the sync path does. Similarly, `ChannelReceiver::teardown`
(`kanal_chan.rs:47-55`) sleep-polls 100×10ms then `panic!`s — return an error
via its `anyhow::Result` instead.

---

## Phase 1 — High-severity correctness in the engine and operators

### 1.1 Filter semantics diverge between Stream and Node
`FilterStream` declares `active = [source, condition]`
(`wingfoil/src/nodes/filter.rs:17`) while `FilterNode` declares
`passive = [condition]` (`nodes/node_flow.rs:122`). With an independently
ticking condition stream, `stream.filter(cond)` re-emits the *stale* source
value every time the condition ticks true; `node.filter(cond)` does not.
Decide the semantics (passive condition is the safer default), align both, and
document the behavior on `StreamOperators::filter`.

### 1.2 `TimeQueue` silently deduplicates events
`TimeQueue` is a `PriorityQueue<ValueAt<T>, _>`
(`wingfoil/src/queue/time_queue.rs:13-17`), which dedupes identical
`(value, time)` pairs — two cloned `FeedbackSink`s sending equal values in the
same cycle lose all but one event (`nodes/feedback.rs:55-63`); same for
`CallBackStream::push`. For an event-processing library this is silent data
loss. Replace with a `BinaryHeap` keyed by `(time, seq)` (or `VecDeque` where
ordering is externally guaranteed). This also removes the `T: Hash + Eq`
bounds that currently make `delay`/`delay_with_reset`/`feedback` unusable on
`f64` streams (`nodes/mod.rs:410,434,445`).

### 1.3 Window/flush edge cases
`WindowStream` (`wingfoil/src/nodes/window.rs:36-40`): a value pushed in the
same cycle as a boundary flush is dropped on the last cycle (the `!flushed`
guard skips the residual buffer), and windows only flush on the *next upstream
tick* — a quiet upstream withholds a complete window indefinitely. Fix the
last-cycle guard; either schedule a boundary callback or document tick-driven
flushing honestly.

### 1.4 `distinct` swallows an initial default-equal value
`DistinctStream` starts `value` at `T::default()`
(`nodes/distinct.rs:11-28`), so a first upstream value of `0`/`""`/`false`
never ticks. Use `Option<T>` like `DifferenceStream` already does
(`difference.rs:14-17`).

### 1.5 Graph lifecycle and wiring robustness
- `Graph::run()` skips `stop`/`teardown` when `run_nodes` errors
  (`graph.rs:594-602`) — sockets/threads/files leak on any node failure.
  Capture the error, still run stop+teardown, attach secondary errors as
  context.
- `initialise_node` recurses (`graph.rs:658-684`): deep chains overflow the
  stack at wiring time and cyclic wiring hangs instead of erroring. Convert to
  an explicit stack with an in-progress set (as was already done for
  `fix_layers`, `graph.rs:976-1003`), and report cycles diagnosably.
- `panic!("ready_callbacks are not supported in historical mode")`
  (`graph.rs:710-712`) is a reachable user wiring error — `anyhow::bail!`
  instead.
- `always_callbacks` (`graph.rs:270-275,696-724`): no dedup (a node calling
  `always_callback()` from `cycle()` grows the vec unboundedly), and in
  historical mode a purely-always graph freezes time so `RunFor::Duration`
  never terminates. Dedup per node; advance or reject the historical+always
  combination.
- `impl MutableNode for RefCell<NODE>` forwards everything except `setup`/
  `teardown` (`types.rs:233-249`) — double-wrapped nodes silently skip
  lifecycle hooks. Add the two forwards.
- `HashByRef` hashes the fat pointer but `eq` compares data pointers
  (`queue/hash_by_ref.rs:20-35`) — vtable-pointer divergence across codegen
  units can register a node twice. Hash `Rc::as_ptr as *const ()`; this also
  removes refcount churn from `node_to_index` lookups on the hot path.

### 1.6 Consumer-side async errors are invisible
`MessageStream::to_stream` turns `Message::Error` into `log::error!` + break
(`nodes/async_io.rs:421-429`), so the consumer sees clean end-of-stream and
the run *succeeds*; `AsyncConsumerNode` then reports the misleading "consumer
task exited early without error" (`async_io.rs:103`). Propagate the error to
the graph like the producer path already does (tested by
`produce_async_mid_stream_error`).

### 1.7 dynamic_group (beta) hygiene
Inserting an existing key orphans the old node — it stays wired as an active
upstream forever and becomes unreachable for `remove`
(`nodes/dynamic_group.rs:111-114`); `on_remove` fires for keys never in the
group (`dynamic_group.rs:169-175`). Fix both and add unit tests — this module
currently has **zero** unit tests despite doing graph surgery. Related: removed
nodes stay in `scheduled_callbacks`/`always_callbacks` and are re-marked dirty
forever (`graph.rs:316-320,779`).

### 1.8 FIX adapter integrity
- `FixSenderNode` comments promise blocking back-pressure but `start()` sets
  the socket non-blocking (`adapters/fix/mod.rs:1436-1461`); a full send
  buffer can abort the graph after a **partial frame** was written — protocol
  corruption on the wire. Keep the socket blocking or buffer-and-resume.
- Session-layer errors are swallowed (`fix/mod.rs:1002,1019` return a generic
  `Disconnected`) — log or emit the real error in `FixSessionStatus`.
- Document the codec's integrity limits on `FixMessage` (no checksum/
  BodyLength verification, seq-num parse failures default to 0, SOH inside
  `RawData` desyncs the parser — notable given the Binance RawData logon
  support) (`fix/mod.rs:392-429`).

---

## Phase 2 — CI/release pipeline hardening

### 2.1 Close the CI blind spots
- **Workflow-only PRs run zero CI**: both triggers in `rust-test.yml` carry
  `paths-ignore: [".github/workflows/**"]` — edits to CI itself are never
  validated. Drop the ignore from `pull_request` (keep it on `push` if
  desired).
- **`wingfoil-js` tests never run**: `vitest` and a 287-line test suite exist
  but `web-integration.yml` only lints/builds and `npm-publish.yml` publishes
  untested. Add `pnpm test` to both.
- **Wasm tests don't exist despite a comment claiming they do**
  (`wingfoil-wasm/src/lib.rs:214-216`); the `wasm-bindgen-test` dev-dep is
  already declared — add the tests or fix the comment.

### 2.2 Make the release pipeline retry-safe
`release.yml` pushes the `v$VERSION` tag **before** the publish jobs; any
publish failure strands a tag with no artifacts and the preflight then blocks
re-runs. Tag last (or make publishes idempotent: `--skip-existing` on PyPI,
tolerate already-published from cargo). Remove the duplicate ZMQ integration
job (already covered via `all-tests.yml` → `adapter-integration.yml`).

### 2.3 Modernize Python packaging
`pypi-publish.yml`: 21-job matrix building per-interpreter wheels directly on
`ubuntu-latest`/`macos-latest`, meaning no manylinux containment, **no x86_64
macOS wheels, no Linux aarch64, and no sdist ever uploaded** (the step named
"Build wheel and sdist" passes no `--sdist`, upload copies only `*.whl`).
Switch to `PyO3/maturin-action` with manylinux containers, adopt
`abi3-py39` (drops the matrix to ~3 jobs; Python 3.8 is EOL), add an sdist
leg, and move to PyPI trusted publishing (OIDC) for consistency with the npm
workflow. Fix the license metadata conflict: `wingfoil-python/pyproject.toml:10`
says MIT while the crate and `LICENSE.txt` say Apache-2.0.

### 2.4 Deduplicate the 28 workflows
- Composite action for the Python-bindings setup block (copy-pasted in ~8
  adapter workflows) and for "start container + wait for port" (6 copies) —
  or use GHA `services:` with health checks, which
  `kafka-python-integration.yml` already does idiomatically.
- Fold the structurally identical per-adapter workflows (otlp, iceoryx2,
  augurs, etcd/redis/postgres Rust halves) into `adapter-integration.yml`'s
  matrix — 13 workflow files can become ~5, and per-adapter cache keys will
  behave better than today's shared `shared-key: integration` written by ~10
  jobs with different feature sets.
- Standardize on `dtolnay/rust-toolchain` + `checkout@v4` (5 workflows still
  use the archived `actions-rs/toolchain@v1`).
- Add `concurrency:` groups with `cancel-in-progress` on PR-facing workflows
  (currently none — rapid pushes stack redundant coverage runs).
- Single clippy invocation feeding both the gate and the SARIF report in
  `rust-test.yml` (clippy currently runs three times plus one redundant
  build).
- Constrain `bulk-rebase.yml` — it currently force-pushes **every** branch
  except main, including other contributors' in-flight branches; add a branch
  pattern or explicit branch-list input.
- Move the live-LMAX FIX leg out of path-triggered CI into a scheduled/
  dispatch-only job (external-service outages currently fail CI;
  LMAX allows one session per credential).

### 2.5 Reproducibility and dependency health
- **Triage the open Dependabot alerts** — GitHub reports 26 vulnerabilities on
  the default branch (1 critical, 6 high, 17 moderate, 2 low):
  https://github.com/wingfoil-io/wingfoil/security/dependabot. Review the
  critical/high ones first and decide upgrade vs. dismiss-with-reason for
  each.
- Commit `wingfoil-wasm/Cargo.lock` (the root `Cargo.toml:9-13` comment claims
  it exists; npm releases currently resolve deps fresh at publish time).
- Address the `cargo report future-incompatibilities` warnings
  (`quick-xml 0.22`, `sqlx-core 0.5.13` via transitive pins) before a future
  Rust release breaks the build.
- Benchmarks never run in CI — add a scheduled/dispatch bench workflow with
  baseline tracking, and stamp `benches/README.md` numbers with commit/date
  and reproduction commands (the "blazingly fast" README claim currently
  rests on unversioned static numbers).

---

## Phase 3 — Consolidation and deduplication

The single biggest maintainability lever. Each item removes a class of
future divergence bugs (one has already happened — see 1.1).

### 3.1 Collapse the `try_*` operator twins
`map`/`try_map`, `bimap`/`try_bimap`, `trimap`/`try_trimap`,
`ConsumerNode`/`TryConsumerNode` differ only by `Result` + `?`. Implement the
infallible one as a wrapper (`map(f)` → `try_map(|x| Ok(f(x)))`) or generate
both via `macro_rules!` — collapses ~6 near-identical files. Likewise extract
the identical active/passive `Dep` partition block repeated in four files
(`bimap.rs:18-31` et al.), or teach `#[node]` about `Dep<T>` fields (a known
gap noted in CLAUDE.md).

### 3.2 Unify Stream/Node flow-operator pairs
`ThrottleStream/ThrottleNode`, `DelayStream/DelayNode`, `LimitStream/
LimitNode`, `FilterStream/FilterNode` duplicate cycle logic between
`nodes/*.rs` and `node_flow.rs` and have already diverged once (1.1). A single
generic parameterized on an optional output keeps semantics locked together.
Also: `DelayWithResetStream` embeds `DelayStream` byte-for-byte
(`delay_with_reset.rs:47-73` = `delay.rs:29-56`) — compose instead; and
`FeedbackStream::cycle`/`CallBackStream::cycle` share the same
drain-TimeQueue loop — one helper is also the single place to fix 1.2.

### 3.3 Shared adapter infrastructure (`adapters/common.rs`)
1. **Bounded `channel_pair` with blocking send** — every `produce_async`
   source is currently unbounded (`async_io.rs:46-54` acknowledges it); this
   is the memory-safety story for all async adapters and unifies
   `AsyncConsumerNode`/`FixSenderNode`.
2. **`require_realtime(state, "adapter")` guard** — historical-mode policy is
   currently inconsistent: kdb/postgres/fix/zmq/aeron bail, kafka/redis/fluvio
   silently stamp wall-clock times into a "historical" run. Pick bail as the
   policy and apply uniformly.
3. **Generic `Data/Status` demux helper** — the same enum+split pattern is
   hand-rolled three times (fix `split_events`, zmq `ZmqEvent`, aeron
   `AeronItem`).
4. **Credential-redacting connection display** (fixes 0.5, future-proofs
   kdb/redis).
5. **Idle-backoff spinner** for poll threads (two copies in aeron, flat
   sleeps in iceoryx2/fix).
6. **Shared rustls config builders** pinning the `ring` provider (duplicated
   between fix and web-tls, with the same explanatory comment).
7. Standardize error context on `anyhow::Context` — kafka/redis/etcd/fluvio
   currently flatten chains via `anyhow!("... {e}")`, destroying
   `root_cause()`/downcast.
8. Adopt the `WebServer` thread-lifecycle pattern (shutdown channel + join)
   in the Prometheus exporter, which currently detaches a thread forever
   (`prometheus/exporter.rs:52`).

### 3.4 Smaller cleanups
- `csv_write` panics at wiring time on file-open failure (`csv/write.rs:61-64`)
  while `csv_read` returns `Result` — open in `start()` and propagate.
- iceoryx2 receive errors are treated as "no data" (`iceoryx2/read.rs:151,168`)
  — a persistent port error becomes a silent busy loop; surface it.
- `mark_dirty` implemented twice byte-for-byte (`graph.rs:419-426,686-693`);
  dead/commented code in `graph.rs:21,381`, `channel/mod.rs:6-8`;
  production `assert!`s in `buffer.rs:20`, `demux.rs:165-346` →
  `debug_assert!` or delete.
- `otlp_push` records `0.0` for unparseable values (`otlp/push.rs:81-84`) —
  skip the sample instead; `FileCache::get` rewrites the whole file to bump
  mtime (`cache/file_cache.rs:54`) — use a metadata touch.

---

## Phase 4 — Python/JS bindings quality

### 4.1 Safety
- Replace the fat-pointer `transmute` of `*const dyn Node` to `(usize, usize)`
  (`wingfoil-python/src/lib.rs:64-77,166-184`, `py_stream.rs:125-140`) — fat
  pointer layout is not guaranteed, so this is formally UB. A small
  `AssertSend<T>` wrapper (with a safety comment that `py.detach` runs on the
  calling thread) achieves the same with no layout assumption.
- `env_logger::Builder::init()` in module init (`lib.rs:195-196`) panics if a
  logger is already registered (e.g. another Rust extension in-process) —
  use `try_init()`.

### 4.2 API correctness and idiom
- `stream.not()` and `stream.finally()` are Python keywords — users must write
  `getattr(stream, 'not')()` (the project's own tests do). Rename to
  `not_`/`finally_` per PEP 8. Also `not` currently calls `__neg__`
  (arithmetic), so `not(True) == -1` (truthy) — fix to logical negation.
- `py_web.rs:112` silently publishes `null` on conversion failure (discarding
  the carefully written TypeError) — use `try_map`. Similarly `kafka_pub` and
  `csv_write` silently drop non-dict values (`py_kafka.rs:85-96`,
  `py_csv.rs:108-139`) — log or raise.
- Remove the leftover debug `print` in
  `wingfoil-python/python/wingfoil/stream.py:16`.
- Guard `etcd_sub` with `hasattr` like the other optional adapters
  (`python/wingfoil/__init__.py:25`) so non-default builds import cleanly.

### 4.3 API parity (highest-leverage gaps for backtesting users)
Expose `fold`, `merge`, `throttle`, `window`, and — most importantly — an
`from_iterable` source (core has `iterator_stream`; Python users currently
fake it with `CustomStream`). Extend `CustomStream` with passive upstreams and
lifecycle hooks; accept `PyNode` upstreams (currently panics,
`proxy_stream.rs:44-53`).

### 4.4 JS client
Catch `boot()` rejection in `WingfoilClient` (`wingfoil-js/src/index.ts:78`)
and emit an error `ConnectionState` instead of an unhandled rejection; add
mock-WebSocket tests for the client's subscribe/reconnect bookkeeping (only
`LatencyTracker` is tested today).

---

## Phase 5 — Documentation, API polish, longer-term architecture

### 5.1 Documentation debt
- **CLAUDE.md drift**: omits 4 of 6 workspace members (`wingfoil-derive`,
  `wingfoil-wire-types`, `wingfoil-js`, `wingfoil-wasm`), lists examples that
  don't exist (`rfq`, `circuit_breaker`), says "37 nodes" (there are 44
  files). Refresh it; add the release process and integration-test flags.
- Enable `missing_docs = "warn"` for the `wingfoil` crate and fill gaps
  (`reduce`, `sum`, `consume_async`, `finally` are undocumented; `Element`
  and `ValueAt` are `#[doc(hidden)]` but load-bearing public API).
- Document the single-threaded contract (`Graph` and nodes are `!Send`; cross
  threads only via `channel_pair`) on `Graph`/`Node` rustdoc — users currently
  hit an inscrutable compiler error instead.
- Document lossy semantics honestly where they're intentional: `collapse`
  keeps only the last burst element, `merge` drops losing streams' values on
  same-cycle ties (and fix the misnamed `merge_last_ticked_value_wins` test).
- `CONTRIBUTING.md`: add the local integration-test story (Docker), fix the
  Discord invite mismatch with README, close stale "looking for contributors"
  items (Kafka/SQL adapters now exist). Add a short `docs/RELEASING.md`
  (bump → release → trusted-publisher prerequisites).

### 5.2 API polish
- Missing combinators users will expect: `skip`/`skip_while`/`take_while`,
  `debounce` (trailing-edge; `throttle` is leading-edge only), `pairwise`,
  `start_with`, `try_filter_map`, min/max.
- `latency_report_if(false)` returns the upstream itself instead of the
  documented never-ticking node (`latency.rs:603-641`) — materially different
  graph shape; return `NeverNode` as documented.
- A `RunMode`/`RunFor` builder (`graph.historical().for_cycles(10).run()`)
  to cut the boilerplate every test and example repeats.
- `etcd_pub`'s positional `Option`/bool args → options struct, matching every
  other adapter.

### 5.3 Longer-term engine work
- **Opaque `NodeId`**: keying `ticked`/`add_upstream`/`remove_node` on
  `Rc<dyn Node>` + `HashByRef` forces refcount traffic and pointer-identity
  subtleties onto the hot path; a stable index handed out at wiring time makes
  `ticked(id)` a plain Vec index and deletes the `HashByRef` hazard class.
- **Property tests for the scheduler**: random DAGs + tick schedules asserting
  "a node never cycles before its active upstreams" and "no tick is lost" —
  do this *before* any TimeQueue/scheduling optimization.
- **Realtime-path test coverage**: `wait_ready_callback`,
  `process_ready_callbacks`, sender-drop wakeup, and the recv-timeout race
  branch have no direct tests; nearly all graph tests run historical.
- Split `dynamic-graph-beta` machinery out of the 2160-line `graph.rs` into
  `graph/dynamic.rs`.
- Hot-path nits when profiling justifies: `GraphState::ticked` by `&Rc`,
  scheduler `TimeQueue` → `BinaryHeap<Reverse<(NanoTime, usize)>>`, unify the
  two divergent `RunFor` bound predicates (`graph.rs:79-86` vs `546-547`).

---

## Suggested sequencing

| Order | Scope | Size |
|-------|-------|------|
| 1 | Phase 0 (six fixes) | ~1 week, independent small PRs |
| 2 | Phase 1.1–1.4 (operator correctness) + tests | ~1 week |
| 3 | Phase 2.1–2.2 (CI blind spots, release safety) | days |
| 4 | Phase 1.5–1.8 (engine lifecycle, FIX) | 1–2 weeks |
| 5 | Phase 3 (consolidation) — start with 3.3.1 bounded channels and 3.3.2 realtime guard | 2–3 weeks, incremental |
| 6 | Phases 2.3–2.5, 4, 5 | ongoing/opportunistic |

Test-coverage gaps to close alongside their fixes: `dynamic_group` (zero
tests), `sample` (test has no assertions), `window` boundary cases, `feedback`
duplicate-sink sends, consumer-side async errors, `merge` tie behavior.
