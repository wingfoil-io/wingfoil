# Zero-alloc I/O — closing the pool's residuals, and pushing loans out to the adapters

**Status: proposed, not built.** Nothing here is on `main`. The
[`pool`](../../../crates/wingfoil/src/pool.rs) module ships today with two
*deliberate* per-message residuals (its own "Residual allocations" section
names them); this document is the plan to remove them and then make the loan
protocol available to the I/O adapters, which cannot reach it at all right now.

> **Why `planning/proposals/` and not `decisions/`.** It carries a sequencing
> section and open engineering work. The reasoning below dies when the work
> lands, which is the test in [`docs/README.md`](../../README.md#ruling-or-record).

---

## 0. Where we are, measured

`crates/wingfoil/tests/steady_state_allocs.rs` (the pooled order-book pipeline,
1000 messages, 64-buffer pool) on `main` today:

```
steady state: 1121 allocations for 1000 messages (1.12/message), 0 of payload scale (≥ 16000B)
```

Bucketing every allocation by size over the same run attributes it:

| Size class | Count | What it is |
|---|---|---|
| 32–63 B | ~1000 | the `Rc<PoolLoan<T>>` control block — **one per message** |
| everything else | ~130 total | queue-block growth (unbounded `mpsc` transport, the pool's return queue, the crossbeam waker channel) + occasional `Burst` heap spill |
| ≥ 16 KB | **0** | payloads — the pool already does its main job |

So the pool's headline claim holds (no payload ever touches the allocator), and
the remainder is **1.00/message of `Rc` box plus ~0.13/message of amortized
queue and burst growth**. The target of this plan is a strict
`assert_eq!(0, allocs)` over a measured steady-state window, and a loan-based
ingress path the adapters can actually use.

---

## Part A — make the pool itself zero-alloc

### A1. Kill the per-message `Rc` (−1.00/msg)

`Pooled<T>` is `Option<Rc<PoolLoan<T>>>`, and `Pooled::adopt` mints a fresh
`Rc` per arrival. `pool.rs` already names the end state: *"a slab handle —
`{slot, generation}` indices into a preallocated side table"*. The loan budget
makes it easy, because live handles are capped at `capacity` by construction:

```rust
struct Slot<T> { buf: UnsafeCell<Option<Box<T>>>, rc: Cell<u32> }

pub struct PoolTable<T> {
    slots: Box<[Slot<T>]>,      // exactly `capacity`, built once at wiring
    free:  RefCell<Vec<u32>>,   // ditto — never grows
    ret:   SyncSender<Box<T>>,
}

pub struct Pooled<T> { table: Option<Rc<PoolTable<T>>>, idx: u32 }
```

- `adopt` pops a free index, parks the loan's `Box<T>` in the slot, sets
  `rc = 1`. No allocation: the table, the slot array and the free list are all
  preallocated.
- `Clone` is `Rc::clone` (a non-atomic counter bump, no allocation) plus
  `rc += 1`. The last drop takes the box back out, returns it down `ret`, and
  pushes the index back on the free list — the same prompt return the `Rc` drop
  gives today.
- `Deref` needs one `unsafe` block. **SAFETY**: the payload is immutable while
  `rc > 0`; a slot is not recycled until `rc` reaches 0; every handle is
  `!Send` and lives on the graph thread, so no `&mut` can coexist.
- `PartialEq` stays pointer identity, expressed as `(Rc::ptr_eq(table), idx)` —
  a slot is only reused after its last handle is gone, so two live handles
  sharing an index *are* the same loan. Empty handle: `table: None`, which
  keeps `Default` / `get()` / the deref panic exactly as they are.

**Rejected alternative:** keep `Rc` and recycle the control blocks by parking
them in the receiver and reclaiming those whose `strong_count` has fallen back
to 1. It needs an `O(capacity)` scan per message and defers buffer return to
the next scan — strictly worse than the intrusive count, and no less `unsafe`
in spirit.

**API break.** `Pooled::adopt(loan)` is public ("so custom adapters can feed
pooled payloads through their own ingress") and cannot mint a handle without a
table. Replacement: `PoolTable::adopt(&Rc<Self>, PoolLoan<T>) -> Pooled<T>`,
with the table handed out by `pooled_channel`. Recommendation: keep
`Pooled::adopt` as a **deprecated shim** that builds a private one-slot table —
it stays correct, costs one allocation per call, and nothing silently changes
behaviour for a user who has written a custom ingress. That keeps this a minor
release rather than a major one.

### A2. Preallocate both transports (−~0.06/msg)

Both queues in the pooled path are `std::sync::mpsc::channel()` — the unbounded
form, which is block-allocating (one block per ~32 messages).

- **Producer → graph transport**: `sync_channel(capacity + SLACK)`. The bounded
  form allocates its slot array once at construction and never again, and it
  cannot block here because the pool already caps values in flight at
  `capacity`. `SLACK` covers the non-value envelopes (the exhausted-pool
  checkpoint nudge, EOS, error). The nudge switches to `try_send` and is
  dropped when full — it is idempotent, and a full queue means the graph has
  work pending anyway.
- **Return queue**: `sync_channel(capacity)`. At most `capacity` buffers can be
  outstanding, so it can never fill.

`ret_tx.clone()` per loan is a refcount bump, not an allocation; it can stay.

### A3. Recycle the receiver's burst (−~0.06/msg)

`Burst<T>` is `TinyVec<[T; 1]>` — inline at one element, heap above it. The
realtime arm of `channel_inner_mapped` builds a fresh `Burst::new()` each cycle
and assigns it into the slot, so every multi-value burst allocates and the old
block is freed.

Fix: keep a per-node **scratch burst** in the node's state and `mem::swap` it
with the slot each cycle, clearing before the drain. Both `TinyVec`s then keep
their heap capacity for the life of the run, and — because it is a swap, not a
clear-in-place of the slot — the quiet-wake semantics of *both* receivers are
untouched: plain channels keep the last burst readable, the pooled receiver
still releases it under `release_quiet`.

### A4. The waker channel (−~0.03/msg, engine-wide)

`waker_channel()` is a crossbeam `unbounded()` carrying node indices, so every
cross-thread wake costs an amortized block allocation. A bounded channel is
*not* a safe swap (dropping a wake for node X because the queue is full of
wakes for other nodes loses an edge). The correct shape is a preallocated
ready-bitset — `Box<[AtomicBool]>` indexed by node, plus one condvar/semaphore
for the "something became ready" edge — which deduplicates repeated wakes for
free and cannot allocate by construction.

This is engine-wide and breaking (`pub type ReadyReceiver =
crossbeam::channel::Receiver<usize>` is public API in `runtime/kernel.rs`), so
it is sequenced last and belongs in its own issue. Everything above is
pool-local.

### A5. Turn the gate strict

`steady_state_allocs.rs` currently pins a ceiling of 8 allocs/message. Split
the run into a **warm-up window** (first N messages — first burst spill, first
touch of each table slot, thread spawn) and a **measured window** asserting
`assert_eq!(0, allocs)`. Keep the large-allocation assertion as it is; it is
the one that survives regardless. Add a second gate over a pooled *adapter*
source once Part B lands.

---

## Part B — the adapters

Today the loan protocol is reachable only by hand-written producers via
`SourceOps::pooled_channel`. **No adapter in the tree can use it**, because
every threaded adapter source is built on the deferred-connection primitive
`SourceOps::source_at_start`, which hands `setup` a `ChannelSender<T>`.

### B1. The missing primitive

```rust
fn pooled_source_at_start<T, Setup>(
    &self,
    capacity: usize,
    init: impl Fn() -> T,
    setup: Setup,
) -> Stream<Burst<Pooled<T>>>
where
    T: Send + 'static,
    Setup: FnMut(PooledSender<T>) -> Result<StopHandle> + 'static;
```

Wired from `Builder::pooled_channel` plus the existing deferred-setup machinery
(`source_at_start_with_params`), so it inherits historical-mode rejection,
`StopHandle` teardown and error propagation unchanged. An adapter switching
over is then a ~10-line diff. **Land this before any adapter conversion** — it
is the piece that makes the pool an engine capability rather than a
demo.

### B2. Which adapters, in what order

Criterion for being on this list: a **realtime** source whose payload carries
interior heap and is constructed per message.

1. **iceoryx2 subscriber** — `iceoryx2/read.rs` does `sample.to_vec()` per
   sample. The adapter is zero-copy right up to the graph boundary and then
   allocates; a loaned `Vec<u8>` (`clear()` + `extend_from_slice`) closes the
   last copy. Highest payoff, smallest diff — do this one first.
2. **aeron threaded subscriber** — fragment bytes into a loaned buffer. (The
   `Spin` mode already runs the parser on the graph thread with no channel at
   all; that path is *already* the zero-alloc one and should be documented as
   such rather than changed.)
3. **zmq subscriber** — two allocations per message today: `recv_bytes()` into
   a fresh `Vec`, then `bincode::deserialize` into a fresh `T`. Both are
   removable: `recv_into` a loaned byte buffer, and `deserialize_in_place` /
   a `clear()`-and-refill decode into a loaned `T`.
4. **ws** — `WsMessage::{Text(String), Binary(Vec<u8>)}` per frame. Worth doing
   for throughput, but note the trading roadmap's §2 position: WebSocket venues
   sit behind milliseconds of path jitter, so this is not a latency argument.
   After 1–3.
5. **lines tail / csv replay** — a `String` per line. Easy, but low-rate and
   mostly historical; do them only if they earn a worked example.
6. **fix** — `FixMessage` carries `Vec<(u32, String)>`. The biggest single win
   on the trading path, and the only one that is not merely a transport change:
   it needs a decoder that refills a recycled message in place (field vec and
   field strings both `clear()`ed and rewritten). Separate issue.

### B3. Constraints to fold into `/new-adapter`

These are the rules a converting adapter must not discover the hard way, and
they belong in the adapter skill in the same PR as B1:

- **`PooledSender<T>` is not `Clone`** — single producer, unsynchronised free
  list. An adapter with more than one producer task (kafka partitions,
  multi-connection ws, the fix reader + timer threads) needs either one pool
  per producer or a genuine multi-producer pool. Decide per adapter; do not
  reach for an `Arc<Mutex<_>>` hack.
- **`Pooled<T>` is `!Send`** — it cannot cross `spawn` / `spawn_map`, cannot be
  a `#[pyop]` payload, and cannot feed a sink that hands values to another
  thread. So pooled variants are **additive** (`*_sub_pooled`), never a
  replacement for the owned-payload API.
- **Async adapters must use `try_loan`** — a blocking `loan()` on a tokio
  worker parks a runtime thread. Each adapter documents its policy when the
  pool is exhausted (drop, or propagate backpressure to the socket).
- **Pooled adapter sources are realtime-only.** The pre-run backlog trap
  already documented on `pooled_channel` (a historical producer that queues its
  whole feed blocks once `capacity` values are outstanding) applies in full;
  keep replay paths on the owned API.
- **The loan budget is an adapter option** — surfaced in the options struct,
  defaulted, and documented as the backpressure knob it is.

---

## Sequencing

| | Work | Effect |
|---|---|---|
| 1 | A1 + A5 | −1.00 alloc/msg; the gate becomes strict |
| 2 | A2 + A3 | pooled path reaches 0 alloc/msg at steady state |
| 3 | B1 (+ example, + skill update) | adapters can reach the pool |
| 4 | B2.1 iceoryx2 → B2.2 aeron → B2.3 zmq | one PR each, each with its own alloc gate |
| 5 | A4 waker bitset | engine-wide, breaking, own issue |
| 6 | B2.6 fix decoder recycling | own issue |

Docs that move with the work: the "Residual allocations" section of `pool.rs`
(it is the thing being deleted), `benches/README.md`, the ingress ladder in
`planning/trading-roadmap.md`, and `.claude/commands/new-adapter.md` (B3).

## Open decisions

1. **`Pooled::adopt` break** — deprecated one-slot shim (recommended, keeps
   this a minor release) vs. a clean break at the next major.
2. **`unsafe` in `pool.rs`** — one documented block for the slab deref
   (recommended). The safe alternatives are all worse; `RefCell` cannot give
   `Deref` a `&T`, and the strong-count recycler is rejected above.
3. **Feature gating** — do the `*_pooled` adapter variants ship under each
   adapter's existing feature, or behind an additional `pool` feature?
