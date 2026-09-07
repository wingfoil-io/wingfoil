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

## 6. Egress — ingress-first, not ingress-only

It is tempting to scope this project to ingress and argue egress away — the
raw rung really is harder on TX. But **the latency budget is roughly
symmetric**: a 1 µs RX path behind a 10 µs kernel TX path caps the win at
half, and the *tick-to-trade* number gate P0 exists to produce is by
definition both halves. An ingress-only project measures one of them and
calls it the answer.

**What comes free, and is why P0 is still first.** Onload (and VMA)
accelerate TX transparently — `fix` order entry, rustls included, with zero
code changes, because they intercept the socket calls. So the P0 measurement
is already wire-to-wire, not wire-to-decision, and the transparent rung needs
nothing from this section.

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
| **P0 — measure** | Run `trading_e2e` under Onload on a Solarflare NIC. Zero code. Also `SO_BUSY_POLL` on `fix` `AlwaysSpin` on a commodity NIC. | Before/after per-stage numbers in the benches README, and the first wire-to-trade number the page has ever been able to claim. **If the delta is small, stop here and say so.** |
| **P1 — the seam** | `adapters/bypass`: `RxSource`, `Frame`, `RxStats`, `bypass_rx`, the `pcap` + `udp` backends, `pcap_sink`, tier-1 tests, `CLAUDE.md`. One new dependency edge, and it is `libc`, which this crate already has. | A decoder written against the seam runs unchanged over a pcap file and a live UDP socket, with a replay-determinism test pinning it. |
| **P2 — a feed to decode** | roadmap #4 `mold_itch` (MoldUDP64 + A/B arbitration + ITCH), developed against pcap/vendor data, normalising into `market`. **This is the dependency that makes P3 meaningful**, not a parallel track. | A book built from a captured session, replayed deterministically, gaps detected. |
| **P3 — the raw rung** | `xdp` backend as its own excluded-from-workspace crate (§3.2); ef_vi/DPDK backends as out-of-tree crates against the P1 seam (§3.3). Requires #392 landed. | AF_XDP-on-`veth` integration test green; a hardware ef_vi number published beside the P0 Onload number. |
| **P3b — the TX seam** | `TxSink` with the `arm()`/`fire(delta)` shape (§6), a UDP-socket backend in tree, TCPDirect/CTPIO backends out of tree. Sized *after* P0 says how much of the budget is on the TX side. | A strategy sink runs unchanged over a UDP socket and a transmit-template backend; the tick-to-trade number covers both halves of the wire. |
| **P4 — zero-copy** | The pool changes of §5, if and only if a P3 profile demands them. | The copy is out of the top three costs; the in-flight-descriptor discipline is documented and asserted. |

P3b is listed after P3 but is not blocked by it — it is blocked by P0, which
is what says whether the TX half of the budget is worth a seam at all.

Two orderings that are deliberate and should not be swapped: **P0 before
everything** (every later claim needs a baseline under it, and P0 may end the
project), and **P2 before P3** (a raw ring with nothing to decode cannot be
tested, and the decode layer is where the bugs actually are).

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
