# Sources establish their I/O in `start()`, not at wiring

Status: **implemented, for every I/O adapter.** Tracks deviations **A1–A4** in
[`../planning/deviation-register.md`](../planning/deviation-register.md).

## The question

Every background I/O source used to spawn its producer thread/task and connect
its socket at **wiring time** — inside the factory, while the graph was still
being constructed, before `Runner::run`. Should it defer to `start()`?

## The decision

> **Yes. Wiring is pure — it registers intent and validates config. All I/O
> establishment happens at graph `start()`, and is torn down at teardown.**

## Why

- **Testability.** `zmq_sub(&g, …)` used to open a socket and spawn a thread the
  moment it was called, so the wiring logic — address parsing, registry
  resolution, mode rejection, graph shape — could not be unit-tested without
  live I/O. A pure factory can be asserted on directly.
- **A lifecycle you can reason about.** Nothing happens until `run()`. There is
  no "wired but not running" window in which a realtime source is quietly
  accumulating messages nobody will read.
- **Error timing matches legacy.** A connect/subscribe/lease failure surfaces
  during the run, with node context, rather than during graph construction.
- **It removed the `RunParams` validation dance.** Because `produce_async`
  spawned at wiring, callers passed the run's params *at wiring* and the engine
  had to check they matched the actual `run()` later. That entire class of
  "wiring params ≠ run params" mismatch existed only because of the eager spawn;
  deferring means params come straight from the run.

## What shipped

- **`Builder::source_at_start` / `SourceOps::source_at_start`** (`interp.rs`,
  `fluent.rs`) — a `channel`-fed source whose producer is established in
  `start()`. The factory allocates the channel and stores a `setup` closure but
  performs no I/O. On each run `start()` calls `setup(sender)`, which
  connects/spawns the live producer and returns a **`StopHandle`** (a
  generalised, start-scoped `ThreadStopGuard`) dropped at teardown. A `setup`
  error aborts the run at start with node context.
- **Every source migrated** — `zmq_sub` first (#547, the spike), then the
  `produce_async` `_sub` family (etcd/kafka/redis/postgres, via
  `source_at_start_with_params`) and `postgres_read`. `produce_async` no longer
  takes `RunParams`; the `_sub` sources take only a `RunMode`, for the
  wiring-time historical rejection.
- **Every sink migrated** — the streaming sinks connect lazily inside their
  `consume_async` consumer on the first write: `postgres_write` (#577), `redis_pub`
  / `redis_stream_write` (#578), `etcd_pub`. `kafka_pub` was already compliant
  (librdkafka connects lazily on first `send()`).
- **Acceptance tests** (`tests/fluent_primitives.rs`) — assert no `setup` at
  wiring or `build()`, exactly one at start, the `StopHandle` dropped at
  teardown, and a `setup` error aborting the run with node context.

What deliberately stays at wiring is **pure validation**: address parsing, the
historical-mode rejection, `postgres_read`'s window check and slice computation.
Nothing there touches the network. `prometheus::serve` keeps a synchronous
pre-run bind, which is legacy-parity.

## Re-run was dropped — the premise was wrong

This plan originally led with a third motivation: that deferring to `start()`
would make I/O sources **re-runnable**, closing a parity gap with legacy. That
was **incorrect, and the work was dropped rather than deferred.**

Legacy is *also* single-run for I/O sources, verified against its source:
`AsyncProducerStream::setup` (`legacy/wingfoil/src/nodes/async_io.rs:214`) takes
its `FnOnce` and sender with `.take().ok_or_else(…)`, so a second `run()` errors;
`ChannelReceiverStream::setup` (`nodes/channel.rs:257`) `.take()`s its notifier
and drains its receiver on the first run, so a channel/external source produces
nothing on a second. Wingfoil's explicit single-run error is therefore parity —
and clearer than legacy's silent-nothing on the channel path.

So the reset/channel-recreation interlock this document once called "the hard
part" is **not needed**, and `port-plan.md` §0.4's single-run ruling **stands
unchanged**. The deterministic subset (tickers/constants/combinators/feedback)
re-runs in both engines via the Phase-1 `reset` hook; that parity is real and
already delivered.

The caller-owned `external` and plain `channel` feeders are the one remaining
wiring-time establishment, and they are the same single-run question rather than
a connect-at-wiring deviation.
