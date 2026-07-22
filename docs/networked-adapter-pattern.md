# Networked adapter pattern for wingfoil-next — scoping note

Status: **scoping / blocked-on-primitive.** This note is the outcome of the
first attempt to port a *networked* adapter (`redis`) to `wingfoil-next`. Every
adapter ported so far (`cache`, `common`, `csv`, `lines`) is non-networked: it
touches a file or an in-process buffer, never an external service with a
connection to open, authenticate, and tear down. `redis` is the first
request/response- and streaming-shaped adapter, and it is meant to *establish
the connection-lifecycle pattern* the later request/streaming adapters
(`postgres`, `etcd`, `zmq`, `kafka`, `kdb`, `fix`, `web`, …) reuse.

The conclusion: **the `Op` *contract* is ready, but the *constructors* that
would let an adapter use it are not.** `wingfoil-next` has no
established, working way to wire a **source or sink whose `start` connects, whose
fallible `cycle` does the request/reply or drain, and whose `stop`/`teardown`
disconnects.** Expressing `redis` faithfully in that shape requires a new engine
primitive (a lifecycle-capable source/sink registration on `Builder`), not just
an adapter written on existing primitives — so per the porting guardrail this
was scoped rather than forced. What follows is exactly what is missing, how the
existing primitives get close, and the minimal addition that unblocks the port.

## What the classic `redis` adapter needs

Classic `wingfoil::adapters::redis` (`wingfoil/src/adapters/redis/`) exposes four
edges over two transports:

| Function | Shape | Redis commands | Lifecycle it wants |
|---|---|---|---|
| `redis_sub` | streaming **source** | `SUBSCRIBE` then an on-message loop | connect+subscribe once, stream messages, disconnect |
| `redis_pub` | request/reply **sink** | one `PUBLISH` per entry | connect once, publish per burst, disconnect |
| `redis_stream_read` | streaming **source** | `XRANGE` snapshot → `XREAD BLOCK 0` tail | connect once, snapshot, tail-block, disconnect |
| `redis_stream_write` | request/reply **sink** | one `XADD` per record | connect once, append per burst, disconnect |

Classic implements all four on the **async** ergonomics — `produce_async`
(sources) and `consume_async` (sinks) — running the `redis` crate's
`tokio-comp`/`aio` connections on a tokio runtime. The classic feature is
`redis = ["dep:redis", "async"]`, and the dependency is
`redis = { version = "0.27", features = ["tokio-comp", "aio", "streams"] }`
— **async only; no synchronous connection feature is enabled.** Classic went
async precisely because the streaming sources *block* (`on_message().next()`,
`XREAD BLOCK 0`), which does not fit a synchronous single-threaded cycle.

The non-live tests that must port (from `redis/integration_tests.rs`,
`test_connection_refused`) are: construction/wiring without connecting, and
**connect-error propagation against an unreachable endpoint** (ECONNREFUSED),
asserting the error reaches the graph with context. That test is the
correctness gate for the connection lifecycle.

## The engine contract is already ready

`wingfoil-next`'s `Op` trait already has everything the lifecycle needs
(`wingfoil-next/src/op.rs`):

- `cycle(..) -> Result<Tick<Out>>` — fallible; `Err` aborts the run with an
  anyhow chain, `Tick::Quiet` is ordinary control flow.
- `start`, `stop`, `teardown` — all `-> Result<()>`, defaulting to `Ok(())`.
  The doc on `start` literally says *"IO ops open resources here"*; `teardown`
  says *"The place to release resources (close sockets, files)."*

And the interpreted `Runner::run` (`wingfoil-next/src/interp.rs`, ~L1582–1621)
already drives that lifecycle with the classic-parity semantics:

1. `start` every node (a `start` error aborts before any cycle, with
   `node {i} ({label}) start` context);
2. run cycles;
3. `stop` every node, then `teardown` every node — **both run even when a cycle
   aborted**, and the first error wins (a cleanup error only surfaces if nothing
   already failed).

`tests/fallibility.rs` covers abort-with-context, teardown-runs-on-error, and
clean-run teardown. Phase 0.1 of the port plan (`docs/port-plan.md`) records
this lifecycle as **landed**. So a redis source/sink *op* — `start=connect`,
fallible `cycle=publish/read`, `teardown=disconnect` — would execute perfectly
if only it could be **constructed**.

## The gap: no constructor exposes those hooks to a source or sink

An adapter builds nodes through the fluent extension points
(`GraphBuilder::source` / `with_builder`, `Stream::wire`) over `Builder`'s
public/`pub(crate)` registration methods. None of them can wire a source or sink
that connects at `start` and disconnects at `teardown`:

- **`for_each`** (`Stream::for_each`, the sink used by the csv/lines sinks) has
  a **fallible cycle** — good — but **no `start`/`stop`/`teardown`**. The csv and
  lines sinks therefore open their file **eagerly at wiring time** and rely on
  `Drop` to close it. Fine for a file handle; a socket wants *connect at
  `start`* (so merely building a graph does not open a live authenticated
  connection) and an *explicit `teardown` disconnect*, neither of which
  `for_each` offers.
- **`poll`** (`SourceOps::poll`, the lines *tail* source) gives an `ALWAYS`
  busy-poll source — the right activation for a drain-shaped source — but its
  closure is `Fn() -> Option<T>`: **infallible, no way to propagate a connect or
  read error**, and **no lifecycle hooks**. A poll-based redis source could
  neither surface ECONNREFUSED nor connect in `start`.
- **`finally` / `timed` / `print`** are the *only* lifecycle-carrying nodes in
  the whole crate, which proves the engine runs `stop`/`teardown` — but each is a
  bespoke `Builder` method hard-wired to one pure op (a teardown-time callback, a
  perf summary, a debug dump). They attach their hooks by reaching into the
  private `self.nodes.last_mut()` **inside `interp.rs`**; an adapter in
  `adapters/redis.rs` cannot do the same (the node vector, `push_node`,
  `new_slot`, `has_always` are all private to the engine module). And none of
  them is a *source* or a request/reply *sink*, nor generic over connect state.
- **`channel` + a producer thread** (`SourceOps::channel` +
  `ChannelSender::send`/`send_error`) is the closest thing to a streaming source:
  a background thread feeds values in and a `Message::Error` aborts the
  receiver's next (fallible) cycle with context. But the producer is spawned at
  **wiring time** and connects **there**, not in a `start` hook; disconnect is by
  dropping the sender/receiver, not a `teardown`; and there is **no sink
  counterpart** (a channel is ingest-only).
- **`produce_async`** (`wingfoil-next/src/async_source.rs`, `async` feature) is
  *exactly* classic redis's source transport: an async closure connects inside a
  spawned tokio task and `send_error`s a failure into the fallible cycle. It is
  the natural home for `redis_sub` / `redis_stream_read` and would port them
  almost verbatim from classic. But (a) it connects at task spawn (wiring), not
  in `start`; (b) it **bypasses `start`/`stop`/`teardown` entirely**; and (c)
  there is **no `consume_async`** — no async *sink* — so `redis_pub` /
  `redis_stream_write` have no matching primitive at all
  (`grep -rn consume_async wingfoil-next/src` is empty).

So every existing primitive is *ingest-and-eager-open* or *pure-op lifecycle*:
none is a **connection source** (connect-at-start, fallible/streaming cycle,
disconnect-at-teardown) or a **connection sink** (connect-at-start, fallible
request/reply cycle, disconnect-at-teardown), and no adapter can assemble one,
because the hook-attachment machinery lives behind `interp.rs`'s private surface.

## Two obstacles beyond the missing constructor

1. **Streaming sources block in a single-threaded engine.** `redis_sub`'s
   on-message loop and `redis_stream_read`'s `XREAD BLOCK 0` are *blocking*
   reads. The interpreted engine cycles nodes on one thread; a synchronous
   `ALWAYS` cycle that blocks on the socket freezes the whole graph. The faithful
   options are the thread/`channel`/`produce_async` feed (which is why classic is
   async) *or* a non-blocking poll with a short read timeout (which drags in the
   `redis` sync `PubSub`'s self-referential borrow — the `PubSub<'_>` borrows the
   `Connection`, so the op state can't own both). Request/reply **sinks**
   (`PUBLISH`, `XADD`) have no such problem — they only cycle when the upstream
   ticks and each command is a bounded round-trip — so the sink half maps onto a
   synchronous `start=connect` / fallible `cycle` / `teardown=disconnect` op
   cleanly, once a constructor exists.

2. **Classic redis is async-only.** The `redis` crate is pulled in with
   `tokio-comp`/`aio` and no sync-connection feature. A *synchronous*
   lifecycle port (`Connection::get_connection`, blocking commands) needs the
   `redis` crate's default/sync features enabled in `wingfoil-next/Cargo.toml`
   — a deliberate dependency deviation from classic, to decide up front.

## Minimal addition that unblocks the port

No `Op`-trait change and no execution-model change — the hooks and the run loop
already exist. What is missing is **two generic registration primitives on
`Builder`** (mirroring `register_op1`, and attaching hooks exactly the way
`timed`/`finally` already do via `nodes.last_mut()`), plus the thin fluent
wrappers over the existing `with_builder` / `wire` extension points:

```rust
// A connection SINK: one active input, fallible request/reply cycle,
// connect at start, disconnect at teardown. Serves redis_pub / redis_stream_write,
// and postgres / etcd request-reply writes.
pub(crate) fn register_sink_lifecycle<A, S, Cyc, StartF, DownF>(
    &mut self, src: Handle<A>, label: &'static str, state: S,
    cycle: Cyc,     // FnMut(&mut S, &A, &mut Ctx) -> Result<()>
    start: StartF,  // FnMut(&mut S, &mut Ctx) -> Result<()>   // connect
    teardown: DownF // FnMut(&mut S, &mut Ctx) -> Result<()>   // disconnect
) -> Handle<()>;

// A connection SOURCE: no upstreams, fallible drain cycle, connect at start,
// disconnect at teardown. `ALWAYS` for a non-blocking drain; `THREADED` for a
// background connect+read thread feeding a channel (the streaming case).
pub(crate) fn register_source_lifecycle<S, Out, Cyc, StartF, DownF>(
    &mut self, label: &'static str, activation: Activation, state: S,
    cycle: Cyc,     // FnMut(&mut S, &mut Ctx) -> Result<Tick<Out>>
    start: StartF,  // FnMut(&mut S, &mut Ctx) -> Result<()>   // connect
    teardown: DownF // FnMut(&mut S, &mut Ctx) -> Result<()>   // disconnect
) -> Handle<Out>;
```

Both are ~30 lines each, structurally identical to `register_op1` +
`timed`'s hook attachment, and reusable by every later networked adapter — which
is the whole point of doing `redis` first.

### Recommended sequencing

1. **Land `register_sink_lifecycle` first.** It is unambiguous, needs no
   engine-model change, and immediately and faithfully expresses the
   request/reply half of redis (`redis_pub` = `start` connect / `cycle`
   `PUBLISH` each `RedisEntry` in the burst / `teardown` disconnect;
   `redis_stream_write` = the same with `XADD`) *and* the same shape for
   postgres/etcd writes. Its connect-error test (127.0.0.1:1 → ECONNREFUSED in
   `start`, aborting the run with context) is the reference connection-lifecycle
   test.
2. **Decide the streaming-source transport** for `redis_sub` /
   `redis_stream_read`, one of:
   - `register_source_lifecycle` with `Activation::THREADED`: `start` spawns the
     connect+subscribe+read thread and hands its `ChannelSender` to the op state;
     `cycle` drains what arrived (fallible, `Message::Error` → abort); `teardown`
     signals the thread to stop and joins. This keeps connect/disconnect on the
     lifecycle hooks *and* respects the blocking-read reality. **Preferred** — it
     is the pattern the streaming adapters (`zmq`, `kafka`, `kdb`) will all want.
   - Or keep the streaming sources on `produce_async` (verbatim from classic) and
     accept that they connect at task-spawn rather than in `start`, documenting
     the split. Cheaper now, but does not establish the streaming half of the
     lifecycle pattern.
3. **Resolve the `redis` dependency**: enable the sync `redis` features in
   `wingfoil-next` for the synchronous sink cycle, or keep the async connection
   and `block_on` a runtime handle inside the lifecycle hooks. Decide before
   writing the adapter so the feature graph mirrors a deliberate choice.

Once (1) lands, `wingfoil-next/src/adapters/redis.rs`, its `redis` feature in
`Cargo.toml` (mirroring classic's deps + the sync decision), its registration in
`adapters/mod.rs`, and `tests/redis_adapter.rs` (construction/wiring +
connect-refused propagation; live tests gated behind a `redis-integration-test`
marker exactly as classic, never silently skipped, never requiring redis in CI)
follow directly and become the template every later networked adapter copies.
</content>
