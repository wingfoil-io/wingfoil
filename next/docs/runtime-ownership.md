# Async runtime ownership: caller-passed `&Handle` → Graph-owned with override

Status: **proposal.** Coupled to
[`source-lifecycle-defer-to-start.md`](./source-lifecycle-defer-to-start.md) —
these two should be decided and executed together. Tracks deviation **A5** in
[`deviation-register.md`](./deviation-register.md).

> **Update (spike landed):** the `source_at_start` primitive from the
> lifecycle plan has since landed on `next` (PR #547) and `zmq_sub` now
> establishes its socket + producer thread in `start()`. That directly
> unblocks the runtime work below — the migration can build on the *landed*
> primitive rather than a proposed one. The `produce_async` / `consume_async`
> family (etcd/redis/kafka/postgres/otlp) still spawns at wiring and still
> takes `&Handle`, so the analysis and recommendation are unchanged; only the
> "current state" and "migration sketch" are updated to reflect the spike.

## The question

Legacy wingfoil **owns** a tokio runtime (with an override to inject your own).
wingfoil-next instead makes every async adapter take a caller-supplied
`&tokio::runtime::Handle`. Which is right — and is next's change justified?

## Current state (what next does today)

- `produce_async` / `produce_async_bounded` / `consume_async`
  (`next/crates/wingfoil-next/src/async_source.rs`) take a
  `&tokio::runtime::Handle` (sources also take `RunParams`). The async adapters
  — etcd, redis, kafka, postgres, otlp — all thread that handle through their
  factories, and each still **spawns/connects at wiring time**.
- Each adapter documents this as deviation #1: *"The tokio runtime is the
  caller's … classic hides a global runtime … next takes the `Handle`
  explicitly."* The stated rationale is (a) no hidden global state, and (b)
  **the producer task spawns at wiring time, so the factory needs a handle**.
- **Exception since #547**: `zmq_sub` no longer spawns at wiring. It uses the
  `source_at_start` primitive (`fluent.rs` / `interp.rs`) — the socket connect
  and producer thread now run in `start()`, wiring stays pure. `zmq_sub` is
  channel-fed, not `produce_async`-based, so it never took a `&Handle`; but it
  is the proof that deferring I/O establishment to `start()` works in the
  engine today, which is exactly the mechanism this plan relies on.

## Analysis

**Sharing across adapters.** Both models can share one runtime; they differ in
the *default*. Legacy's owned runtime shares across all adapters with zero
caller effort. next's `&Handle` shares only if the caller threads the *same*
handle to every adapter — and nothing stops a caller from standing up several.
Sharing is a mild point *for* owned-by-default, not a reason to pass handles.

**API simplicity.** `etcd_sub(conn, prefix)` beats
`etcd_sub(&g, &handle, params, conn)`. next pays this verbosity at every async
call site; an owned-with-override runtime keeps the simple default and still
allows the explicit case.

**Is next's change justified?** The steelman is real but narrow:
- *"Libraries shouldn't own runtimes"* is a genuine Rust design principle, and a
  `lazy_static` global runtime is exactly the spooky global state next's engine
  is built to avoid (never dropped cleanly, pollutes test isolation,
  shutdown-ordering hazards). This is the one thing next was **right** to avoid.
- Explicit-in-the-signature: the async dependency is visible in the type.

But the concrete rationale next documents — *the wiring-time spawn needs a
handle* — **dissolves under the defer-to-start change.** If adapters establish
their I/O in `start()` rather than at wiring, the handle is needed at
`start()` — which is the *engine's* moment, not the caller's. So the strongest
reason for `&Handle`-at-wiring goes away exactly when we do the lifecycle work
we already want.

**The one shared constraint.** next's `block_on`-on-the-graph-thread means the
graph thread must be **non-async**. But an owned runtime satisfies this
identically — its worker threads are separate from the graph thread either way.
So this constraint does **not** force caller-passed handles.

## Recommendation

Adopt neither legacy's hidden *global* nor next's *handle-everywhere*, but the
middle that captures the best of both:

> **The `Runner`/`Graph` owns a tokio runtime** — created lazily when the first
> async adapter registers, dropped at teardown — **with an override** to inject
> a caller-supplied `Handle`. Adapters receive the handle from the engine at
> `start()`.

This gives:
- legacy's simple default API (no `&Handle` in the common call),
- automatic sharing across adapters (they all get the graph's one runtime),
- a **clean, non-global lifecycle** (owned by the runner, not a `lazy_static`) —
  the real thing next was right to avoid,
- and the override for embedding in a caller's async app / custom runtime config
  / tests.

## Coupling to defer-to-start (do them together)

Once sources and sinks establish I/O in `start()` (the source-lifecycle plan),
the engine simply hands each adapter its owned runtime's handle at `start()`.
The `&Handle`-at-wiring parameter then **disappears as a side effect** of that
refactor — the two changes are one decision, not two.

The `source_at_start` spike (#547) has already carved out the source half of
this for the channel-fed path: `zmq_sub`'s I/O now runs in `start()`, so the
engine already owns a "establish this adapter's I/O when the run begins" hook.
Extending the same hook to the `produce_async` family is what lets the engine
supply the handle at `start()` instead of the caller supplying it at wiring.

## Migration sketch

1. `Runner` gains an optional runtime: `Option<tokio::runtime::Runtime>` created
   lazily on first async-adapter registration, plus an override slot for a
   caller `Handle`. Drop it at teardown.
2. Expose the handle to adapters at `start()` — via the `start` hook signature
   (or `Ctx`), reusing the **landed** `source_at_start` primitive (#547) that
   already runs adapter setup in `start()`. `consume_async` sinks need the same
   deferral, which is still outstanding.
3. Drop the `&Handle` (and wiring-time `RunParams`) parameters from
   `produce_async` / `consume_async` and the async adapter factories; the async
   closures capture the engine-provided handle at `start()`.
4. Override mechanism: `GraphBuilder::with_runtime(handle)` (or a `RunParams`
   field) — keep passing a `Handle` available as the escape hatch, so nothing
   is *lost* relative to today.
5. Migrate the async adapters (etcd/redis/kafka/postgres/otlp) and their tests
   (which today each construct a runtime and pass its handle).

## Decisions to ratify

- **Runner-owned vs `GraphBuilder`-owned** runtime (lifecycle boundary).
- **Override surface** — a builder method vs a `RunParams` field.
- Whether `&Handle` stays as the documented override (recommended: yes).
- Confirm no interaction with **B1** (etcd_pub's `block_on` on the graph thread):
  the owned runtime's workers are separate from the graph thread, so `block_on`
  from the graph thread is fine — but verify no nested-runtime panic if the
  caller drives the graph from inside their own runtime (that's the existing
  "drive from a non-async thread" rule, unchanged).

## Acceptance criteria

- An async adapter can be wired **without** the caller passing a handle.
- All async adapters in one graph share **one** runtime.
- An override injects a caller-supplied `Handle` (embedding / custom config).
- The runtime is dropped at teardown — no leaked worker threads across runs.
- No nested-runtime panics; the "drive from a non-async thread" rule is the only
  remaining constraint, documented as today.
- All existing async-adapter parity/integration tests pass (rewritten to drop
  the explicit handle where they now rely on the default).
