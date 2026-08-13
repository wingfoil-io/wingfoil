# Wingfoil as an electronic trading platform — evaluation and roadmap

**Status: agreed direction, not a backlog.** This records an evaluation of
wingfoil as the core of an electronic trading system — what it already
serves, what is missing, and a phased plan for closing the gaps. Individual
items graduate to issues when they are actually scheduled; nothing here is
committed by being written down. Companion reading:
[`wingfoil-architecture.md`](../wingfoil-architecture.md) for the engine,
[`../crates/wingfoil/benches/README.md`](../../crates/wingfoil/benches/README.md)
("Where wingfoil currently sits") for the measured basis of the latency
claims and the four projects that move it, and
[`proposals/fpga-hdl-backend.md`](proposals/fpga-hdl-backend.md) (**Project
Metal**) for the hardware end-state this plan feeds into.

## 1. Where wingfoil stands today

**Fit for purpose now: mid-tier latency trading** — tens-of-microseconds
budgets and up (crypto, market-making and signals off commodity co-lo). The
differentiator is not raw speed but that the *same graph* backtests
deterministically, runs live, and stamps its own latency. The trading-shaped
surface that already exists:

- **Deterministic backtest/live parity, structurally.** Engine time vs the
  wall snap, `HistoricalFrom`, timestamped `channel` replay, tick-time-exact
  test conventions. This is the property hand-rolled trading systems
  invariably get wrong, and here it is enforced by types rather than
  discipline.
- **A hot path with HFT-credible bones.** No locks on the cycle path, no
  per-cycle allocation (allocator-counter-tested, not just claimed), burst
  delivery that never coalesces latest-wins, a compiled tier that runs a
  37-node graph in ~19 ns/cycle, pooled zero-alloc ingress at 0.87 µs/msg.
- **A serious FIX session engine** (`adapters/fix`): initiator/acceptor, TLS,
  sequence validation, resend/GapFill, heartbeat probes, a busy-spin mode,
  venue-specific logon signing hooks.
- **A venue-neutral market-data layer** (`adapters/market`): fixed-point
  `Px`/`Qty`, an `OrderBook` with gap detection, pre-snapshot delta
  buffering, stale-snapshot protection.
- **Per-stage latency stamping** (`latency`, `Traced<T, L>`) across process
  hops, with a live three-process showcase (`examples/showcase/trading_e2e`).

**Not fit yet: competitive single-digit-µs software HFT** — ingress is
TCP/websocket-class (no kernel-bypass adapter, no exchange multicast feed
handler), and deployment discipline (pinning, NUMA, warm-up) is entirely the
operator's problem. **Never fit: sub-microsecond wire-to-wire** — that race
is won in FPGAs, which is what the HDL-backend exploration is for.

The structural claim this roadmap rests on: **every gap identified below is
an adapter, an op, or ops-tooling.** None requires touching the kernel, the
`TimeQueue`, or the tiers. The engine core does not need rework.

## 2. The ingress latency ladder

The context for the short- and medium-term items. Each rung removes a layer
between the wire and the graph:

| Rung | Path of a packet | Wire-to-decision | Wingfoil today |
|---|---|---|---|
| Kernel sockets | NIC → interrupt → kernel stack → syscall | ~5–20 µs | `ws` (tokio/threaded), `fix` `Threaded` |
| Kernel bypass, transparent (Onload) | NIC → DMA → user-space spin loop, socket API intact | ~1–2 µs | `fix` `AlwaysSpin` is already the right shape — needs deployment, not code |
| Kernel bypass, raw (ef_vi/DPDK) | NIC → DMA ring → pooled decode on the graph thread | ~1 µs, tight tails | missing — the `Activation::ALWAYS` + pool-loan pattern is ready for it |
| FPGA | parsed in gateware, CPU optional | ~40 ns–1 µs | exploratory (`proposals/fpga-hdl-backend.md`, #727) |

Three consequences worth writing down so they are not re-derived:

- **TCP protocols (FIX, WebSocket) get bypass via Onload-style transparent
  acceleration, not via a raw-frame adapter.** Raw ef_vi/DPDK delivers
  layer-2 frames and forfeits the kernel's TCP stack; Onload intercepts the
  socket calls and services them from user space, so the existing
  `AlwaysSpin` non-blocking-read loop accelerates with **zero code changes**.
  rustls sits above the socket and is unaffected.
- **The raw-frame adapter is for UDP multicast exchange feeds** (ITCH/SBE
  families — see §4), which are stateless datagrams and need no TCP stack.
  That is also the one ingress where zero-copy is reachable: the pool's
  loan/return protocol maps directly onto an RX descriptor ring.
- **WebSocket venues do not reward bypass.** They are reached over the
  public internet or cloud regions where path jitter is milliseconds;
  shaving 10 µs off a leg with 5 ms of variance is optimizing the wrong
  term. The `ws` adapter stays on the threaded path by design.

## 3. The functional gap against trading platforms

Measured against a batteries-included platform (NautilusTrader is the
reference point), wingfoil is an *engine with trading-adjacent adapters*,
not a platform. Missing, in rough effort order: a fill simulator /
matching engine for backtests (the hardest single piece — a naive
fill-at-touch simulator lies), an execution engine + OMS (order state
machine, order types, reconciliation), position/account/portfolio,
pre-trade risk checks, a data catalog, and venue adapters (the true moat of
any incumbent platform — weeks each, forever).

What shrinks the gap: the platform property incumbents sell — identical
code backtest and live — is the part wingfoil has already solved, arguably
better; the order book, FIX engine, `feedback` edges (the order → fill →
position → strategy loop), Python bindings and stats ops all exist.

The wingfoil-native shape of the missing layer: **the simulated venue is
just an op.** The execution boundary is a swap point — live wires a venue
sink, backtest wires a `SimVenue` op consuming the order stream and the
replayed book and emitting fills; `RunMode` decides. Position keeping is a
fold over fills, risk is a filter on the order stream, PnL a join of
position and mid. Per the `market.rs` philosophy these belong in **separate
crates** (`wingfoil-sim`, `wingfoil-exec`, venue crates), out of this tree.

Deliberately *not* a goal: parity with an incumbent platform's breadth
(ten venues, community, years of battle-testing). The aim is the ~20% a
serious latency-focused shop needs, on a faster and more deterministic
core, with FIX-native execution and a latency layer the incumbents lack.

## 4. Roadmap

### Short term (weeks — sharpens what exists, no new dependencies)

1. **Onload validation run.** Run the existing `trading_e2e` showcase on a
   Solarflare NIC under Onload and publish before/after stage numbers in
   the benches README. Zero code changes; converts "the engine core is
   HFT-credible" from claim to measurement, and produces the wire-to-trade
   number the benches README currently (and deliberately) declines to claim.
2. **Runner deployment knobs.** Small additions to `runtime/`: pin the
   graph thread to a core (the spin loop under `Activation::ALWAYS` *is*
   the graph thread), a NUMA-node option on pool construction, and a
   documented warm-up recipe — drive the compiled graph over recorded data
   before go-live; historical mode already makes the *exact* production
   graph warmable. The cheap half of deployment discipline, useful to every
   latency user today.
3. **Write the ingress positioning down** as a short docs page: the ladder
   in §2, which adapter serves which rung, and why WS venues don't reward
   bypass. It is the answer to the first question every trading evaluator
   asks. (This document is the seed; a user-facing page should follow once
   the short-term items land.)

### Medium term (one to three quarters — the two adapter-shaped gaps)

4. **`mold_itch` adapter**: MoldUDP64 framing + A/B feed arbitration + ITCH
   decode, normalizing into `adapters/market`. Exchange feeds are UDP
   multicast with venue-specific binary encodings that cluster into two
   families — ITCH-lineage (Nasdaq, LSE, ASX, Nordics; fixed-layout binary
   over MoldUDP64) and SBE (CME MDP3, Eurex EOBI, Euronext Optiq, B3).
   These protocols are siblings of FIX, not layers under it: they unbundle
   FIX's three jobs (semantics / encoding / session) and solve sequencing
   with per-packet numbers plus recovery side channels instead of a
   stateful session. Develop against **pcap replay + vendor data
   (Databento)** so no venue contract is needed; the raw bypass source
   drops in behind the same API later. The `OrderBook`
   gap/snapshot/buffered-delta machinery is already the correct recovery
   model — the adapter fills in the transport it was designed for. A/B
   arbitration (two spin sources merged on sequence number, first-arrival
   wins) lives in the adapter, before the book.
5. **SBE decode path**, CME MDP 3.0 as the reference — schema-generated
   fixed-layout structs, zero-parse by construction. With #4 this covers
   most of the world's listed markets.
6. **Trading-layer phase 1, out of tree**: `SimVenue` op (limit/market
   fills against the existing `OrderBook`, conservative queue model, fees)
   + a position/PnL fold + a minimal typed order vocabulary. The
   highest-leverage slice of §3 — it makes end-to-end strategy backtesting
   possible and exercises `feedback` in anger. Defer the OMS, risk engine
   and venue breadth until a real strategy demands them.

### Long term (opportunistic — keep gated, do not start yet)

7. **Raw kernel-bypass source** (ef_vi first, DPDK second) once `mold_itch`
   exists to feed: an `Activation::ALWAYS` spin node draining the RX ring
   into pooled buffers — the iceoryx2 `Spin` shape with a DMA ring as the
   producer. Copy-once (recv+decode fused) first; true zero-copy (the
   `Pooled` handle wraps the DMA buffer, drop returns the descriptor) only
   if profiles demand it. Only worth building against real hardware and a
   real feed entitlement.
8. **FPGA — Project Metal — in the order the decision doc already implies.**
   First an
   **FPGA-sink adapter** — arming triggers/pre-canned orders into fast-path
   registers over PCIe, the industry-standard hybrid pattern (smart slow
   path in software, dumb ~100 ns trigger path in gateware). That is useful
   with commercial feed-handler/trigger cards and needs no HDL work: the
   wingfoil graph is the slow path, the FPGA is the lowest-latency actuator
   hanging off it, writable as an ordinary sink. Only later the RHDL
   emission backend (#727) — the graph *as* gateware, the backtest as the
   testbench — which stays gated behind the software codegen spike per
   [`proposals/fpga-hdl-backend.md`](proposals/fpga-hdl-backend.md). Do not
   reorder that gate.
9. **Durable FIX message store** (framing, rotation, fsync — see
   `adapters/fix/CLAUDE.md`, which names this as the next substantive step)
   when, and only when, a venue certification is actually on the calendar.

## 5. Sequencing rationale

Measure first (1) so every later claim has a number under it; then feed
coverage (4–5), because listed-markets data is the prerequisite for both
the bypass source and any credible fill simulator; then trading semantics
(6), which turns the engine into something a strategy can run on
end-to-end; then hardware (7–8), each rung of which is only exercisable
once the rung above it exists. Item 9 floats — it is certification-driven,
not sequence-driven.
