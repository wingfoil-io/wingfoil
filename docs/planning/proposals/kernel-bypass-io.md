# Project Bypass — kernel-bypass I/O, the ingress design

**Status: designed, not scheduled.** This is the design body for the
**kernel bypass** project named in the root
[README's open projects](../../../README.md#get-involved), the benches README's
[what moves the line](../../../crates/wingfoil/benches/README.md#kernel-bypass),
and items 1 and 7 of [`../trading-roadmap.md`](../trading-roadmap.md). The
roadmap says *what* rungs exist and in which order; this says *what gets built*
at each rung, what it touches in this tree, and what the answer is when the
build is not in this tree at all.
**Tracked as [#957](https://github.com/wingfoil-io/wingfoil/issues/957)**, which
carries the §8 gates as a checklist — the issue is the status, this is the
reasoning.

> **Why `planning/proposals/` and not `decisions/`.** Nothing here is settled
> and true of `main` — §8 is a five-gate sequencing plan whose first gate is a
> measurement nobody has taken, and the later gates are explicitly conditional
> on what that measurement says. The pieces that *are* rulings — where a
> proprietary NIC dependency may live, and that engine time never comes off a
> NIC clock — are called out as such in §3.3 and §4, and should graduate to
> `decisions/` if and when they are exercised.

**Question.** What does wingfoil have to build to run its ingress off a
kernel-bypass NIC, and how much of it is engine work?

**Answer.** Almost none of it is engine work, and the largest single result of
writing this down is how little code the first two rungs need. The transparent
rung (Onload) is a **deployment**, not a diff — `Activation::ALWAYS` +
non-blocking reads is already the shape Onload accelerates. The raw rung is one
new source op behind a narrow `RxSource` trait, and its proprietary backends
(ef_vi, DPDK) belong in **out-of-tree crates**, exactly as venue adapters do
(`adapters/market/CLAUDE.md`). What *is* real work, and is underweighted
everywhere this project is currently described in one line, is the part with no
NIC in it: a **pcap capture/replay path**, without which a bypass source
silently forfeits the property that makes this engine worth pointing at a feed —
that the same graph backtests deterministically.

The scope, stated once up front: **bypass is ingress-first, not
ingress-only.** The latency budget is roughly symmetric and the tick-to-trade
number needs both halves, so egress gets a seam of its own — a much cheaper
one, because a sink needs no spin node and has no determinism consequence. But
raw-frame TX is not the mirror image of raw-frame RX, and what it costs
depends on the transport (§6).

---

## 1. The ladder, with the rungs the roadmap's table leaves out

[`../trading-roadmap.md`](../trading-roadmap.md) §2 has the four-rung ladder
(kernel sockets → Onload → ef_vi/DPDK → FPGA) and the reasoning for it; that is
not repeated here. Two rungs sit *between* its first two, and both matter to
this design because they are the ones reachable on hardware wingfoil users
actually have:

| Rung | Path of a packet | Needs | Where it lands |
|---|---|---|---|
| Kernel sockets, blocking/epoll | NIC → IRQ → stack → syscall → park | nothing | `ws`, `fix` `Threaded` today |
| **Busy-poll sockets** (`SO_BUSY_POLL`, `napi_defer_hard_irqs`) | NIC → stack, but polled from the application's spin, no IRQ, no park | a sysctl and a socket option | `fix` `AlwaysSpin` already spins; the socket option is a ~5-line addition |
| **AF_XDP** (`XDP_ZEROCOPY`) | NIC → XDP program → UMEM ring, kernel stack skipped, stack still available for other flows | Linux ≥5.4, a driver with zero-copy support, `CAP_NET_RAW` | the in-tree reference backend of §3 |
| Kernel bypass, transparent (Onload/VMA) | NIC → DMA → user-space TCP in a spin loop, socket API intact | a Solarflare/Mellanox NIC + the vendor stack | **no code** — an LD_PRELOAD and a measurement |
| Kernel bypass, raw (ef_vi/DPDK) | NIC → DMA ring → decode on the graph thread | vendor NIC, hugepages, a dedicated port | out-of-tree backend crate (§3.3) |
| FPGA | parsed in gateware | a card and gateware | Project Metal (`fpga-hdl-backend.md`) |

Why the two inserted rungs earn their place in the plan:

- **AF_XDP is the only rung with real bypass characteristics that CI can
  exercise** — a `veth` pair in a privileged container carries an AF_XDP
  socket. Every rung above it needs hardware no runner has. That makes AF_XDP
  the reference backend whether or not anyone deploys on it, because it is what
  keeps the trait in §3 honest. It pays for that with a C toolchain, which is
  why §3.2 gives it its own crate rather than a feature.
- **Busy-poll sockets are the cheapest measurable win in the tree** and apply
  to `fix` `AlwaysSpin` and any future UDP source on a commodity NIC, with no
  vendor stack at all.

**`io_uring` is not on this ladder, deliberately.** It removes syscalls, not
the stack; its win is throughput and batching under many concurrent
connections, which is not the axis this project is on, and its completion-queue
model wants a different source shape than `Activation::ALWAYS`. If it is worth
doing it is its own item, not a rung here.

## 2. What the engine already provides — an audit

The claim in the benches README is that bypass "needs a NIC, not code". That is
true of the *transparent* rung and worth checking rather than repeating, so:

**Already there, and directly load-bearing:**

- **`Activation::ALWAYS`.** A source cycled unconditionally, every cycle, on
  all three tiers — interpreted, `compiled()` (which ORs its ops' `ACTIVATION`
  and calls `Kernel::set_spin`) and islands (which declare `always` outward).
  Pinned by `tests/poll_all_tiers.rs`. A DMA-ring drain is exactly this node
  with a different producer; `adapters/iceoryx2` `Spin`, `adapters/aeron`
  `Spin` and `fix` `AlwaysSpin` are three existing instances of the shape.
- **`Burst<T>`.** One cycle drains N packets into one burst, never
  latest-wins, never dropped — which is the correct model for a ring that
  delivers in batches, and the reason a bypass source needs no new tick
  semantics.
- **The two clocks.** `Ctx::time()` (source-driven engine time) vs
  `Ctx::wall_time()` (a lazy, once-per-cycle wall snap). This is the split
  that makes §4 expressible at all.
- **`pooled_channel` / `Pooled<T>`.** Loan-and-return with zero steady-state
  allocation, allocator-counter-tested (`tests/steady_state_allocs.rs`). The
  handle is `Rc`-based and `!Send` by design: the payload crosses threads once
  as a unique `PoolLoan`, then lives on the graph thread.
- **`adapters/market`.** `Px`/`Qty`, `OrderBook`, gap detection, pre-snapshot
  delta buffering — the recovery model a multicast feed needs, already built
  and already the normalisation target.
- **`latency` / `Traced<T, L>`.** Per-stage stamping across process hops, so
  "wire-to-decision" becomes a number the graph reports about itself.
- **`source_at_start`.** I/O established at `start()`, not at wiring, so a
  source that opens a NIC queue keeps wiring pure and unit-testable.

**Not there, and each one is a line item below:**

| Gap | Why it bites | Where |
|---|---|---|
| No pcap capture/replay path | a bypass source that only runs live forfeits determinism, the differentiator | §4, gate P1 |
| `PooledSender` is single-producer and `Box<T>`-backed | a DMA buffer is foreign memory, not a `Box`; and `loan()` **blocks** when exhausted, which on an RX ring means dropping frames while stalled | §5 |
| No core pinning in `runtime/` | the spin loop *is* the graph thread; unpinned, a bypass NIC buys jitter | #392, a hard prerequisite (§8) |
| No hardware-timestamp channel into the graph | NIC RX timestamps are the only honest wire-side latency origin | §4.3 |
| No UDP multicast feed handler | a raw source with nothing to decode is untestable and unshippable | roadmap #4 (`mold_itch`), a hard prerequisite |

The audit's conclusion matches the roadmap's structural claim: **nothing here
requires touching the kernel, the `TimeQueue`, or the tiers.** The two engine-
adjacent items are `runtime/` knobs (#392) and a pool constructor (§5).

## 3. The design

### 3.1 One trait, several backends

```rust
// crates/wingfoil/src/adapters/bypass/mod.rs  — feature `bypass`
pub trait RxSource {
    /// Drain up to `max` frames, calling `f` per frame. Returns frames seen.
    /// MUST NOT block, allocate, or make a syscall on the empty path.
    fn poll_rx(&mut self, max: usize, f: &mut dyn FnMut(Frame<'_>)) -> Result<usize>;
    /// Counters the source publishes as a side stream (drops, ring full, ...).
    fn stats(&self) -> RxStats;
}

pub struct Frame<'a> {
    pub bytes: &'a [u8],
    /// NIC hardware timestamp, when the backend has one. Never engine time (§4).
    pub hw_time: Option<NanoTime>,
}
```

The graph-facing surface is one free function, per adapter convention:

```rust
let frames: Stream<Burst<Pooled<FrameBuf>>> =
    bypass_rx(&g, RxConfig::new(backend).batch(64))?;
```

`bypass_rx` is a `custom_node` with `Activation::ALWAYS` that drains the
backend into a burst, and rejects `RunMode::HistoricalFrom` at wiring
(register B2) — replay comes from the pcap source in §4, not from this node.

What the trait buys: the decode side (`mold_itch`, SBE, and anything a user
writes) is written **once**, against `Burst<Pooled<FrameBuf>>`, and is
identical whether the frames came from a pcap file, an AF_XDP UMEM ring, an
ef_vi event queue or a plain `recvmmsg`. That equivalence is the whole
architecture; everything else is a backend.

### 3.2 The backends, and what each costs in dependencies

**The default build gains nothing.** Everything here is optional, behind
`bypass`.

| Backend | Where | Deps | Purpose |
|---|---|---|---|
| `pcap` | `crates/wingfoil`, feature `bypass` | none (see below) | replay + every test; the default |
| `udp` | `crates/wingfoil`, feature `bypass` | `libc` — **already a workspace dep**, already `optional` in this crate | the commodity-NIC rung, and the honest baseline every faster backend is measured against |
| `xdp` | `crates/wingfoil-bypass-xdp`, its own crate | `xsk-rs` → `libxdp-sys`/`libbpf-sys` → clang, libbpf, libelf, zlib | the reference bypass backend, CI-exercisable on `veth` |

The `udp` backend is not filler. Without it there is no in-tree number to
compare a bypass backend *to*, and the project's whole claim is a delta. `libc`
covers all of it — `recvmmsg`, `SO_BUSY_POLL`, `setsockopt`,
`IP_ADD_MEMBERSHIP`. Not `socket2`: it is in the lock but not in this crate's
default graph (our `tokio` has no `net` feature), and it does not cover
`recvmmsg`, so it would be a second dependency for socket *setup* alone.

**Two judgment calls at P1, both recorded rather than settled:**

- **pcap parsing.** `pcap-file` (pure Rust, MIT) against ~120 lines for the
  classic format's 24-byte global header and 16-byte per-packet header. Start
  hand-rolled — the published manifest is the expensive place to put something
  this small — and take the crate the moment **pcapng or a second linktype**
  is needed. Behind the seam it is a one-line swap.
- **Frame headers.** `etherparse` against ~60 lines to skip Ethernet/IPv4 to
  the UDP payload. Same call, with a nearer tipping point: **VLAN tags** (common
  on exchange feeds) or IPv6 make the crate the right answer immediately.

**Why `xdp` gets its own crate rather than a `bypass-xdp` feature.** A feature
cannot be excluded from `--all-features`. Put `xsk-rs` in `crates/wingfoil` and
every `cargo lint-all`, every CI all-features job and docs.rs need libbpf and
clang installed — the third such tax after aeron (clang, libbsd, cmake ≥3.30)
and iceoryx2, and the same trap `libbsd-dev` set, which fails at *link* time
after a long successful build. So `crates/wingfoil-bypass-xdp` is a workspace
member **excluded from the default workspace**, exactly as `wingfoil-wasm` is,
with its own integration workflow. It stays this repo's code and this repo's
CI; it just stops being in the root build's feature union.

Adding all of this is a **minor** version bump under the dependency policy:
new optional dependencies behind a new feature, nothing on the public API.

### 3.4 What this looks like from the user's side

**For an existing user: nothing.** Additive, `bypass` off by default, out of
the prelude like every other adapter, no engine/`Op`/`Tick`/`Burst` change. The
one cross-cutting addition is the `runtime/` core-pin knob, which is #392 and
opt-in.

**For someone using it**, the shape is the swap point the `market` example
already demonstrates one rung up (`examples/adapters/market/main.rs`'s
`FeedBuilder`): the transport line differs, everything downstream does not.

```rust
use std::sync::Arc;
use std::time::Duration;

use wingfoil::Burst;
use wingfoil::adapters::bypass::{FrameBuf, RxConfig, RxStats, UdpBackend, bypass_rx, pcap_rx};
use wingfoil::adapters::market::{BookUpdate, MarketBookOps, OrderBook};
use wingfoil::adapters::mold_itch::MoldItchOps;   // roadmap #4, gate P2
use wingfoil::pool::Pooled;
use wingfoil::prelude::*;
use wingfoil_bypass_xdp::XdpBackend;              // §3.2, its own crate
use wingfoil_bypass_efvi::EfViBackend;            // §3.3, out of tree entirely

// ── The swap point: one of these, and it is the only line that differs ──────
let rx: RxConfig<UdpBackend>  = RxConfig::udp("239.1.1.1:16001")?.busy_poll();
let rx: RxConfig<XdpBackend>  = RxConfig::new(XdpBackend::open("eth0", 3)?);
let rx: RxConfig<EfViBackend> = RxConfig::new(EfViBackend::open("eth0")?);

let frames: Stream<Burst<Pooled<FrameBuf>>> = bypass_rx(&g, rx)?;   // RunMode::RealTime
// …or the backtest, same type out:
let frames: Stream<Burst<Pooled<FrameBuf>>> =
    pcap_rx(&g, "captures/2026-09-04.pcap")?;                       // RunMode::HistoricalFrom

// ── Identical downstream, whichever of the four produced `frames` ───────────
let updates: Stream<Burst<BookUpdate>> = frames.mold_itch(instrument);
let book:    Stream<Arc<OrderBook>>    = updates.order_book();
```

The type that carries the whole design is `Stream<Burst<Pooled<FrameBuf>>>`:
`Burst` because a ring delivers in batches, `Pooled` because the buffer is
loaned rather than allocated, and **the same type from a file as from a DMA
ring** — which is what makes `mold_itch` and everything above it
transport-blind.

**Writing the types out surfaces something the prose hid.** `RxConfig<B>`
makes `bypass_rx` generic over the backend, so the four lines above are a
*compile-time* choice. The `market` example's swap point is a **runtime** one
— `feed_from_args()` returns a `Box<dyn FeedBuilder>` — and any deployment
picking its backend from a flag or an env var wants that. Which means either
`Box<dyn RxSource>` at the ingress node (one dyn call per *drain*, not per
frame — almost certainly free, and it is what the interpreted tier does
everywhere anyway) or a backend enum. Settle it at P1; it is open question 5.

Drop counters are the one part this snippet cannot state honestly yet, because
it is open question 2. The side-stream candidate reads:

```rust
let (frames, stats): (Stream<Burst<Pooled<FrameBuf>>>, Stream<RxStats>) =
    bypass_rx_with_stats(&g, rx, Duration::from_secs(1))?;
```

which costs a node on the hot path; the alternative is a `Cell` snapshot
sampled by an ordinary timer node. Settle it at P1, since it changes this
signature.

`Cargo.toml` is `features = ["bypass", "market"]`, plus one ordinary
dependency for the raw rung. The same binary backtests and runs live;
pinning, NUMA and hugepages are runner configuration rather than code.

**Three things a user must learn that nothing else in the tree teaches**, and
each is a documentation obligation on P1 rather than an afterthought:

1. **It burns a core.** `Activation::ALWAYS` means the spin loop *is* the graph
   thread — pair it with the core pin, and expect the live source to be
   rejected at wiring under `HistoricalFrom` (replay is `pcap_rx`).
2. **Normalise before you buffer.** A `window()` or `buffer()` over raw frames
   holds descriptors out of the ring (§5); do it after the decode.
3. **Drops are a stream, not a log line.** `RxStats` has to be wired somewhere
   or the user is blind to the failure mode bypass is most likely to have.

### 3.3 Where ef_vi and DPDK live — a ruling

**Out of tree, in their own crates, depending on `wingfoil = { features =
["bypass"] }` and implementing `RxSource`.** This is the `market` rule applied
to transports rather than venues, and it holds for the same two reasons plus a
third:

1. Their build requirements (an Onload installation; DPDK's hugepages,
   `pkg-config` and a ~1M-line C tree) never enter this crate's dependency
   graph, CI matrix, `cargo deny` surface or docs.rs build.
2. This crate stays neutral infrastructure — the tree already declines to carry
   venue code for the same reason.
3. **Licensing and distribution.** ef_vi ships inside Onload under vendor
   terms and is not a crates.io dependency in any honest sense; DPDK is BSD-3
   but unvendorable at this crate's size. A published library cannot take
   either as an optional dependency without lying about what `--all-features`
   means.

The in-tree obligation is the seam and its documentation: `RxSource`,
`Frame`, `RxStats`, a worked out-of-tree backend example, and the statement in
`adapters/bypass/CLAUDE.md` of what such a crate owes the contract — mirroring
what `market/CLAUDE.md` already does for venue crates.

## 4. Determinism is the part that is not optional

The engine's differentiator is that the same graph backtests and runs live. A
source that exists only on a spinning NIC queue is the one adapter shape that
can quietly cost that, so the design fixes it in three places.

### 4.1 Capture is a sink, replay is a source

`bypass_rx` gets a sibling sink, `pcap_sink`, and a replay source,
`pcap_rx(&g, path)`, that emits the same `Stream<Burst<Pooled<FrameBuf>>>` from
a capture file with `send_at`-style timestamped delivery under
`RunMode::HistoricalFrom`. Every packet the live path can see, the replay path
can produce. This is what makes the decode layer testable without hardware, and
it is why `pcap` is the *default* backend rather than a test fixture.

### 4.2 `recv_time` is engine time. Always.

`adapters/market` already names this as the single most likely adapter bug: an
adapter that stamps `recv_time` from `NanoTime::now()` has broken determinism,
and it will not show up in a live test. A NIC hardware timestamp is a *more*
tempting version of the same mistake, because it is more accurate and more
obviously "the truth". It is still wall-clock, and business logic branching on
it is non-replayable. **`recv_time` comes from `Ctx::time()`; `hw_time` is
telemetry.**

### 4.3 What hardware timestamps *are* for

Two legitimate uses, both outside business logic:

- A `latency` stage origin — the wire-side zero point that finally makes
  "wire-to-decision" measurable end to end rather than from the first thing the
  process does.
- Feed-arbitration diagnostics: A/B skew on a dual-fed multicast feed, which is
  a monitoring signal, not an input to the book.

Both mean `hw_time` travels in `Traced<T, L>` / the frame envelope, never into
`Ctx::time()`.

## 5. Zero-copy: copy-once first, and what the pool would have to grow

The roadmap's phrasing — copy-once (recv+decode fused) first, true zero-copy
only if profiles demand it — is right, and this section exists to record what
"true zero-copy" would actually cost, so that nobody discovers it mid-build.

**Copy-once** is available immediately: `poll_rx` hands out `Frame<'_>`
borrowing the ring, the decoder writes normalised messages into a `Pooled<T>`
loan, and the descriptor is returned to the ring inside the same cycle. Zero
allocation, one copy, no lifetime leaves the cycle. This is what ships.

**True zero-copy** — `Pooled<T>` wrapping the DMA buffer, drop returning the
descriptor — needs three things the pool does not have:

1. **A foreign-memory loan.** `PoolLoan<T>` owns a `Box<T>` and returns it down
   a channel on drop. A DMA-backed loan owns a descriptor index and must return
   *that* to a ring the producer owns. That is a second constructor plus a
   return channel abstraction, not a change to `Pooled`'s graph-side contract
   (which stays `Rc`-based, `!Send`, refcount-returns-on-last-drop).
2. **A non-blocking exhaustion path.** `PooledSender::loan()` blocks when the
   full capacity is in flight — correct backpressure for a producer thread,
   *wrong* for an RX ring, where stalling means the NIC drops frames while you
   wait and you lose the drop count too. A DMA pool must fail the loan and
   count it, surfacing the count on the `RxStats` side stream.
3. **A holding-time discipline.** Any graph node that retains a `Pooled` frame
   across cycles holds a descriptor out of the ring. That is a documented
   contract (`window`/`buffer` over raw frames is a bug; normalise first), and
   worth a debug-mode assertion on maximum in-flight age.

The gate: build it when a profile shows the copy in the top three costs of the
ingest path, and not before. The copy is ~64–1500 bytes into L1 that the
decoder is about to touch anyway.

## 6. The TCP side: egress, and FIX

It is tempting to scope this project to ingress and argue egress away — the
raw rung really is harder on TX. But **the latency budget is roughly
symmetric**: a 1 µs RX path behind a 10 µs kernel TX path caps the win at
half, and the *tick-to-trade* number gate P0 exists to produce is by
definition both halves. An ingress-only project measures one of them and
calls it the answer.

**What comes free, and is why P0 is still first.** Onload (and VMA)
accelerate TX transparently — `fix` order entry included, with zero code
changes, because they intercept the socket calls rather than replacing the
datapath. So the P0 measurement is already wire-to-wire, not wire-to-decision,
and the transparent rung needs nothing from this section. §6.1 has the
`fix`-specific detail, which is where the caveats live.

**What the raw rung looks like on TX**, and why it is still out of tree:

- **You do not have to write a user-space TCP stack on Solarflare.**
  TCPDirect (`zf`) is one, shipped inside Onload, and ef_vi has transmit
  templates (`ef_vi_transmit_alloc_template` + a small delta pushed at
  decision time) and CTPIO for cut-through sends — which *is* the classic
  pre-canned-order trick, at sub-microsecond and with a tight tail. This is a
  vendor library, so it lands under the same ruling as ef_vi RX (§3.3): an
  out-of-tree backend crate.
- **The caveat that survives: TCPDirect is not a socket API.** rustls is a
  state machine over buffers and can be driven over it, but not for free — a
  TLS venue on the raw rung is real work that the transparent rung does not
  ask for. One more reason P0 precedes P3.
- **DPDK genuinely has no TCP.** Pairing it with F-Stack/TLDK/mTCP to send FIX
  *is* the order-of-magnitude project. On DPDK, egress means UDP order entry
  or nothing.
- **UDP order entry** is a small, venue-specific set, and a venue-crate
  concern when one is actually on the calendar.

**So the in-tree obligation is a `TxSink` seam symmetric to `RxSource`**, and
it is much cheaper than the ingress side for two reasons: a sink needs no
spin node (it is an ordinary extension trait on `Stream<Burst<T>>`, per
adapter convention), and **sinks have no determinism consequence** — none of
§4's pcap machinery has an egress twin. What it needs is a pre-armed-frame
shape (`arm()` / `fire(delta)`) that a transmit-template or CTPIO backend can
implement and a plain UDP socket can emulate, so the strategy code above it
does not know which it is talking to.

### 6.1 What this means for the `fix` adapter

FIX is the case users will ask about first, and the answer is not the frame
seam: `bypass_rx` yields L2 frames and a FIX session is a byte stream. FIX
rides the **transparent** rung, and the adapter is already the right shape for
it — `FixPollMode::AlwaysSpin` is a non-blocking read on the graph thread
(`custom_node` + `Activation::ALWAYS`), which is precisely what Onload and VMA
intercept. **Zero diff, and both directions of the session at once**, since
socket interception is not direction-specific.

The one FIX-specific diff at P0 is `SO_BUSY_POLL` on that socket for the
commodity-NIC rung — a handful of lines, and open question 4 decides whether
it belongs on `FixOptions` or on the runner beside the core pin.

**Three constraints P0 must publish alongside its numbers**, all already true
of the adapter and all discoverable the expensive way:

- **`AlwaysSpin` is plaintext-only and does not reconnect.** So the accelerated
  FIX path is the co-lo cross-connect shape, not the internet-facing one. A TLS
  venue runs `Threaded`, which Onload still accelerates (rustls sits above the
  socket) but which reintroduces the channel hop the spin mode exists to
  remove. Quote the two modes' numbers separately or the table lies.
- **`FixSeqNumStore::File` puts a write syscall on the graph thread under
  `AlwaysSpin`** — `fix/CLAUDE.md` already says pair it with `Threaded`. Behind
  a bypass NIC that stops being a footnote and becomes the dominant term.
- **Two spinners, one core.** Onload has its own spin (`EF_POLL_USEC`)
  underneath the graph's spin loop. Pinned to one isolated core without
  thinking about it, they contend; this belongs in the deployment recipe of §7,
  not in a user's incident review.

**The raw rung is where FIX gets structurally harder**, and it is the reason
§6's TCPDirect caveat matters: `zf` is not a socket API, so `fix.rs` would need
a byte-stream transport seam — non-blocking connect/read/write, `TcpStream` as
today's backend, TCPDirect as an out-of-tree one — plus rustls driven over
buffers by hand. What survives that change unaltered is the subtle part:
`write_frame` / `pending_out`, the "never spin, never tear" backpressure
machinery, is transport-agnostic. This is P3b work, gated on P0 saying the
residual kernel-TCP cost is worth it.

**The end state is still the FPGA sink** — pre-canned orders armed over PCIe,
the hybrid the roadmap's item 8 describes, and the same `arm()`/`fire()`
shape one level further down. Designing the seam here is what makes that a
backend swap rather than a rewrite.

## 7. Deployment, testing, CI

**Deployment discipline** (mostly #392, listed because a bypass NIC without it
measures worse than a socket): pin the graph thread to an isolated core
(`isolcpus`/`nohz_full`), pin the NIC IRQs and RSS queues to the same NUMA
node, allocate the pool on that node, hugepages where the backend wants them,
and warm the graph over recorded data before go-live — which historical mode
already makes possible against the *exact* production graph. The spin loop must
make **no syscall on the empty path**: no logging, no allocation, and note that
`Ctx::wall_time()`'s laziness means a cycle in which nothing stamps reads no
clock at all — a per-frame `NanoTime::now()` in a drain loop would undo that at
~24 ns a go.

**Tests**, in the three tiers `adapters/CLAUDE.md` defines:

1. `tests/bypass_adapter.rs`, `#![cfg(feature = "bypass")]` — the pcap backend
   end to end: replay determinism (two runs, identical tick times and values),
   burst grouping, pool exhaustion accounting, wiring-time rejection of
   `HistoricalFrom` on the live source and of `RealTime` where it does not
   apply. No NIC, no privileges — this is where the design is actually pinned.
2. AF_XDP over a `veth` pair in a privileged container, with its own
   `.github/workflows/bypass-integration.yml` registered in
   `integration-tests.yml`, matching every other adapter. It lives in
   `crates/wingfoil-bypass-xdp`'s own `tests/` rather than this crate's,
   because that crate is outside the default workspace (§3.2) — which is also
   what keeps its C toolchain out of the root build's requirements.
3. **Hardware, manual, published as numbers not as CI**: Onload on a Solarflare
   NIC, then ef_vi. The output is a benches README section, not a green tick.

**Benches**: extend the ingress comparison already in `benches/README.md` with
a `bypass_ingest` row per backend against the same pooled pipeline, so the
delta is stated in the same units as everything else on that page.

## 8. Gates, in order, each with an exit criterion

Each gate is cheap enough to abandon at, and none of them is started before its
predecessor has produced a number.

| Gate | Work | Exit criterion |
|---|---|---|
| **P0 — measure** | Run `trading_e2e` under Onload on a Solarflare NIC. Zero code. Also `SO_BUSY_POLL` on `fix` `AlwaysSpin` on a commodity NIC. Publish `AlwaysSpin` and `Threaded` separately, with §6.1's three constraints. | Before/after per-stage numbers in the benches README, and the first wire-to-trade number the page has ever been able to claim. **If the delta is small, stop here and say so.** |
| **P1 — the seam** | `adapters/bypass`: `RxSource`, `Frame`, `RxStats`, `bypass_rx`, the `pcap` + `udp` backends, `pcap_sink`, tier-1 tests, `CLAUDE.md`. One new dependency edge, and it is `libc`, which this crate already has. | A decoder written against the seam runs unchanged over a pcap file and a live UDP socket, with a replay-determinism test pinning it. |
| **P2 — a feed to decode** | roadmap #4 `mold_itch` (MoldUDP64 + A/B arbitration + ITCH), developed against pcap/vendor data, normalising into `market`. **This is the dependency that makes P3 meaningful**, not a parallel track. | A book built from a captured session, replayed deterministically, gaps detected. |
| **P3 — the raw rung** | `xdp` backend as its own excluded-from-workspace crate (§3.2); ef_vi/DPDK backends as out-of-tree crates against the P1 seam (§3.3). Requires #392 landed. | AF_XDP-on-`veth` integration test green; a hardware ef_vi number published beside the P0 Onload number. |
| **P3b — the TX seam** | `TxSink` with the `arm()`/`fire(delta)` shape (§6), a UDP-socket backend in tree, TCPDirect/CTPIO backends out of tree, plus the `fix.rs` byte-stream transport seam (§6.1) if a TCP venue is in scope. Sized *after* P0 says how much of the budget is on the TX side. | A strategy sink runs unchanged over a UDP socket and a transmit-template backend; the tick-to-trade number covers both halves of the wire. |
| **P4 — zero-copy** | The pool changes of §5, if and only if a P3 profile demands them. | The copy is out of the top three costs; the in-flight-descriptor discipline is documented and asserted. |

P3b is listed after P3 but is not blocked by it — it is blocked by P0, which
is what says whether the TX half of the budget is worth a seam at all.

### 8.1 Effort

**Dev effort for one experienced Rust dev already familiar with this tree**, at
this repo's bar — tests, `CLAUDE.md`, module docs and a CI workflow are roughly
a third of every number below and are included in it. **Calendar is longer**:
hardware procurement and a feed entitlement dominate the early gates, and no
amount of staffing compresses them.

| Gate | Item | Effort |
|---|---|---|
| **P0** | `trading_e2e` under Onload, per-stage capture | 2–3 d |
| | `SO_BUSY_POLL` on `fix` `AlwaysSpin` + commodity-NIC run | 2–3 d |
| | Benches README write-up, both `fix` modes quoted separately | 1–2 d |
| | **subtotal** (zero code for the Onload half) | **1.5–2 w** |
| **P1** | `RxSource` / `Frame` / `RxStats` / `RxConfig` + `bypass_rx` | 1 w |
| | pcap reader + `pcap_rx` + `pcap_sink` | 1 w |
| | `udp` backend (`recvmmsg`, multicast join, busy poll) | 1 w |
| | tier-1 tests (determinism, bursts, exhaustion, rejections) | 1 w |
| | docs, `CLAUDE.md`, adapters index, example + README | 0.5–1 w |
| | **subtotal** | **~5 w** |
| **P2** | MoldUDP64 framing, sequencing, recovery hooks | 1.5 w |
| | ITCH decode, one venue | 2–3 w |
| | A/B feed arbitration | 1 w |
| | normalisation into `market` + `OrderBook` integration | 1 w |
| | tests against captured/vendor data, docs | 1.5 w |
| | **subtotal** (+ a data entitlement as cost/calendar) | **~7–8 w** |
| **P3** | #392 core pin, promoting the example code into `runtime/` | 1 w |
| | `crates/wingfoil-bypass-xdp` + `xsk-rs` backend | 2 w |
| | `veth` integration test + workflow | 1 w |
| | ef_vi out-of-tree crate (bindgen, EQ drain, pooled loans) | 3–4 w |
| | profiling, tuning, published numbers | 1 w |
| | **subtotal** | **~7–9 w** |
| **P3b** | `TxSink` + `arm()`/`fire(delta)` + in-tree UDP backend | 2 w |
| | tests + docs | 1 w |
| | `fix.rs` transport seam, preserving `write_frame`/`pending_out` | 3 w |
| | TCPDirect backend out of tree, rustls over buffers | 4–6 w |
| | **subtotal** | **~6 w**, or **10–12 w** with TCPDirect |
| **P4** | foreign-memory loan + non-blocking exhaustion in `pool.rs` | 1.5 w |
| | descriptor discipline, debug assertion, docs | 0.5 w |
| | profile-driven validation | 1 w |
| | **subtotal** (conditional on a P3 profile) | **~3 w** |

**Totals.** Ingress to a real bypass feed (P0+P1+P2+P3) is **~21–24 weeks**;
egress adds **6 w**, or **10–12 w** if a TCP venue needs the raw rung;
everything including zero-copy is **~30–39 weeks**, call it 7–9 months for one
dev. SBE (roadmap #5) is a further 4–6 w and is not in those totals.

**Lines, for scale rather than for planning.** Calibrated against what
comparable work in this tree measures — `iceoryx2` 1,850 src / 397 test, `zmq`
818, `market` 1,938 / 429, `fix` 4,670, `pool.rs` 440 — and counted the way
this repo writes Rust, where 25–35% of a file is doc comment.

| Gate | In this repo | Out of tree |
|---|---|---|
| P0 | ~50 (the `SO_BUSY_POLL` option; the Onload half is zero) | — |
| P1 | ~2,800–3,700 | — |
| P2 | ~4,000–5,400 | — |
| P3 | ~1,350–1,950 | ef_vi crate ~900–1,400 |
| P3b | ~1,300–2,000 (plus ~600–900 of `fix.rs` touched) | TCPDirect ~1,200–2,000 |
| P4 | ~480–800 | — |
| **total** | **~10,000–13,800** | **~2,100–3,400** |

Sanity-check that against the effort above and it comes out near 350 lines a
week, which is right for this bar and wrong as a typing rate — the difference
is docs, tests, review and hardware bring-up. **Treat LOC as the least
reliable number on this page**: a third of it is doc comment, and the two
gates that actually decide the project — P0, and P1's pcap determinism work —
are the ones where a line count says least about the risk. P0 is fifty lines
and two weeks.

Two things the shape of that table says out loud. **P2 is a third of the
total and is not bypass work at all** — it is a feed handler, valuable on its
own, and the reason the raw rung is untestable without it. And **P0 is two
weeks that can save the other twenty-eight**, which is why it is first and why
its exit criterion is allowed to end the project.

Two orderings that are deliberate and should not be swapped: **P0 before
everything** (every later claim needs a baseline under it, and P0 may end the
project), and **P2 before P3** (a raw ring with nothing to decode cannot be
tested, and the decode layer is where the bugs actually are).

### 8.2 What we expect, so that P0 can falsify it

A measurement gate with no prediction attached cannot fail, so this is the
prediction. **Separate what this tree has measured from what is
vendor-typical**, and do not let the second column into the benches README
until P0 has replaced it with our own numbers.

Measured here (benches README — dev VMs, shape not spec): compiled tier ~19
ns/cycle for a 37-node graph; pooled ingress 0.87 µs/msg; `fix` `AlwaysSpin`
~1–5 µs against `Threaded` ~10–100 µs; iceoryx2 `Spin` ~1–5 µs. Typical for
the ladder, from vendor and literature figures rather than from us: kernel
sockets 5–20 µs wire-to-decision, Onload 1–2 µs, raw ef_vi ~1 µs with much
tighter tails.

The structural fact those two lists imply, and the reason this project is an
I/O project rather than an engine one: **the engine is already a rounding
error.** 19 ns–0.87 µs of graph cost sits under 5–20 µs of kernel stack, so
the transport is upwards of 90% of the wire-to-decision budget. Hence:

| Gate | Expected return | Falsified if |
|---|---|---|
| **P0** Onload | 3–15 µs off *each* leg (interception is bidirectional). Illustrative tick-to-trade ~11–41 µs → ~3–5 µs, i.e. **3–10×**, for two weeks and fifty lines | the accelerated path does not reach the 1–2 µs band — then the ladder does not apply to us and the project stops here |
| **#392** core pin | plausibly the largest win per week spent — pinning was the dominant end-to-end win in the showcase deployment | unpinned and pinned runs are within noise on an isolated core |
| **P3** raw ef_vi | little on the mean (~0.5–1 µs) and a lot on the **tail** — no IRQ coalescing, no softirq, no scheduler between wire and loop; p99.9 is the number to quote | p99.9 does not separate from Onload's, in which case P3 buys a dependency and nothing else |
| **P4** zero-copy | ~100–200 ns/msg | it fails to reach the top three costs in a P3 profile — the gate as already written |
| **WS venues** | ~0. Millisecond path jitter eats all of it | n/a — this is the §2 ruling, not a prediction |

**And the consequence to plan for if P0 succeeds:** once the transport is
~1 µs, decode and book-building become the dominant term. P2 stops being a
prerequisite for the raw rung and becomes the thing worth optimising — which
is an argument for doing P2 well rather than minimally.

## 9. What would make this not worth doing

Written down so the project can be killed cleanly rather than drifting:

- **P0 shows a small delta.** If Onload buys 1–2 µs on a path whose venue
  jitter is milliseconds — true of every WebSocket venue, per the roadmap §2 —
  the honest conclusion is that this engine's users are not on this ladder, and
  the effort belongs in `mold_itch` and the trading layer instead.
- **Nobody has a feed entitlement.** ef_vi against no multicast feed is a
  benchmark of a loopback.
- **The spin cost is not acceptable.** A bypass source burns a core
  permanently. For a user running many graphs, that is the dominant cost and
  the threaded path is correct.

## 10. Open questions

1. **`Frame<'_>` vs a pooled frame at the seam.** The borrowed form is
   zero-copy-ready but makes the trait object dance (`&mut dyn FnMut`) part of
   the public contract. The alternative — `poll_rx` fills a `Pooled<FrameBuf>`
   directly — is simpler and forecloses §5. Prototype both against `pcap`
   before P1 freezes the seam.
2. **Where `RxStats` surfaces.** A side `Stream<RxStats>` is the wingfoil-shaped
   answer, but it costs a node on the hot path; a `Cell`-backed snapshot read by
   a slow timer node may be better.
3. **Multi-queue / RSS.** One `RxSource` per queue and one graph per core, or
   one graph draining N queues? The former is the industry pattern and the
   engine's one-graph-one-thread rule points at it, but it makes A/B feed
   arbitration cross-graph.
4. **Whether the `udp` backend should carry `SO_BUSY_POLL` or that should be a
   `runtime/` deployment knob beside the core pin.** It is a socket option, but
   it is deployment discipline in spirit.
5. **Compile-time or runtime backend selection.** `RxConfig<B>` is generic, so
   the backend is a type; `market`'s `FeedBuilder` swap point is a `Box<dyn>`,
   so the backend is a flag. A deployment wants the latter, and `Box<dyn
   RxSource>` costs one dyn call per drain rather than per frame. Decide before
   P1 freezes `bypass_rx`'s signature (§3.4).
