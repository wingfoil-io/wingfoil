//! The dual-mode thesis, completed by the `nitro!` macro: **one** wiring
//! definition expands to both an interpreted runner and a fully
//! monomorphized compiled runner. The two cannot drift — same tokens, same
//! `Op` semantics — and the compiled one gets the compiler's full
//! optimization across node boundaries.
//!
//! The wiring is a **split/recombine** DAG: a counter is split on parity into
//! two labelled branches which are then merged back into one stream — so this
//! also shows the macro deriving a non-linear graph (a shared apex node and a
//! recombine) for both engines:
//!
//! ```text
//!               count                 (apex — shared node, once/cycle)
//!              /     \
//!       is_odd?      is_even?         (split on parity)
//!            |          |
//!      "{i} is odd" "{i} is even"     (format each branch)
//!              \     /
//!               merge                 (recombine — at most one fires/tick)
//!                 |
//!               acc                   (accumulate — see the note below)
//! ```
//!
//! The tail node is [`accumulate`](wingfoil::fluent::StreamOps::accumulate)
//! rather than a `print`/`for_each` sink for the one reason that justifies it:
//! this example's claim is that the two engines *agree*, and comparing them
//! means holding both runs' whole output as a value. The run is bounded to
//! `CYCLES`. In a graph that emits rather than asserts, the tail is a streaming
//! sink — see [`hello_graph`](../hello_graph/) and
//! [`ema_crossover`](../ema_crossover/).
//!
//! ```sh
//! cargo run -p wingfoil --release --example dual_mode
//! ```
//!
//! # What procedural code you can write inside `nitro!`
//!
//! The macro parses its body as a plain Rust `fn`, but it does not *run* that
//! code — it reads the tokens to derive a **static DAG** at expansion time,
//! then re-emits the whole schedule three ways (`interpreted` / `compiled` /
//! `nested`). Because `compiled()` monomorphizes one local per node (see the
//! generated code at the bottom of this file), the node list must be complete
//! and fixed after parsing. That is the one rule everything below follows:
//! **wiring must be straight-line — the shape of the graph cannot depend on
//! runtime values.** Values and per-element logic can be as procedural as you
//! like; the *topology* cannot.
//!
//! Each top-level statement is sorted into one of three buckets: a **wiring**
//! `let name = <chain>;` (rooted at the builder or an already-bound stream),
//! the **tail** expression naming the outputs, or **passthrough** (anything
//! else, re-emitted verbatim into every engine). The builder and stream names
//! may appear *only* in wiring statements and the tail.
//!
//! ## ✅ Allowed
//!
//! ```rust,ignore
//! // Straight-line wiring: each `let` is one fluent chain.
//! let count  = g.ticker(PERIOD).count();
//! let parity = count.map(|i| i % 2);
//!
//! // Passthrough: ordinary Rust that does NOT mention `g` or a stream —
//! // compute a config, declare a local a closure will capture, etc.
//! let base = 2;
//! let threshold = base * 4;
//! let tagged = count.filter(&parity).map(move |i| i + threshold);
//!
//! // Any control flow you want *inside* an op closure — it is opaque config
//! // the macro passes straight through, never topology.
//! let label = count.map(|i| if i % 2 == 0 { "even" } else { "odd" });
//!
//! // Static repetition sugar with a LITERAL count: `map_n` chains N maps,
//! // `fan` builds N branches and merges them. The DAG stays static.
//! let chained = count.map_n(3, |i| i + 1);
//! let fanned  = count.fan(2, |s| s.map(|i| i * 10));
//! ```
//!
//! ## ❌ Not allowed
//!
//! ```rust,ignore
//! // A helper that does wiring — the macro cannot see the nodes it builds,
//! // so `compiled()` would be blind to them. (Compose by NESTING nitro!s
//! // instead: each nitro! fn is itself reusable wiring via its `wire`.)
//! let x = build_subgraph(g, &count);
//!
//! // A loop that wires — the node count would be a runtime value, which
//! // `compiled()` cannot monomorphize. Use `.map_n`/`.fan` with a literal.
//! for _ in 0..n { count = count.map(|i| i + 1); }
//!
//! // A conditional that picks the TOPOLOGY — one branch or the other would
//! // exist depending on a runtime flag. (Branch *inside* a closure instead,
//! // or build both and select at runtime.)
//! let s = if fast { count.ema(2) } else { count.ema(8) };
//!
//! // A non-literal repeat count — the unrolled DAG must be known statically.
//! let chained = count.map_n(n, |i| i + 1);
//! ```
//!
//! The full expansion of the `nitro!` block below — `wire`, `interpreted`,
//! `compiled`, and `nested` — is reproduced (abridged) in a block comment at
//! the end of this file, so you can see exactly what "straight-line wiring
//! becomes a static schedule" means in emitted code.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode, Tier};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const CYCLES: u32 = 10;
const PERIOD: Duration = Duration::from_millis(10);

// One definition — a valid fluent wiring function whose DAG is a
// split/recombine. The macro parses it to derive the DAG and expands to a
// module: `odds_evens::wire` (this function, verbatim),
// `odds_evens::interpreted()` (built through wire), and
// `odds_evens::compiled()` (the monomorphized schedule derived from the same
// tokens).
//
// `count` is referenced three times, so it is a *shared* apex node: the
// interpreted engine runs it once per cycle and fans the tick out, and the
// compiled engine emits it once and feeds every reader from the same slot.
// `merge` is the recombine — since a number is either odd or even, at most
// one branch fires on any tick.
wingfoil::nitro! {
    fn odds_evens(g: &GraphBuilder) -> Stream<Vec<String>> {
        let count = g.ticker(PERIOD).count();
        let is_even = count.map(|i| i.is_multiple_of(2));
        let is_odd = is_even.map(|b| !b);
        let odd_str = count.filter(&is_odd).map(|i| format!("{i} is odd"));
        let even_str = count.filter(&is_even).map(|i| format!("{i} is even"));
        let acc = odd_str.merge(&even_str).accumulate();
        acc
    }
}

fn main() {
    let run_for = RunFor::Cycles(CYCLES);

    // Interpreted: build the graph, run it, read the accumulated labels.
    let (mut runner, acc) = odds_evens::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(acc);

    // Compiled: the same tokens, monomorphized into a standalone runner.
    let (compiled,) = odds_evens::compiled(HISTORICAL, run_for).unwrap();

    assert_eq!(interpreted, compiled, "engines must agree");

    // The two entry points above are shaped differently — a runner plus
    // handles to read *after* running, versus run bounds in and values out —
    // which is what would otherwise stop you swapping engines behind a flag.
    // `run` reconciles them: same signature, same outputs, tier as an
    // argument. `Tier::default()` resolves from `WINGFOIL_TIER` if it is set
    // and otherwise from the build profile (interpreted in debug, compiled in
    // release), so the usual workflow — develop interpreted, deploy compiled —
    // needs no call-site change at all.
    let tier = Tier::default();
    let (via_run,) = odds_evens::run(tier, HISTORICAL, run_for).unwrap();
    assert_eq!(via_run, interpreted, "`run` must agree with both engines");

    for line in &interpreted {
        println!("{line}");
    }
    println!(
        "\n{} labels over {CYCLES} cycles — interpreted and compiled engines agree.",
        interpreted.len()
    );
    println!("`run(Tier::default(), ..)` resolved to the {tier} tier and matched them.");
}

// ===========================================================================
// Generated code (abridged)
// ===========================================================================
//
// `cargo expand -p wingfoil --example dual_mode` prints the full ~1.5k
// lines. Below is a faithful, abridged rendering of what the `nitro!` block
// above expands into: a module `odds_evens` with four entry points. The DAG is
// 10 nodes, indexed in wiring order:
//
//   0 ticker(wf_anon_1)  1 count   2 is_even  3 is_odd  4 filter(wf_anon_2)
//   5 odd_str  6 filter(wf_anon_3)  7 even_str  8 merge(wf_anon_4)  9 acc
//
// Intermediate (unnamed) nodes get `wf_anon_N` slots; user `let` names are
// kept verbatim. Every op — built-in or user — is dispatched through
// naming-convention forwarders `__wf_op_<method>_{cycle,start,stop,teardown,
// seed_state,seed_value}` (+ `_owned` variants for literal-closure configs),
// plus the per-op consts `__WF_OP_<METHOD>_{ACTIVATION,PASSIVE}`. The macro
// never names an op type — rustc's inference resolves each from the argument
// types at the call site, and the consts fold into the tick gates after
// monomorphization.
//
// ---------------------------------------------------------------------------
// mod odds_evens {
//
//     // (a) `wire` — the wiring fn, verbatim. This is the ONLY human-written
//     //     form; the other three are derived from these same tokens, so they
//     //     cannot drift. A nitro! fn with inputs is reusable by calling
//     //     `wire(g)` from another graph (composition = nesting).
//     pub fn wire(g: &GraphBuilder) -> Stream<Vec<String>> {
//         let count = g.ticker(PERIOD).count();
//         let is_even = count.map(|i| i.is_multiple_of(2));
//         let is_odd = is_even.map(|b| !b);
//         let odd_str = count.filter(&is_odd).map(|i| format!("{i} is odd"));
//         let even_str = count.filter(&is_even).map(|i| format!("{i} is even"));
//         let acc = odd_str.merge(&even_str).accumulate();
//         acc
//     }
//
//     // (b) `interpreted` — build the graph through `wire` against a runtime
//     //     builder, hand back the runner + a typed handle per output.
//     pub fn interpreted() -> (interp::Runner, interp::Handle<Vec<String>>) {
//         let __g = GraphBuilder::new();
//         let acc = wire(&__g);
//         let __runner = __g.build();
//         (__runner, acc.handle())
//     }
//
//     // (c) `compiled` — the same DAG, fully monomorphized: one local per
//     //     node, tick propagation as bools, every Op::cycle (closures
//     //     included) visible to the optimizer. No heap, no dyn, no table.
//     pub fn compiled(run_mode, run_for) -> anyhow::Result<(Vec<String>,)> {
//         let mut __k = codegen::Kernel::new(run_mode, run_for);
//
//         // One (cfg?/state/value) slot per node, seeded via forwarders.
//         let mut __cfg_wf_anon_1   = PERIOD;                              // ticker cfg
//         let mut __state_wf_anon_1 = __wf_op_ticker_seed_state(&__cfg_wf_anon_1);
//         let mut __v_wf_anon_1     = __wf_op_ticker_seed_value(&__cfg_wf_anon_1);
//         let mut __state_count = __wf_op_count_seed_state(&());
//         let mut __v_count     = __wf_op_count_seed_value(&());
//         // … is_even, is_odd, wf_anon_2, odd_str, wf_anon_3, even_str,
//         //   wf_anon_4, acc — same shape …
//
//         let mut __first_err = None;
//         let __run = (|| -> anyhow::Result<()> {
//             // start(), nodes 0..=9 in wiring order (no-op starts inline away)
//             { let mut __ctx = Ctx::new(&mut __k, 0);
//               __wf_op_ticker_start(&mut __cfg_wf_anon_1, &mut __state_wf_anon_1, &mut __ctx)?; }
//             // … starts for nodes 1..=9 …
//
//             // The cycle loop IS the whole schedule, straight-line.
//             let mut __dirty = [false; 10];
//             while __k.begin_cycle(&mut __dirty) {
//                 // node 0 — ticker (a source: fires only when scheduled).
//                 let __t_wf_anon_1 = (__WF_OP_TICKER_ACTIVATION.always
//                     || (__WF_OP_TICKER_ACTIVATION.callback_activated() && __dirty[0]))
//                     && { let mut __ctx = Ctx::new(&mut __k, 0);
//                          match __wf_op_ticker_cycle(&mut __cfg_wf_anon_1,
//                                  &mut __state_wf_anon_1, (), &mut __ctx)? {
//                              Tick::Value(v)  => { __v_wf_anon_1 = v; true }
//                              Tick::Silent(v) => { __v_wf_anon_1 = v; false }
//                              Tick::Quiet     => false,
//                          } };
//
//                 // node 1 — count. Its gate: "an active input ticked" (built
//                 // from the upstream tick flag masked by PASSIVE), or the op
//                 // is `always`, or it was scheduled (`callback_activated` +
//                 // dirty). Inputs arrive as (&value, ticked) pairs.
//                 let __t_count = ((((__WF_OP_COUNT_PASSIVE >> 0) & 1) == 0 && __t_wf_anon_1)
//                     || __WF_OP_COUNT_ACTIVATION.always
//                     || (__WF_OP_COUNT_ACTIVATION.callback_activated() && __dirty[1]))
//                     && { let mut __ctx = Ctx::new(&mut __k, 1);
//                          match __wf_op_count_cycle(&mut (), &mut __state_count,
//                                  ((&__v_wf_anon_1, __t_wf_anon_1),), &mut __ctx)? {
//                              Tick::Value(v)  => { __v_count = v; true }
//                              Tick::Silent(v) => { __v_count = v; false }
//                              Tick::Quiet     => false,
//                          } };
//
//                 // nodes 2..=9 — identical shape. A two-input op (filter,
//                 // merge) gets one (&value, ticked) pair per edge in call
//                 // order, e.g. the odd-branch filter (node 4):
//                 //   __wf_op_filter_cycle(&mut (), &mut __state_wf_anon_2,
//                 //       ((&__v_count, __t_count), (&__v_is_odd, __t_is_odd)),
//                 //       &mut __ctx)?
//                 // …
//
//                 __k.end_cycle(&mut __dirty);
//             }
//             Ok(())
//         })();
//         if let Err(e) = __run { __first_err = Some(e); }
//
//         // stop() then teardown() for every node, first error wins (abridged).
//         // …
//
//         match __first_err {
//             Some(e) => Err(e),
//             None    => Ok((__v_acc,)),   // the tail binding(s), by value
//         }
//     }
//
//     // (d) `nested` — the whole graph mounted as ONE compiled node inside an
//     //     interpreted graph. The outer engine pays one dyn call per
//     //     activation for the entire sub-graph; inside, it is the same
//     //     straight-line schedule as `compiled`. Inner schedules (the ticker)
//     //     run through a private TimeQueue; only the earliest is forwarded to
//     //     the outer kernel, and only the tail's tick is emitted downstream.
//     pub fn nested(__g: &GraphBuilder) -> Stream<Vec<String>> {
//         // … same per-node slots as compiled(), captured by the closure …
//         let mut __q = TimeQueue::<usize>::new();
//         let mut __dirty = [false; 10];
//         __g.__composite(__active, __passive, /* any inner op callback-activated? */ …,
//             move |__ctx, __phase| {
//                 match __phase {
//                     CompositePhase::Start    => { /* start all; schedule earliest */ }
//                     CompositePhase::Stop     => { /* stop all */ }
//                     CompositePhase::Teardown => { /* teardown all */ }
//                     CompositePhase::Cycle    => {}
//                 }
//                 // drain the private queue into __dirty, then run the SAME
//                 // node-0..=9 schedule as compiled() (via Ctx::nested(.., ix)),
//                 // re-arm the queue, and forward only the tail:
//                 Ok(if __t_acc { Tick::Value(__v_acc.clone()) } else { Tick::Quiet })
//             })
//     }
// }
// ---------------------------------------------------------------------------
