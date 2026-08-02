# Async runtime ownership: the graph owns the tokio runtime (with override)

Status: **implemented** (decision record). Tracks deviation **A5** in
[`deviation-register.md`](./deviation-register.md).

## The question

Legacy wingfoil **owns** a tokio runtime (with an override to inject your own).
Early wingfoil-next instead made every async adapter take a caller-supplied
`&tokio::runtime::Handle`. Which is right?

## The decision

Neither legacy's hidden *global* nor next's *handle-everywhere*, but the middle:

> **The `GraphBuilder` owns one tokio runtime** — created lazily when the first
> async adapter asks for a handle, shared by every async adapter in the graph,
> and dropped at teardown — **with an override** to inject a caller-supplied
> `Handle`.

This gives legacy's simple default API (no `&Handle` in the common call),
automatic sharing across adapters, a clean **non-global** lifecycle (owned by
the graph, not a `lazy_static`), and the override for embedding in a caller's
async app or custom runtime.

## Why (the rationale the code encodes)

- **Sharing.** Both models *can* share one runtime; they differ in the default.
  An owned runtime shares across all adapters with zero caller effort; `&Handle`
  shares only if the caller is disciplined enough to thread the *same* handle
  everywhere. A mild point *for* owned-by-default.
- **API simplicity.** `etcd_sub(&g, conn, prefix)` beats
  `etcd_sub(&g, &handle, params, conn, prefix)` at every async call site; the
  override keeps the explicit case available.
- **What next was right to avoid** was a `lazy_static` **global** runtime —
  spooky global state (never dropped cleanly, pollutes test isolation,
  shutdown-ordering hazards). A graph-owned runtime keeps that win: it is owned,
  scoped, and dropped deterministically, without leaking a global.
- **The one real constraint** — next's `block_on` on the graph thread requires
  the graph thread to be **non-async** — is satisfied identically by an owned
  runtime (its workers are separate threads either way), so it never forced
  caller-passed handles.

## What shipped

- `GraphBuilder`/`Runner` carry an executor-free `AsyncRuntimeSlot`
  (`interp.rs`) — an opaque field until the `async` feature names the tokio
  types. On the first `async_runtime_handle()` call it lazily creates one
  `tokio::runtime::Runtime` (`GraphRuntime`, `async_source.rs`) and caches the
  handle every async adapter then shares.
- The runtime is moved into the `Runner` at `build()` and is the `Runner`'s
  **last field**, so it is dropped only *after* the nodes — an offloaded sink's
  teardown `block_on` still sees a live runtime.
- Override: `GraphBuilder::with_async_runtime(handle)` installs a caller runtime
  (the graph then owns nothing). Sink traits reach the runtime through
  `Stream::graph()`.
- `produce_async` / `produce_async_bounded` / `consume_async` and every async
  adapter factory dropped their `&Handle`; the three primitives return `Result`
  (the lazy `Runtime::new()` is the only fallible part).

## Decoupled from defer-to-start

The original plan framed this as one decision with
[defer-to-start](./source-lifecycle-defer-to-start.md). In practice they
separated cleanly: `produce_async` / `consume_async` still spawn/connect at
**wiring**, but now on the *graph's* runtime rather than a caller handle — so
the ergonomic win landed without waiting for the lifecycle change. `zmq_sub`
already defers via `source_at_start` (#547). When the `produce_async` family
follows, it will pull its handle from the engine at `start()` rather than the
builder at wiring, and the wiring-time `RunParams` will fall away — a *further*
refinement on top of this, not a prerequisite for it.
