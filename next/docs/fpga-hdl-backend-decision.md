# FPGA/Verilog as a third backend — the RustHDL/RHDL emission design

**Status: exploratory design, not scheduled.** This records the reasoning
and the de-risk plan; nothing is committed to build. It is **gated behind**
the software generator of
[`wired-graph-codegen-decision.md`](wired-graph-codegen-decision.md) — read
that first: this document reuses its front-end (wired-graph traversal +
`func!` quotation metadata) wholesale and adds a hardware emission backend
behind it.

**Question.** Could the wired-graph front-end also emit hardware — generate
RustHDL/RHDL code that in turn generates Verilog, so a graph's hot path
(feed handler, signal pipeline, tick-to-trade) runs on an FPGA while the
same wiring backtests in software?

**Answer.** Architecturally yes, and the pieces line up strikingly well —
the Op pattern is accidentally RTL-shaped, and `func!` quotation is exactly
the bridge RHDL's no-closures kernel subset needs. But it is a much bigger
lift than the software generator (a restricted type dialect, per-op RTL
twins with simulation-based parity, clock/valid semantics design) stacked
on a pre-release one-person dependency. Position it as **backend #3** of
the multi-front-end design, de-risked by a small hand-written spike before
any emitter exists.

---

## 1. The target crates (surveyed 2026-07)

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

**Which to target:** `rhdl`, because its kernel model lets quoted closure
bodies splice through nearly verbatim (§3) and its frontend doubles as the
eligibility checker — but as a *prototyping vehicle*, not a load-bearing
production dependency, until it has releases. The shipped interface is the
**emitted Verilog**, which is toolchain-portable: if `rhdl` stalls, the
emitter's front half (traversal, eligibility, kernel extraction) survives
and could retarget Chisel/Amaranth/raw Verilog templates.
[rust-hdl](https://github.com/samitbasu/rust-hdl) ·
[rhdl](https://github.com/samitbasu/rhdl)

## 2. Why the mapping is unusually good

The wired-graph front-end already collects what a structural HDL generator
needs — op identity, topology, active/passive edges, static `ACTIVATION`,
and (with `func!`) closure source and explicit captures. A hardware emitter
walks the same metadata and instantiates one module per node, wiring
valid/data signals along the edges.

The Op pattern is accidentally RTL-shaped — the legacy object graph
(`RefCell` fields, peeked `Rc<dyn Stream>`s) had no such mapping. Another
case where the rearchitecture is the prerequisite, not the casualty:

| wingfoil-next concept | hardware twin |
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

**`func!` is exactly the bridge RHDL needs.** rhdl kernels forbid closures
— but the quotation design already decomposes a closure into *body tokens +
explicit capture values*. Emit the body as a `#[kernel]` fn and the
captures as arguments or consts, and rhdl's own compiler frontend becomes
the validator: a quoted body outside the synthesizable subset fails
loudly, at generation time, with the wiring call site attached — not at
synthesis. Tier-1 (closed) closures translate nearly verbatim.

## 3. Worked example

The smallest graph that exercises the machinery: price feed → `delta` →
threshold `filter` — one stateful op, one quoted closure. The RHDL layer is
**rhdl-flavored sketch** (pre-release API; the shape, not exact syntax);
the Verilog is idealized readable output.

### 3a. What the user writes

Fixed-width types (the FPGA dialect — `i16` ticks, not `f64`), quoted
closure, explicit capture:

```rust
use wingfoil_next::prelude::*;
use wingfoil_next::func;

/// Emit price deltas that exceed a threshold. FPGA-eligible:
/// fixed-width types, kernel-subset closure bodies.
pub fn spike_detector(g: &GraphBuilder, cfg: &Config) -> Stream<i16> {
    let thresh: i16 = cfg.threshold;                     // frozen at generation time
    g.channel::<i16>("price-feed")                       // -> input port in hardware
        .delta()                                         // stateful op: has an RTL twin
        .filter(func!([thresh] |d| d.abs() > thresh))    // quoted body -> #[kernel]
}
```

```rust
// bin/genfpga.rs — same front-end as the software generator, different backend
wingfoil_next::codegen::generate_hdl(|g| spike_detector(g, &config), "src/spike.gen.rs")?;
```

The same graph runs interpreted in software, unchanged — that run *is* the
parity oracle for the hardware.

### 3b. What the emitter generates — RHDL (sketch)

Three parts: kernels spliced from `func!` bodies, hand-written op twins
from a library, and the generated structural composition.

```rust
// spike.gen.rs — GENERATED from spike_detector + desk.toml. Do not edit.
use rhdl::prelude::*;

// ---- (a) Kernel spliced from the quoted closure -------------------------
// from wiring.rs:9: func!([thresh] |d| d.abs() > thresh), thresh = 25
// rhdl's compiler frontend type-checks this against its synthesizable subset.
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

### 3c. What rhdl emits — Verilog (idealized)

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

### 3d. The parity test (house style, extended)

```rust
#[test]
fn spike_detector_hw_matches_interpreted() {
    // software truth: historical run -> (time, value) trace
    let g = GraphBuilder::new();
    let out = spike_detector(&g, &test_config());
    let expected = run_historical_collect(&g, out, PRICES);

    // hardware: same inputs as valid strobes into rhdl's simulator
    let got = rhdl::sim::run_synchronous(SpikeDetector, strobes(PRICES));
    assert_eq!(expected.values(), got.values());   // same spikes, same order
}
```

The example makes two properties of the design concrete: the **filter came
through single-sourced** (the quoted body is the same tokens in software
and in the kernel — drift-impossible, exactly as in the software
generator), while **delta needed a twin** (`delta_update` re-states the
semantics; simulation parity is what holds it honest).

## 4. The three walls (accepted, documented)

1. **It's a dialect, not the full language.** `Element` admits `f64`,
   `String`, `Vec`, `Burst`; hardware needs fixed-width `Digital` types,
   floats mean fixed-point or FP cores, and bursts/unbounded queues need
   explicit sized FIFOs and ready/valid backpressure that software
   wingfoil never thinks about. FPGA-eligibility is a *third, stricter
   tier* of the per-node eligibility pattern (loud errors listing
   ineligible nodes with wiring call sites). Most existing graphs will not
   qualify as-written, by design.
2. **Ops need hardware twins — the drift problem partially returns.**
   Catalog ops' `cycle` bodies are software (allocations, `TinyVec`,
   `anyhow`); each FPGA-eligible op needs an RTL twin, with parity held by
   simulation tests rather than shared code — the dual-implementation
   drift the Op pattern eliminated in software. The mitigating hope, and
   the flagship property if it works: for arithmetic/stateful-scalar ops
   (`map`, `filter`, `ewma`, `delta`, …) a kernel-subset `cycle` could be
   genuinely **single-sourced** — one body compiled by rustc for software
   and by rhdl for hardware. Queue-y ops (`delay`, `window`, merge) will
   need twins regardless.
3. **Dependency maturity.** rhdl is pre-release and one person's
   spare-time project; rust-hdl is stable but frozen and lacks the kernel
   compiler that makes the splicing story clean. Mitigation as in §1:
   emitted Verilog is the shipped interface; rhdl is the prototyping
   vehicle.

## 5. The de-risk spike (the gate for going further)

Before any emitter exists — a few days' work, entirely by hand:

1. Write rhdl twins for three or four ops (`ticker`, `map`-via-kernel,
   `ewma`, `delta`).
2. Wire one toy graph by hand, the way the emitter would.
3. Simulate, and assert parity against the interpreted run (§3d shape).

This answers the two load-bearing uncertainties — *is the kernel subset
rich enough for quoted bodies?* and *can scalar ops be single-sourced?* —
before committing to the emitter, the twin catalog, or the dialect design.

## 6. Sequencing

Strictly behind the software generator's own gates
([`wired-graph-codegen-decision.md`](wired-graph-codegen-decision.md) §7–8):
the `func!`/`OpFn`/metadata layer is shared infrastructure and lands first
for software reasons; the §5 spike can run any time after that as a
standalone experiment; the emitter itself is gated on the spike succeeding
*and* a named workload (a hot path worth an FPGA, with topology/config
cadence compatible with regenerate-and-resynthesize). Nothing here blocks
or reorders the parity port.
