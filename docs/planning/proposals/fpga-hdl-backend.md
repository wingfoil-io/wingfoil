# Project Metal — FPGA/Verilog as a third backend, the RustHDL/RHDL emission design

**Status: exploratory design, not scheduled.** This records the reasoning
and the de-risk plan; nothing is committed to build. **Project Metal** is the
name this work carries across the rest of the docs — the root
[README's open projects](../../../README.md#get-involved), the benches README's
[what moves the line](../../../crates/wingfoil/benches/README.md#what-moves-the-line),
and `../trading-roadmap.md` §4.8.
**Tracked as [#727](https://github.com/wingfoil-io/wingfoil/issues/727)**,
which carries the §7 de-risk spike as a checklist. It is **gated behind Project
Lightning**, the software generator of
[`wired-graph-codegen.md`](wired-graph-codegen.md) — read
that first: this document reuses Lightning's front-end (wired-graph traversal +
recorded closure metadata) and adds a hardware emission backend behind it.

> **Why this is filed under `planning/proposals/` rather than `decisions/`.**
> A decision record answers a question and freezes; this one's own status is
> *we have not decided*, its §7 is a spike checklist and its §8 is a four-gate
> sequencing plan interleaved with `../trading-roadmap.md`. It is also
> rewritten as facts change (see the revision note below), which is what plans
> do and decision records do not. The reasoning is still worth keeping — it is
> just not a ruling.
>
> **On "the generator as built" below:** that revision was written against the
> [#769](https://github.com/wingfoil-io/wingfoil/pull/769) branch. **That PR is
> open and unmerged**, so the software generator this document is gated behind
> does not exist on `main`. Read §2's audit as "what the generator *would* hand
> the hardware backend", not as a property of the shipping tree.

**Question.** Could the wired-graph front-end also emit hardware — generate
RustHDL/RHDL code that in turn generates Verilog, so a graph's hot path
(feed handler, signal pipeline, tick-to-trade) runs on an FPGA while the
same wiring backtests in software?

**Answer.** Architecturally yes, and the pieces line up strikingly well —
the Op pattern is accidentally RTL-shaped. But it is a much bigger lift than
the software generator (a restricted type dialect, per-op RTL twins with
simulation-based parity, clock/valid semantics design) stacked on a
pre-release one-person dependency. Position it as **backend #3** of the
multi-front-end design, de-risked by a small hand-written spike before any
emitter exists.

> **Revision 2026-08-09 — rewritten against the generator as built on the
> #769 branch.** This document was written before any software generator
> existed. With one implemented on
> [#769](https://github.com/wingfoil-io/wingfoil/pull/769) (still unmerged),
> the central claim could be checked against real code rather than assumed,
> and it did not survive intact:
>
> - **§2 is new, and is the section to read.** It audits what the generator
>   actually hands the hardware backend. The answer is "most of it, but not
>   the part this document led with" — closure quotation, the advertised
>   bridge, is the *weakest* carryover (§2b), while the unglamorous
>   machinery (traversal, refusals, capture detection) carries cleanly.
> - **§5.4 is new and is the load-bearing wall.** Pipeline skew was absent
>   from the original, whose worked example (§4) is a single path and
>   therefore cannot exhibit it.
> - **§5.1 is corrected.** The type dialect is not a thing to introduce —
>   wingfoil already ships fixed-point `Px`/`Qty`. They are `i128`, which is
>   the actual problem.
> - **§6 is new** — the host↔card interface, needed whether or not a line of
>   HDL is ever generated.
> - **§7's spike is amended** so it can fail. As originally written it could
>   only succeed.

---

## 1. The target crates (surveyed 2026-07, widened 2026-08)

### 1a. The two Rust-embedded candidates

Both are by the same author (Samit Basu):

- **`rust-hdl`** — the original. Mature-ish and complete: struct-based
  circuits (`Logic` trait with an `update()` kernel), a widget library
  (FIFOs, RAMs, flip-flops, SPI, PWM), simulation in Rust, deliberately
  *readable* Verilog output. Effectively frozen: last release 0.46.0
  (July 2023); the author moved to the rewrite.
- **`rhdl`** — the ground-up rewrite. Compiles **actual Rust function
  bodies** — `#[kernel]` functions — through its own compiler frontend (an
  IR called RHIF) to Verilog, with first-class algebraic data types
  (enums/`match` synthesize, which Verilog cannot natively express). The
  kernel subset is "just Rust": `match`, `if`, `let`, type inference,
  generics, early returns — but **no references/pointers and no closures**.
  Status: active on GitHub (1000+ commits) but explicitly a spare-time
  project; the crates.io `rhdl` 0.1.0 (Sept 2023) is a name reservation —
  the real code is unreleased.

### 1b. The wider field, and why it changes the answer

The original survey compared only those two, which framed the choice as
"which crate is the backend". That is the wrong unit. A hardware emitter has
two separable halves, and only one of them is anybody else's problem:

- **Leaf kernels** — the combinational bodies of `map` / `filter` / `join`.
  Here a Rust-source-to-RTL compiler is irreplaceable, and `rhdl` is the only
  game in town.
- **Structural composition** — one module per node, valid/data along edges,
  state registers, and (§5.4) latency balancing. This is *specific to
  wingfoil's graph semantics*. No third party can supply it, and no third
  party's IR makes it materially easier.

Other targets, ranked by how much they would actually change the design:

| | what it is | bearing on this design |
|---|---|---|
| **CIRCT / FIRRTL** | MLIR-based hardware compiler; `firtool` lowers FIRRTL → optimised Verilog | The most credible *emission target*. A real IR with a maintained optimiser and vendor-neutral output — emitting FIRRTL rather than Verilog buys a lowering pipeline we would otherwise own. |
| **Spade** | Rust-inspired HDL (its own language) | Has **pipelining as a first-class construct**, with valid propagation across stages. That is precisely §5.4's problem, solved. Worth reading even if never adopted. |
| **XLS** (Google) | HLS with an automatic scheduler; `proc`s communicating over channels | Its dataflow model is the closest existing thing to a wingfoil graph, and its scheduler does the pipelining automatically. The strongest "buy, don't build" candidate for §5.4. |
| **Veryl** | Rust-flavoured HDL transpiling to SystemVerilog | Active and pragmatic, but a *language* — no closure-splicing story, so it loses the property that motivates this document. |
| **Amaranth** | Python HDL (ex-nMigen) | Excellent simulator and productivity; wrong host language. |

**Which to target — revised.** `rhdl` for the **leaf kernels only**.
**wingfoil owns the structural emission**, targeting SystemVerilog directly
or FIRRTL via CIRCT.

That split is the point: it bounds the pre-release dependency to the half
that has a fallback. If `rhdl` stalls we lose the kernel compiler — an
inconvenience, since `map`/`filter` bodies over a fixed-point dialect are a
small enough language to compile ourselves — rather than the backend. Under
the original framing an `rhdl` stall took the entire emitter with it, which
made a spare-time dependency load-bearing for the project. It is not, and
should never have been.

[rust-hdl](https://github.com/samitbasu/rust-hdl) ·
[rhdl](https://github.com/samitbasu/rhdl) ·
[CIRCT](https://circt.llvm.org/) ·
[Spade](https://spade-lang.org/) ·
[XLS](https://google.github.io/xls/)

## 2. What the software generator actually carries over

### 2a. The ledger

The header of this document claims the hardware backend "reuses the
front-end wholesale". With
[#769](https://github.com/wingfoil-io/wingfoil/pull/769) landed that is
checkable. The honest ledger:

| Generator asset | Carries to hardware? | Notes |
|---|:--:|---|
| `NodeInfo` traversal — index, label, active/passive edges, `passive_mask`, `edges_in_call_order`, `Activation` | ✅ **fully** | This is the real inheritance. A structural HDL emitter walks exactly this, and the active/passive distinction *is* valid-gating vs plain sampling. |
| Refusal machinery — `Ineligible` / `NotEmittable`, all reasons at once, `#[track_caller]` call sites | ✅ **fully** | The FPGA tier is a stricter instance of the same pattern. Refusing "node 12 is not synthesizable" with a wiring line number is infrastructure the hardware tier gets free. |
| Capture detection — `free_vars` + `Probe` | ✅ **fully, and it is the sleeper asset** | The detected capture set is exactly the **runtime-parameter set** (§6c). Nothing else identifies which values a design must expose as CSRs. |
| `check_artifact` staleness guard | ✅ **fully** | More important here, not less: a stale bitstream is worse than a stale binary, and the resynthesis cycle makes "just regenerate" expensive enough that nobody does it casually. |
| Two-pass architecture — run the wiring, walk it, emit, compile | ✅ **fully** | Synthesis replaces pass 2. The shape is identical. |
| `#[op(emit_cfg)]` recording configs on the node | ⚠️ **the recording, not the rendering** | That configs are *reachable at traversal time* is what matters and it transfers. `EmitLiteral` produces Rust source; hardware wants the value, for a `localparam` or a CSR reset. Needs a sibling trait, not a reuse. |
| **Closure quotation (`#[wiring]` / `func!` recorded bodies)** | ❌ **the weakest link — see §2b** | The advertised bridge. It does not splice verbatim, and the reason is structural. |
| `nitro!` as the sole backend | ❌ **no equivalent exists** | See §2c. This is the software generator's best architectural property and hardware does not get it. |
| Anything about **latency** | ❌ **absent entirely** | `NodeInfo` has no notion of pipeline depth, and no way to say "these two edges must be depth-matched". §5.4 is new IR the hardware backend must add. |

### 2b. Why quotation does not splice verbatim

§3 of this document originally asserted that "tier-1 (closed) closures
translate nearly verbatim" into rhdl `#[kernel]` functions. That is wrong,
and the reason is visible in one line of `ops.rs`:

```rust
F: Fn(&A) -> B + 'static,          // map
F: Fn(&A, &B) -> C + 'static,      // join
F: Fn(&mut B, &A) + 'static,       // fold
```

**Every op closure in the catalog takes references.** rhdl's kernel subset
forbids references and pointers outright. So the bodies `#[wiring]` records
today look like

```rust
|n: &u64| (n * size) as f64
```

and the kernel they must become looks like

```rust
#[kernel] fn k0(n: b64) -> ... { ... }
```

— a signature rewrite plus deref elision throughout the body. That is
mechanical for `map`/`filter`/`join` and a compiler pass we would have to
write and test. It is *not* "splice through nearly verbatim", and the
distinction matters because the verbatim claim is what made quotation look
like the keystone.

`fold`'s `Fn(&mut B, &A)` is worse than a rewrite: an `&mut` accumulator is
a *state-update* kernel, a different shape from a combinational one, and
maps to the register-update path rather than to a spliced expression.

**Two consequences.** First, the recorded body is a `String` of normalised
Rust (prettyplease-formatted, per `#[wiring]`'s docs) — which is *fine*,
because it round-trips through rustc in pass 2 either way, exactly as the
software artifact does. The string-versus-tokens question is a non-issue.
Second, the eligibility check cannot be delegated to rhdl's frontend as §3
hoped, because what we hand rhdl is not what the user wrote. We own the
rewrite, so we own its failure modes.

This does not sink the design. It moves quotation from "the bridge that
makes this work" to "one of several inputs, requiring a translation pass" —
and it means the §7 spike must exercise the rewrite rather than assume it.

### 2c. The asymmetry: there is no `nitro!` for hardware

The software generator's strongest architectural decision is that its
artifact is **`nitro!` input**, not runner code. Exactly one place in the
system knows how to turn wiring into a monomorphized runner, so the
generator cannot drift from it, and the artifact stays reviewable plain
Rust.

Hardware has no such component. There is no macro that turns wiring into
RTL, so the emitter must produce *structure* — modules, ports, registers,
valid chains — directly. The parity burden the software generator
sidestepped lands squarely here, which is why §5.2's op-twin problem and
§7's simulation-parity discipline are not optional extras but the core of
the work.

The nearest available substitute is to make the emitted RTL's *shape* so
regular that it is reviewable in the same way the `nitro!` artifact is: one
module per node, one instantiation per node, edges as named wires. Readable
output was `rust-hdl`'s stated design goal and is worth preserving as ours.

## 3. Why the mapping is unusually good

The wired-graph front-end already collects what a structural HDL generator
needs — op identity, topology, active/passive edges, static `ACTIVATION`,
and (with `#[wiring]`) closure source and detected captures. A hardware
emitter walks the same metadata and instantiates one module per node, wiring
valid/data signals along the edges.

The Op pattern is accidentally RTL-shaped — the legacy object graph
(`RefCell` fields, peeked `Rc<dyn Stream>`s) had no such mapping. Another
case where the rearchitecture is the prerequisite, not the casualty:

| wingfoil concept | hardware twin |
|---|---|
| tick | `valid` strobe on a clock edge |
| `Tick::Value` / `Silent` / `Quiet` | valid high / update state with valid low / valid low |
| `Op::State` | registers |
| `In<'a>` / `Out` | input / output ports |
| active upstream edge | valid gating |
| passive upstream edge | plain sampling (no gate) |
| `const ACTIVATION` | static scheduling knowledge |
| `feedback` | registered loop (hardware-native) |
| `ticker` | counter + comparator |
| `delay` | FIFO + comparator |
| `NanoTime` | gone — time *is* the clock; timestamps survive only as data, and in testbenches |
| `RunMode::HistoricalFrom` | simulation testbench |

That last row extends the house parity discipline unchanged: simulate the
emitted design against the interpreted run and assert values *and* order.

Note what the table does **not** contain: `Burst`. See §5.5.

## 4. Worked example

The smallest graph that exercises the machinery: price feed → `delta` →
threshold `filter` — one stateful op, one recorded closure. The RHDL layer
is **rhdl-flavored sketch** (pre-release API; the shape, not exact syntax);
the Verilog is idealized readable output.

Read it as the *easy* case. It is a single path, so §5.4 cannot appear in
it, and the closure is closed over a scalar, so §2b's rewrite is a
one-liner.

### 4a. What the user writes

Fixed-width types (the FPGA dialect — `i16` ticks, not `f64`), recorded
closure, detected capture:

```rust
use wingfoil::prelude::*;

/// Emit price deltas that exceed a threshold. FPGA-eligible:
/// fixed-width types, kernel-subset closure bodies.
#[wiring]
pub fn spike_detector(g: &GraphBuilder, cfg: &Config) -> Stream<i16> {
    let thresh: i16 = cfg.threshold;                 // frozen at generation time
    g.channel::<i16>("price-feed")                   // -> input port in hardware
        .delta()                                     // stateful op: has an RTL twin
        .filter(move |d: &i16| d.abs() > thresh)     // recorded body -> #[kernel]
}
```

```rust
// bin/genfpga.rs — same front-end as the software generator, different backend
wingfoil::codegen::generate_hdl(|g| spike_detector(g, &config), "src/spike.gen.rs")?;
```

The same graph runs interpreted in software, unchanged — that run *is* the
parity oracle for the hardware.

### 4b. What the emitter generates — RHDL (sketch)

Three parts: kernels rewritten from recorded bodies, hand-written op twins
from a library, and the generated structural composition.

```rust
// spike.gen.rs — GENERATED from spike_detector + desk.toml. Do not edit.
use rhdl::prelude::*;

// ---- (a) Kernel rewritten from the recorded closure ---------------------
// from wiring.rs:9: move |d: &i16| d.abs() > thresh, thresh = 25
// NOTE the rewrite (§2b): `&i16` parameter -> by-value `SignedBits<16>`,
// derefs elided. The recorded text is not spliced as-is.
#[kernel]
pub fn filter_k0(d: SignedBits<16>) -> bool {
    let thresh = s16(25);          // capture, frozen as a literal
    d.abs() > thresh
}

// ---- (b) Op twins: from the wingfoil-hdl op library (hand-written ONCE
//      per op, parity-tested by simulation — not generated per graph) ------
// Delta: state = (prev, have_prev); quiet on first sample, like software delta.
#[derive(Digital, Default)]
pub struct DeltaState { prev: SignedBits<16>, have_prev: bool }

#[kernel]
pub fn delta_update(
    s: DeltaState, in_valid: bool, in_data: SignedBits<16>,
) -> (DeltaState, bool, SignedBits<16>) {
    let out = in_data - s.prev;
    let out_valid = in_valid && s.have_prev;
    let next = if in_valid {
        DeltaState { prev: in_data, have_prev: true }
    } else { s };
    (next, out_valid, out)
}

// ---- (c) Generated top level: the graph's topology as a Synchronous circuit
#[derive(Digital)] pub struct In  { pub valid: bool, pub data: SignedBits<16> }
#[derive(Digital)] pub struct Out { pub valid: bool, pub data: SignedBits<16> }

pub struct SpikeDetector;   // node order: [0] channel-in, [1] delta, [2] filter

impl Synchronous for SpikeDetector {
    type I = In; type O = Out; type S = DeltaState;   // union of op states

    #[kernel]
    fn update(s: Self::S, i: Self::I) -> (Self::S, Self::O) {
        // edge: channel -> delta (active: valid gates)
        let (s1, d_valid, d) = delta_update(s, i.valid, i.data);
        // edge: delta -> filter (combinational: kernel gates the valid)
        let out_valid = d_valid && filter_k0(d);
        (s1, Out { valid: out_valid, data: d })
    }
}
```

### 4c. What rhdl emits — Verilog (idealized)

```verilog
// spike_detector.v — generated by rhdl from SpikeDetector
module spike_detector (
    input  wire               clk,
    input  wire               rst,
    input  wire               in_valid,
    input  wire signed [15:0] in_data,
    output reg                out_valid,
    output reg  signed [15:0] out_data
);
    // Op::State -> registers
    reg signed [15:0] prev;
    reg               have_prev;

    // delta_update, combinational
    wire signed [15:0] delta       = in_data - prev;
    wire               delta_valid = in_valid & have_prev;

    // filter_k0, combinational  (|d| > 25)
    wire signed [15:0] abs_d = delta[15] ? -delta : delta;
    wire               keep  = abs_d > 16'sd25;

    always @(posedge clk) begin
        if (rst) begin
            prev      <= 16'sd0;
            have_prev <= 1'b0;
            out_valid <= 1'b0;
            out_data  <= 16'sd0;
        end else begin
            if (in_valid) begin
                prev      <= in_data;
                have_prev <= 1'b1;
            end
            out_valid <= delta_valid & keep;   // one registered pipeline stage
            out_data  <= delta;
        end
    end
endmodule
```

The whole graph became: two registers of state, a subtractor, an
abs/compare, and a valid chain — spike-to-output in one clock. That is the
payoff the software engine can never touch.

### 4d. The parity test (house style, extended)

```rust
#[test]
fn spike_detector_hw_matches_interpreted() {
    // software truth: historical run -> (time, value) trace
    let g = GraphBuilder::new();
    let out = spike_detector(&g, &test_config());
    let expected = run_historical_collect(&g, out, PRICES);

    // hardware: same inputs as valid strobes into the simulator
    let got = rhdl::sim::run_synchronous(SpikeDetector, strobes(PRICES));
    assert_eq!(expected.values(), got.values());   // same spikes, same order
}
```

The example makes two properties concrete: the **filter came through
single-sourced** (the kernel is a mechanical rewrite of the tokens that ran,
so drift is bounded by the rewrite's correctness rather than by a human
re-statement), while **delta needed a twin** (`delta_update` re-states the
semantics; simulation parity is what holds it honest).

**Scale this test up, not out.** Verilator can run millions of cycles fast
enough for CI, and the historical replay already produces exactly the
stimulus and golden output a testbench needs. That is the flagship property
worth engineering for deliberately: *the backtest is the testbench*, where
the usual shop maintains an RTL testbench and a backtest as two artifacts
that drift apart.

## 5. The walls (accepted, documented)

Three were recorded originally. Two more were missing, and 5.4 is the one
most likely to sink the project.

### 5.1 It is a dialect — and the problem is `i128`, not the absence of fixed-point

The original text said hardware "needs fixed-width `Digital` types, floats
mean fixed-point or FP cores". That framed fixed-point as something to
introduce. It already exists: `adapters/market` ships `Px` and `Qty`, exact
and orderable, and the `OrderBook` keys levels by them precisely so that
`f64` never touches a price comparison.

The real finding is their representation. They are **`i128` scaled at 1e9**,
and deliberately so — the module records that an `i64` at nine decimals caps
at ±9.22 × 10⁹, which makes large-notional and very-small-tick venues
unrepresentable. That is the right call for software and the wrong width for
fabric: 128-bit arithmetic costs several DSP slices and pipeline stages per
operation, for range no single instrument needs.

So the dialect work is **narrowing, not introducing**: a `Fixed<I, F>` with
venue-appropriate width (typically 32 or 64 bits at that venue's tick
scale), convertible from `Px`/`Qty` with a range check at generation time.
That is a smaller and much better-defined job than "add fixed-point", and it
has a software payoff on its own — narrower prices are cheaper in cache on
the CPU path too.

`Element` still admits `f64`, `String`, `Vec`, `Burst`; those stay
ineligible. FPGA-eligibility is a **third, stricter tier** of the per-node
eligibility pattern (loud errors listing ineligible nodes with wiring call
sites — §2's inherited refusal machinery). Most existing graphs will not
qualify as-written, by design.

### 5.2 Ops need hardware twins — the drift problem partially returns

Catalog ops' `cycle` bodies are software (allocations, `TinyVec`,
`anyhow`); each FPGA-eligible op needs an RTL twin, with parity held by
simulation tests rather than shared code — the dual-implementation drift the
Op pattern eliminated in software. The mitigating hope, and the flagship
property if it works: for arithmetic/stateful-scalar ops (`map`, `filter`,
`ewma`, `delta`, …) a kernel-subset `cycle` could be genuinely
**single-sourced**. Queue-y ops (`delay`, `window`, merge) will need twins
regardless.

§2c is why this matters more than it first appears: with no `nitro!`
equivalent, twins are where *all* the parity risk concentrates.

### 5.3 Dependency maturity

rhdl is pre-release and one person's spare-time project; rust-hdl is stable
but frozen and lacks the kernel compiler. §1b's split is the mitigation:
bound the dependency to leaf kernels, own the structure.

### 5.4 Pipeline skew — the wall this document was missing

wingfoil's software semantics are that **a cycle is atomic**: every node
sees one consistent time, and all upstream values settle before downstream
fires. In hardware that holds for free only if the whole graph is
combinational between two register stages. At the clock rates this exists
for, it will not be — so the graph gets pipelined, and the moment it does:

- **Fan-in must be latency-balanced.** A `join` whose two upstream paths
  have different pipeline depth combines values from *different logical
  ticks*. The emitter must compute per-node depth and insert shift-register
  delays on the shallower leg. Mechanical — HLS tools do it — but it must be
  designed in, and it interacts with `Tick::Silent`/`Quiet`, where a node
  updates its slot without producing a strobe to delay.
- **`NodeInfo` says nothing about this.** There is no per-node depth and no
  way to express a depth-matching constraint. This is genuinely new IR, and
  it is the one part of the hardware backend the software generator gives no
  head start on (§2's ledger).
- **It is the strongest argument for XLS or Spade** (§1b), both of which
  treat scheduling/pipelining as a first-class concern rather than something
  the emitter open-codes.

Anything that reads `sample`, `join`, `merge` or a fan-out-then-recombine
shape hits this, which is to say every non-trivial trading graph.

### 5.5 `Burst`, and backpressure

**`Burst` has no strobe.** Same-instant values grouped is a software
concept; the hardware realisation is serialisation over N cycles, which
changes latency and breaks the one-clock story that §4 sells. Either the
dialect forbids bursts on the hardware path, or the emitter owns an explicit
serialiser and the latency claim is stated per-burst rather than per-tick.
Deciding this is a prerequisite for a credible number, not a detail.

**Backpressure is absent from software wingfoil and mandatory in hardware.**
The software engine has unbounded queues; a fabric design needs ready/valid
or a proof of no-stall. For the fast path the right answer is usually *no
backpressure at all* — fixed II=1, never drop, sized to absorb line rate —
but `delay` and `window` need sized FIFOs with an **explicit overflow
policy**, which software wingfoil never has to name.

## 6. The host↔card interface

This section is new, and it is deliberately independent of everything
above: it applies whether the gateware is generated by this design,
hand-written, or bought from an IP vendor. `trading-roadmap.md` item 8
already sequences an **FPGA-sink adapter before any HDL work** for exactly
that reason; this is the design content behind it.

### 6a. PCIe discipline

Three costs govern the whole interface (order-of-magnitude, not
measurements):

| operation | cost | on the hot path? |
|---|---|---|
| MMIO write, CPU → card (posted) | ~100–300 ns to land; the store itself is fire-and-forget | **yes** — with write-combining |
| MMIO **read**, CPU → card → CPU (non-posted) | ~500 ns – 1.5 µs | **never** |
| DMA card → host into a polled ring | ~500 ns – 1 µs, high bandwidth | **yes** |

The rule that falls out: **the CPU writes to the card and never reads from
it on a critical path.** A single stray register read in a hot loop costs
more than every optimisation in the graph engine combined.

### 6b. The DMA ring is already a wingfoil source

The card DMAs descriptors into a pinned, huge-page host ring; the CPU spins
on a cache line watching a sequence number or phase bit flip. No doorbell,
no interrupt.

That is `Activation::ALWAYS` + `poll` — the shape
[#758](https://github.com/wingfoil-io/wingfoil/pull/758) made expressible in
the compiled tiers, and which #769's `ingest` example generates. Worth
stating plainly because it reframes a known gap: **wake-driven sources
(`external`/`channel`) being excluded from `compiled()` (#502/#503) is not
on the hardware ingest path at all**, because DMA rings and kernel-bypass
NICs are polled, not woken. The gap is real for other users; it is not a
blocker here.

Two things are missing for a real ring source, both small and both useful
before any FPGA exists:

1. **Burst-aware polling.** `poll` returns `Option<T>`; every real ring API
   returns a batch. One graph cycle per descriptor discards the
   amortisation the API exists to provide. Maps naturally onto `Burst`.
2. **Zero-copy handoff.** `Pooled<T>`
   ([#719](https://github.com/wingfoil-io/wingfoil/pull/719)) is the right
   shape for wrapping a DMA buffer whose drop returns the descriptor —
   already named as the target in `trading-roadmap.md` item 7.

### 6c. Parameter update, and what capture detection is really for

Parameters reach the card over a register map (AXI4-Lite) via MMIO writes.
The failure mode is **torn updates**: a multi-word parameter set written
word-by-word lets the datapath observe a half-updated state. The standard
fix is double-buffered banks plus a single commit register — write the
shadow bank, then arm.

This is where §2's "sleeper asset" pays off. `#[wiring]`'s free-variable
analysis produces, per node, exactly the set of values the closure depends
on. That set is the design's parameter surface. What is missing is the
*distinction* the software generator did not need to make:

- **frozen** — baked into LUTs as a constant. Fastest possible; changing it
  means resynthesis.
- **live** — a control register, writable at runtime through the commit
  protocol above.

`wired-graph-codegen.md` §3 already names this fork ("emitting
capture values as literals *freezes* them… the alternative — promoting
captures to parameters — is a follow-up, not v1"). Hardware turns it from a
follow-up into a requirement, because no trading desk resynthesises to
change a size.

**Recommendation, and it is time-sensitive:** decide the per-capture
frozen/live distinction *now*, while #769's capture machinery is being
written — a marker on the wiring (`param(fee)` or similar) that renders as a
literal in the software artifact today and as a CSR in a hardware artifact
later. It is cheap while the machinery is fresh and expensive once artifacts
are checked in across the tree.

### 6d. Timestamping

Timestamp at the ingress MAC, with a PTP/PPS-disciplined clock. The two-clock
design maps cleanly: the wire timestamp is source-driven data that becomes
`Ctx::time()`; `Ctx::wall_time()` stays host-side telemetry. The host clock
must stay out of the decision path — which is the same rule the engine
already enforces in software ("never branch business logic on `wall_time`").

### 6e. Observability — the decision tap

The hardest operational problem in FPGA trading is knowing what the fabric
actually did. The mitigation is a **decision tap**: a low-priority DMA
stream carrying every trigger evaluation, reconciled against a software
replay of the same inputs.

wingfoil is unusually well placed for this, because the software graph *is*
the same graph — the parity oracle of §4d runs in production, not only in
CI. Design it in from the start; it is also the most saleable property of
the whole idea.

## 7. The de-risk spike (the gate for going further)

Before any emitter exists — a few days' work, entirely by hand.

**The original spike could only succeed.** It proposed `ticker`,
`map`-via-kernel, `ewma`, `delta` — all single-path, all scalar, so it could
not exhibit §5.4 and barely touches §2b. Amended so that it can fail:

1. Write rhdl twins for three or four ops (`ticker`, `map`-via-kernel,
   `ewma`, `delta`).
2. **Do the §2b rewrite by hand** on one recorded body — take the actual
   text `#[wiring]` produces for a `Fn(&A) -> B` closure and turn it into a
   `#[kernel]` fn. Confirm what the rewrite has to do and where it breaks.
3. **Wire a two-input `join` whose upstream paths have deliberately unequal
   pipeline depth**, and balance it by hand the way the emitter would.
4. Simulate under Verilator, and assert parity against the interpreted run
   (§4d shape) on values **and** order.

This answers the load-bearing uncertainties — *is the kernel subset rich
enough for the rewritten bodies?*, *can scalar ops be single-sourced?*, and
now *is latency balancing tractable at emitter scale?* — before committing
to the emitter, the twin catalog, or the dialect design.

Step 3 is the one to run first if time is short. If latency balancing is not
tractable, nothing else matters.

## 8. Sequencing

Strictly behind the software generator's own gates
([`wired-graph-codegen.md`](wired-graph-codegen.md) §7–8),
and interleaved with `trading-roadmap.md` items 7–8:

1. **Now, independent of any FPGA decision** — the §6b gaps (burst-aware
   polling, `Pooled` zero-copy handoff) and the §6c per-capture frozen/live
   marker. All three are useful to software users and are prerequisites
   here.
2. **The host↔card interface (§6) before any HDL** — an FPGA-sink adapter
   and a DMA-ring source. Useful immediately with commercial
   feed-handler/trigger cards, needs no HDL work, and is an ordinary
   wingfoil adapter. This is `trading-roadmap.md` item 8's first half and
   the highest-value, lowest-risk piece of the entire hardware story.
3. **The §7 spike**, amended — runnable any time after the generator lands.
4. **The emitter**, gated on the spike succeeding *and* a named workload (a
   hot path worth an FPGA, with topology/config cadence compatible with
   regenerate-and-resynthesize).

Nothing here blocks or reorders the parity port. Do not reorder gate 2 ahead
of gate 3's findings *into* gate 4 — the interface work is safe to do early
precisely because it is independent of whether the emitter is ever built.
