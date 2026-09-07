# Zero-alloc I/O — recycling the control block, and pushing loans out to the adapters

**Status: proposed, not built.** Nothing here is on `main`. The
[`pool`](../../../crates/wingfoil/src/pool.rs) module ships today with two
*deliberate* per-message residuals (its own "Residual allocations" section
names them); this document plans their removal, and — the larger half — plans
the loan protocol's escape from `pooled_channel` into the I/O adapters, which
cannot reach it at all today.

Companion to [`kernel-bypass-io.md`](kernel-bypass-io.md) (Project Bypass,
[#957](https://github.com/wingfoil-io/wingfoil/issues/957)), whose §2 audit
lists this pool as load-bearing and whose §5 is a list of things the pool would
have to grow. §5 below is the interlock between the two.

> **Why `planning/proposals/` and not `decisions/`.** It carries a sequencing
> section and open engineering work, and it moves as the measurements move —
> it already has, once. That is the test in
> [`docs/README.md`](../../README.md#ruling-or-record).

---

## 1. Where we are, measured

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

The pool's headline claim holds: no payload ever touches the allocator. What
remains is **1.00/message of `Rc` box plus ~0.13/message of amortized queue and
burst growth**.

## 2. What this is worth — stated honestly, before the design

This section exists because the obvious reading of §1 ("1.12 → 0") oversells
the throughput and undersells where the real win is.

**Part A (the pool's own residuals) is worth about 2–3%, and possibly less
than the bench harness can see.** The `Rc` box is minted by `adopt` on the
graph thread and freed by the last handle drop on the *same* thread: same size
class, tcache hit both ways. It is the allocator's best case, ~15–25 ns for the
pair, against a measured 0.87 µs/message. Design C (§3) saves an indirection on
top of that, worth single-digit nanoseconds. On a shared runner this is
plausibly below Criterion's noise floor — **the exact-count gate is the only
reliable way to observe it**, which is why §3.5 turns that gate strict.

Part A's justification is therefore not throughput. It is three other things,
and they are enough:

1. **A property that can be stated.** "Zero allocations on the steady-state
   path", gated in CI, is a claim a latency evaluator can check. "1.12 per
   message" is a number that drifts.
2. **The tail, not the mean.** tcache refill and arena trim are not on the
   mean; they are on p99.9, which is the number this engine's users care about.
3. **It unblocks the bypass seam** (§5) — the real forcing function.

**Part B (the adapters) is worth roughly an order of magnitude more, and it is
where the measurable performance is.** `iceoryx2`'s spin path does
`sample.to_vec()` per sample — a payload-sized allocation *plus* a memcpy, on
the graph thread. That is the cost class the pool has already been measured
against, in `benches/README.md`:

| Strategy | Per message | Throughput |
|---|---|---|
| owned `Book` | 2.57 µs | 390 Kelem/s |
| `Arc<Book>` | 3.14 µs | 319 Kelem/s |
| `pooled_channel` | **0.87 µs** | **1.15 Melem/s** |

**One caveat that must travel with that 3×**: it is measured on a 2×128-level
book, ~4 KB of payload. For a 64-byte ITCH message the copy is nearly free and
the win collapses toward the allocation cost alone. **The adapter win scales
with payload size** — large for 1500-byte frames, modest for small messages.
Size the expectation per adapter, and measure before claiming.

**So the first piece of work in §6 is a measurement, not a refactor**: a fourth
row on the `pooled_channel` bench for the adapter path. It is an afternoon, and
it is what decides whether the rest is worth its two weeks.

---

## Part A — make the pool itself zero-alloc

### 3.1 Recycle the control block by putting it *in* the buffer (−1.00/msg)

`Pooled<T>` is `Option<Rc<PoolLoan<T>>>` and `Pooled::adopt` mints a fresh `Rc`
per arrival. `pool.rs` currently names the end state as *"a slab handle —
`{slot, generation}` indices into a preallocated side table"*. **That is the
wrong shape, and this document previously repeated it.** A side table adds an
indirection to every read and a second refcount to every clone. The right shape
is intrusive: make the pooled buffer carry its own refcount, so the allocation
we already own *is* the control block.

```rust
struct Slot<T> {
    rc:    Cell<u32>,                 // graph-thread only; non-atomic by design
    ret:   Sender<Box<Slot<T>>>,      // where the last drop sends it home
    value: T,
}

pub struct PoolLoan<T>(Box<Slot<T>>);            // unique owner, `Send`, crosses threads once
pub struct Pooled<T>(Option<NonNull<Slot<T>>>);  // graph handle, `!Send`, refcounted
```

Allocated once per slot at pool construction, recycled for the life of the run.
Against the side-table design:

- **Deref is one hop** (handle → value), versus two today (handle → `Rc` box →
  `Box<T>`) and three under a side table (handle → table → slot → box). The
  side table would have made the read path worse while fixing the allocation.
- **One refcount bump per clone, not two.** A `{Rc<PoolTable>, idx}` handle has
  to bump the table's count alongside the slot's — doubling the cost of the
  operation routing ops perform most.
- **No index or free-list bookkeeping.** `free: Vec<Box<T>>` becomes
  `Vec<Box<Slot<T>>>`; the rest of `PooledSender` is untouched.
- **The unique-loan guarantee stays plain Rust ownership.** `Box<Slot<T>>`
  moves across the thread boundary; no `unsafe Send` argument is needed, where
  a graph-side table owning the payloads would have required one.

The `unsafe` reduces to a `NonNull` deref with one invariant: **while
`rc > 0` the slot is alive and its `value` immutable**, and every handle is
`!Send` and lives on the graph thread, so no `&mut` can coexist. `Clone` bumps,
`Drop` decrements and — at zero — moves the `Box<Slot<T>>` back down `ret` and
runs no destructor on `value` (the buffer is recycled, not dropped; contents
after recycling are undefined, exactly as `loan()` already documents).

The graph-side contract is unchanged in every observable way: `!Send`, deref to
`T`, identity `PartialEq`, the empty `Default` handle, refcount-returns-on-last-
drop. That matters beyond ergonomics — Project Bypass §5.1 depends on it
explicitly (§5).

**Rejected alternatives**, recorded so they are not re-proposed:

- *Keep the `Rc`, hold the allocation alive with a `Weak`.* A dead `Rc` cannot
  be re-initialised — there is no API for it, and `upgrade()` fails once the
  strong count reaches zero.
- *Generational indices into a slab.* Buys nothing here: a slot is never reused
  while a handle to it lives, so there is no ABA to defend against.
- *Cycle/epoch reclamation instead of refcounting.* Breaks outright — `delay`,
  `feedback` and value slots park payloads across cycles by design.
- *An `Rc` recycler that reclaims boxes whose `strong_count` fell back to 1.*
  `O(capacity)` scan per message and deferred returns; strictly worse.

**API break.** `Pooled::adopt(loan)` is public ("so custom adapters can feed
pooled payloads through their own ingress") and its signature survives Design C
unchanged — the loan already carries everything the handle needs. **This is a
second advantage over the side table**, which would have required a table
argument and a deprecation shim. Design C keeps A1 a non-breaking change.

### 3.2 Preallocate both transports (−~0.06/msg)

Both queues in the pooled path are `std::sync::mpsc::channel()` — the unbounded
form, which allocates a block per ~32 messages.

- **Producer → graph transport**: `sync_channel(capacity + SLACK)`. The bounded
  form allocates its slot array once and never again, and cannot block here
  because the pool already caps values in flight at `capacity`. `SLACK` covers
  the non-value envelopes (the exhausted-pool nudge, EOS, error). The nudge
  becomes `try_send` and is dropped when full — it is idempotent, and a full
  queue means the graph has work pending regardless.
- **Return queue**: `sync_channel(capacity)`. At most `capacity` buffers can be
  outstanding, so it can never fill.

This PR owes a written argument for why `try_send` on the nudge cannot
reintroduce the wedge the nudge exists to break.

### 3.3 Recycle the receiver's burst (−~0.06/msg)

`Burst<T>` is `TinyVec<[T; 1]>` — inline at one element, heap above. The
realtime arm of `channel_inner_mapped` builds a fresh `Burst::new()` per cycle
and assigns it into the slot, so every multi-value burst allocates and frees.

Keep a per-node **scratch burst** in the node's state and `mem::swap` it with
the slot each cycle, clearing before the drain. Both `TinyVec`s then keep their
heap capacity for the life of the run, and because it is a swap rather than a
clear-in-place of the slot, the quiet-wake semantics of *both* receivers are
untouched: plain channels keep the last burst readable, the pooled receiver
still releases it under `release_quiet`.

### 3.4 The waker channel (−~0.03/msg, engine-wide)

`waker_channel()` is a crossbeam `unbounded()` of node indices, so every
cross-thread wake costs an amortized block allocation. A bounded channel is
*not* a safe swap (dropping a wake for node X because the queue is full of
wakes for other nodes loses an edge). The correct shape is a preallocated
ready-bitset — `Box<[AtomicBool]>` indexed by node plus one condvar for the
"something became ready" edge — which deduplicates repeated wakes for free and
cannot allocate by construction.

Engine-wide and breaking (`pub type ReadyReceiver =
crossbeam::channel::Receiver<usize>` is public API in `runtime/kernel.rs`), so
it is sequenced last, batched with the other `runtime/` break (§7). Note it is
**irrelevant to bypass ingress**: a spin source never touches the waker.

### 3.5 Turn the gate strict

`steady_state_allocs.rs` pins a ceiling of 8 allocs/message. Split the run into
a **warm-up window** (first N messages — first burst spill, thread spawn) and a
**measured window** asserting `assert_eq!(0, allocs)`. Keep the large-allocation
assertion unchanged; it is the one that survives regardless. Given §2, this
gate — not the bench — is the instrument that proves Part A worked.

**A1 introduces this tree's first `unsafe` on a hot path, and CI runs no Miri
today** (checked: nothing in `.github/workflows/`). Either this PR carries a
Miri job for `pool.rs`, or review carries the argument instead. Prefer the job.

---

## Part B — the adapters, where the performance is

Today the loan protocol is reachable only by hand-written producers via
`SourceOps::pooled_channel`. **No adapter in the tree can use it.**

### 4.1 Two primitives, not one

The threaded adapters and the spin adapters need different things, and an
earlier draft of this document had only the first:

```rust
// (a) threaded adapters — iceoryx2 Threaded/Signaled, zmq, ws, kafka
fn pooled_source_at_start<T, Setup>(&self, capacity: usize, init: impl Fn() -> T, setup: Setup)
    -> Stream<Burst<Pooled<T>>>
where T: Send + 'static, Setup: FnMut(PooledSender<T>) -> Result<StopHandle> + 'static;

// (b) spin adapters — bypass RX, iceoryx2 Spin, aeron Spin
fn pool<T>(&self, capacity: usize, init: impl Fn() -> T) -> LocalPool<T>;
```

(a) is `Builder::pooled_channel` behind the existing deferred-setup machinery
(`source_at_start_with_params`), inheriting historical-mode rejection,
`StopHandle` teardown and error propagation unchanged.

(b) is the one that was missing. **A spin source has no channel and no thread
crossing**: it drains a ring on the graph thread under `Activation::ALWAYS`. It
needs to loan and adopt on that one thread, with no return queue at all — the
same `Slot<T>` from §3.1 minus the `Sender`, with the free list held directly
by the node. Today `Pooled<T>` can only be minted by a cross-thread channel,
which is precisely backwards for this path.

### 4.2 Correction: the spin adapters are *not* already zero-alloc

An earlier draft claimed the spin paths were already allocation-free because
they run the parser on the graph thread. They are not.
`adapters/iceoryx2/read.rs:377-390` (`receive_into`) does `sample.to_vec()` per
sample, in spin mode, with no channel involved. `aeron` `Spin` and `fix`
`AlwaysSpin` share the shape. Primitive (b) is what fixes them.

### 4.3 Conversion order

Criterion: a **realtime** source whose payload carries interior heap and is
constructed per message. Payload size decides the size of the win (§2).

1. **iceoryx2 — spin *and* threaded.** The largest per-message payload in the
   tree that is currently copied out, the adapter is otherwise zero-copy right
   up to the graph boundary, and it exercises *both* new primitives in one PR.
   First, and it is the measurement that justifies the rest.
2. **aeron threaded** — fragment bytes into a loaned buffer.
3. **zmq** — two allocations per message today: `recv_bytes()` into a fresh
   `Vec`, then `bincode::deserialize` into a fresh `T`. Both removable via
   `recv_into` a loaned buffer and a `clear()`-and-refill decode.
4. **ws** — `WsMessage::{Text(String), Binary(Vec<u8>)}` per frame. Throughput,
   not latency: the roadmap's §2 position is that WebSocket venues sit behind
   milliseconds of path jitter.
5. **lines tail / csv replay** — low-rate and mostly historical; only if they
   earn a worked example.
6. **fix** — `FixMessage` carries `Vec<(u32, String)>`. The biggest win on the
   trading path and the only one that is not a transport change: it needs a
   decoder that refills a recycled message in place. Separate issue.

### 4.4 Constraints to fold into `/new-adapter`

- **`PooledSender<T>` is not `Clone`** — single producer, unsynchronised free
  list. An adapter with several producer tasks (kafka partitions,
  multi-connection ws, the fix reader + timer threads) needs one pool per
  producer or a genuine multi-producer pool. Decide per adapter; no
  `Arc<Mutex<_>>` hack.
- **`Pooled<T>` is `!Send`** — it cannot cross `spawn`/`spawn_map`, cannot be a
  `#[pyop]` payload, cannot feed a cross-thread sink. A **compile error, not a
  runtime surprise**, and the reason pooled variants are additive
  (`*_sub_pooled`) rather than replacements.
- **Async adapters must use `try_loan`** — a blocking `loan()` on a tokio
  worker parks a runtime thread. Each adapter documents its exhaustion policy.
- **Pooled adapter sources are realtime-only.** The pre-run backlog trap
  documented on `pooled_channel` applies in full; replay stays on the owned API.
- **The loan budget is an adapter option** — in the options struct, defaulted,
  documented as the backpressure knob it is.

---

## 5. Interlock with Project Bypass

[`kernel-bypass-io.md`](kernel-bypass-io.md) §2 lists `pooled_channel` /
`Pooled<T>` as already-there and load-bearing; §5 lists three things "true
zero-copy" would need. The interaction is close enough to be worth pinning:

1. **§5.1 requires that `Pooled`'s graph-side contract not change** — "stays
   `Rc`-based, `!Send`, refcount-returns-on-last-drop". Design C preserves all
   of it except the literal `Rc`, which becomes an intrusive count. One word in
   that document's §2 and §5.1 wants correcting when this lands.
2. **§5.1's foreign-memory loan must be designed for now, not later.** A DMA
   loan owns a descriptor index and returns it to a ring, not a `Box` down an
   mpsc. Write the return path behind a small private trait in A1 so that case
   is a second impl. Hardcoding the `Sender` is a rewrite of the type we just
   rewrote. **Cheap now, expensive later.**
3. **§5.2's non-blocking exhaustion is half-done**: `try_loan()` exists; the
   missing half is the *count* that `RxStats` surfaces. A `Cell<u64>` on
   `PooledSender` plus an accessor is a handful of lines and belongs in A1, not
   in bypass P4.
4. **§5.3's holding-time assertion becomes cheap.** With an intrusive header,
   per-slot in-flight age is a field; with a bare `Rc` it is essentially not
   implementable.
5. **§11 q1 ("`Frame<'_>` vs a pooled frame at the seam") gets easier.** Once
   primitive (b) exists, "`poll_rx` fills a `Pooled<FrameBuf>` directly" is a
   cheap prototype rather than a fork in the road, and it no longer forecloses
   §5 — handle and buffer ownership are already separated.

**Ordering between the two projects.** Bypass P0 is a measurement with zero
code that may end that project (§9 there); it should be running regardless and
costs this project nothing. Bypass P2 (`mold_itch`) is the long pole and does
not touch the pool. Everything in this document therefore fits *inside* Bypass's
own critical path: **A1 lands well before P1 needs the seam frozen.** What must
not happen is P1 freezing its seam against today's pool, since §5.1 then
describes work that is a rewrite rather than an extension.

---

## 6. Sequencing

Revised from the first draft, which front-loaded the ~2% and back-loaded the
3× (§2).

| | Work | Why here |
|---|---|---|
| 0 | **Measure**: a fourth row on the `pooled_channel` bench for the adapter path | An afternoon; decides whether the rest is worth two weeks |
| 1 | **B1 primitives (a) + (b)**, with example and `/new-adapter` update | Both are needed by step 2, and step 2 is the win |
| 2 | **iceoryx2, spin + threaded**, with its own allocation gate | The largest measurable win; validates both primitives against a real adapter before anything freezes |
| 3 | **A1 (Design C)** + the strict gate + Miri job | Lands before bypass P1; non-breaking, so it does not gate step 2 |
| 4 | A2 + A3 | Reaches 0 allocs/message |
| 5 | aeron, then zmq | One PR each |
| 6 | A4 waker bitset, batched with core pinning ([#392](https://github.com/wingfoil-io/wingfoil/issues/392)) | Both `runtime/`, both breaking, and #392 is a bypass P3 prerequisite |
| 7 | fix decoder recycling | Own issue; a decoder rewrite, not a transport change |

**B2 does not depend on A1** — pooled adapter variants work on today's pool, at
1.12 allocs/message, and still remove the payload allocation and copy. That is
what licenses this order.

Docs that move with the work: the "Residual allocations" section of `pool.rs`
(it is the thing being deleted), `benches/README.md`, the ingress ladder in
`../trading-roadmap.md`, `.claude/commands/new-adapter.md` (§4.4), and the
corrections to `kernel-bypass-io.md` §2/§5.1 (§5 above).

## 7. Versioning

`SourceOps` is public and **not sealed** (`fluent.rs:355`), so adding a required
method is technically breaking for any third-party implementor even though the
trait exists to be used rather than implemented. Put both new primitives on a
new `PooledSourceOps` trait — purely additive — rather than growing `SourceOps`.
The same caveat applies to each adapter's extension trait.

| Work | Bump |
|---|---|
| Steps 0–5 (B1, B2, A1 Design C, A2, A3, A5) | **minor** — 9.1.0. Design C keeps `Pooled::adopt` intact, and the adapter variants are additive |
| Step 6 (A4 waker + #392) | **major** — 10.0.0. `waker_channel()`, `ReadyReceiver`, `Kernel::with_ready` are public |

No dependency-manifest movement is expected (`sync_channel` and the intrusive
header are std), so the dependency-policy rules in `CLAUDE.md` do not fire. The
exception is A4 dropping `crossbeam-channel` from the waker path, which lands
inside the major regardless.

## 8. Effort

Sized in the repo's label vocabulary (`small` <1d, `medium` 1–3d, `large` >3d),
and calibrated against the last merged op PR (`4930a34`, the debounce
combinator: **+313 / −38 across 16 files** for one op — in this tree, logic is a
minority of every diff).

| Step | Size | Insertions | Where the lines go |
|---|---|---|---|
| 0 · bench row | small | ~80 | |
| 1 · both primitives | medium (2–3d) | ~640 | only ~200 is logic; the example is ~240 (the existing `pooled_channel` example is main 199 + README 61) |
| 2 · iceoryx2 | medium (1d) | ~350 | `read.rs` is 719 today; ~120 change, ~140 tests |
| 3 · A1 + gate + Miri | medium (2d) | ~400 / −120 | ~200 of `pool.rs`'s 440 lines; **essentially all the risk sits in the ~30 lines of `unsafe`** |
| 4 · A2 + A3 | small–medium (1.5d) | ~190 | |
| 5 · aeron, zmq | medium (2d) | ~300 each | |
| — · docs | small (1d) | | spread across the above |

**≈ +2,100 / −230, ~2,500 lines across ~25 files, 8–11 working days for the
9.1.0 scope**, of which genuinely new logic is ~600–700 lines. Deferred: A4
≈ +400 / −150, fix decoder ≈ +600.

What inflates it beyond the diff: every commit runs `fmt` + `clippy --workspace
--all-targets` and every push builds and tests the workspace (`lint-all` cannot
reuse `lint`'s artifacts); each adapter conversion needs its three test tiers
plus its integration workflow.

## 9. Open decisions

1. **`unsafe` in `pool.rs`** — one `NonNull` deref with a documented invariant
   (recommended), plus a Miri job. The safe alternatives are all worse:
   `RefCell` cannot give `Deref` a `&T`, and the strong-count recycler is
   rejected in §3.1.
2. **Feature gating** — do the `*_pooled` adapter variants ship under each
   adapter's existing feature, or behind an additional `pool` feature?
3. **Where the loan budget is configured** — each adapter's options struct
   (consistent with `Iceoryx2SubOpts`) or a builder-level default? §4.4
   assumes the former; it is worth one round of use before freezing.
