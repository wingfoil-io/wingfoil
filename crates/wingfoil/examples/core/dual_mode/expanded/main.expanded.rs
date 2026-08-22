#![feature(prelude_import)]
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
//!               log tap               (emit as the run progresses)
//! ```
//!
//! The tail emits as the run progresses — a closure sink over the `log` crate,
//! per the house rule that examples stream their output rather than collect it.
//! Two things worth knowing about that tap:
//!
//! - It is spelled `with_time().for_each(|(t, s)| { log::info!(..); Ok(()) })`
//!   rather than `.logged(..)`, because `logged` is deliberately
//!   **fluent-only**: its op `Cfg` is `(String, log::Level)` while the fluent
//!   method takes `&str`, and `nitro!` uses the call-site argument types as
//!   the `Cfg` verbatim — the same tokens cannot satisfy both (see
//!   `tests/op_completeness.rs`, category 2b). A closure sink is the same tap
//!   and works identically on every tier, because closures are opaque config.
//! - This example **runs** the graph; it does not tie the engines out against
//!   each other. That assertion needs both runs' whole output held as a value,
//!   which is `accumulate()`'s job and a test's place —
//!   [`tests/macro_parity.rs`](../../../tests/macro_parity.rs) pins this same
//!   diamond's interpreted and compiled outputs equal (with an `accumulate`
//!   tail in place of the log tap, which is what lets it hold the output).
//!
//! `log` output is rendered by `env_logger`, so run with `RUST_LOG=info`:
//!
//! ```sh
//! RUST_LOG=info cargo run -p wingfoil --release --example dual_mode
//! ```
//!
//! # What procedural code you can write inside `nitro!`
//!
//! The macro parses its body as a plain Rust `fn`, but it does not *run* that
//! code — it reads the tokens to derive a **static DAG** at expansion time,
//! then re-emits the whole schedule three ways (`interpreted` / `compiled` /
//! `nested`). Because `compiled()` monomorphizes one local per node (see the
//! committed expansion in [`expanded/`](expanded/)), the node list must be
//! complete and fixed after parsing. That is the one rule everything below
//! follows: **wiring must be straight-line — the shape of the graph cannot
//! depend on runtime values.** Values and per-element logic can be as
//! procedural as you like; the *topology* cannot.
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
//! `compiled`, `run`, and `nested` — is committed **verbatim** in
//! [`expanded/main.expanded.rs`](expanded/main.expanded.rs), so you can read
//! exactly what "straight-line wiring becomes a static schedule" means in
//! emitted code. `expanded/README.md` has the command that regenerates it.
extern crate std;
#[prelude_import]
use std::prelude::rust_2024::*;

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode, Tier};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const CYCLES: u32 = 10;
const PERIOD: Duration = Duration::from_millis(10);

// One definition — a valid fluent wiring function whose DAG is a
// split/recombine. The macro parses it to derive the DAG and expands to a
// module: `odds_evens::wire` (this function, verbatim),
// `odds_evens::interpreted()` (built through wire), `odds_evens::compiled()`
// (the monomorphized schedule derived from the same tokens), and
// `odds_evens::run(tier, ..)` (either engine behind one signature).
//
// `count` is referenced three times, so it is a *shared* apex node: the
// interpreted engine runs it once per cycle and fans the tick out, and the
// compiled engine emits it once and feeds every reader from the same slot.
// `merge` is the recombine — since a number is either odd or even, at most
// one branch fires on any tick. `sink` is a side-effect-only node (nothing
// reads it, it is not in the tail): the log tap that streams each label out
// through the `log` crate, stamped with the engine time it ticked at.
mod odds_evens {
    #![allow(clippy :: redundant_closure_call, clippy :: let_and_return)]
    use super::*;
    use ::wingfoil::fluent::*;
    #[allow(unused_imports)]
    use ::wingfoil::ops::*;
    #[allow(unused_variables)]
    pub fn wire(g: &GraphBuilder) -> Stream<String> {
        let count = g.ticker(PERIOD).count();
        let is_even = count.map(|i| i.is_multiple_of(2));
        let is_odd = is_even.map(|b| !b);
        let odd_str =
            count.filter(&is_odd).map(|i|

                    // `Tier::default()` resolves from `WINGFOIL_TIER` if it is set and
                    // otherwise from the build profile (interpreted in debug, compiled in
                    // release), so the usual workflow — develop interpreted, deploy compiled —
                    // needs no call-site change at all. The log lines are identical either
                    // way; that the tiers *agree* is pinned by `tests/macro_parity.rs`.

                    ::alloc::__export::must_use({
                            ::alloc::fmt::format(format_args!("{0} is odd", i))
                        }));
        let even_str =
            count.filter(&is_even).map(|i|
                    ::alloc::__export::must_use({
                            ::alloc::fmt::format(format_args!("{0} is even", i))
                        }));
        let labelled = odd_str.merge(&even_str);
        let sink =
            labelled.with_time().for_each(|(t, s): &(NanoTime, String)|
                    {
                        {
                            {
                                {
                                    let lvl = ::log::Level::Info;
                                    if lvl <= ::log::STATIC_MAX_LEVEL &&
                                            lvl <= ::log::max_level() {
                                        ::log::__private_api::log({
                                                ::log::__private_api::GlobalLogger
                                            }, format_args!("{0} odds/evens {1:?}", t.pretty(), s), lvl,
                                            &("wingfoil", "dual_mode::odds_evens",
                                                    ::log::__private_api::loc()), ());
                                    }
                                }
                            }
                        };
                        Ok(())
                    });
        labelled
    }
    #[doc = r" The graph built through [`wire`] — the function as written."]
    #[doc = r" Returns the runner plus a typed handle per output stream."]
    pub fn interpreted()
        -> (::wingfoil::interp::Runner, ::wingfoil::interp::Handle<String>) {
        let __g = ::wingfoil::fluent::GraphBuilder::new();
        let labelled = wire(&__g);
        let __runner = __g.build();
        (__runner, labelled.handle())
    }
    #[doc =
    r" The same graph, fully monomorphized: node state in locals, tick"]
    #[doc = r" propagation as bools, every `Op::cycle` (closures included)"]
    #[doc = r" visible to the compiler. Derived from the same tokens as"]
    #[doc = r" `interpreted`, so the two cannot drift."]
    #[allow(unused_mut, unused_variables, unused_assignments)]
    pub fn compiled(run_mode: ::wingfoil::RunMode,
        run_for: ::wingfoil::RunFor)
        -> ::wingfoil::anyhow::Result<(String,)> {
        if (false || __WF_OP_TICKER_ACTIVATION.always ||
                                                            __WF_OP_COUNT_ACTIVATION.always ||
                                                        __WF_OP_MAP_ACTIVATION.always ||
                                                    __WF_OP_MAP_ACTIVATION.always ||
                                                __WF_OP_FILTER_ACTIVATION.always ||
                                            __WF_OP_MAP_ACTIVATION.always ||
                                        __WF_OP_FILTER_ACTIVATION.always ||
                                    __WF_OP_MAP_ACTIVATION.always ||
                                __WF_OP_MERGE_ACTIVATION.always ||
                            __WF_OP_WITH_TIME_ACTIVATION.always ||
                        __WF_OP_FOR_EACH_ACTIVATION.always) &&
                !#[allow(non_exhaustive_omitted_patterns)] match run_mode {
                        ::wingfoil::RunMode::RealTime => true,
                        _ => false,
                    } {
            return ::anyhow::__private::Err({
                        let error =
                            ::anyhow::__private::format_err(format_args!("graphs with poll sources require RunMode::RealTime — there is nothing to busy-poll in a deterministic historical replay"));
                        error
                    });
        }
        let mut __k = ::wingfoil::Kernel::new(run_mode, run_for);
        if (false || __WF_OP_TICKER_ACTIVATION.always ||
                                                        __WF_OP_COUNT_ACTIVATION.always ||
                                                    __WF_OP_MAP_ACTIVATION.always ||
                                                __WF_OP_MAP_ACTIVATION.always ||
                                            __WF_OP_FILTER_ACTIVATION.always ||
                                        __WF_OP_MAP_ACTIVATION.always ||
                                    __WF_OP_FILTER_ACTIVATION.always ||
                                __WF_OP_MAP_ACTIVATION.always ||
                            __WF_OP_MERGE_ACTIVATION.always ||
                        __WF_OP_WITH_TIME_ACTIVATION.always ||
                    __WF_OP_FOR_EACH_ACTIVATION.always) {
            __k.set_spin(true);
        }
        let mut __cfg_wf_anon_1 = PERIOD;
        let mut __state_wf_anon_1 =
            __wf_op_ticker_seed_state(&__cfg_wf_anon_1);
        let mut __v_wf_anon_1 = __wf_op_ticker_seed_value(&__cfg_wf_anon_1);
        let mut __state_count = __wf_op_count_seed_state(&());
        let mut __v_count = __wf_op_count_seed_value(&());
        let mut __state_is_even = __wf_op_map_seed_state(&());
        let mut __v_is_even = __wf_op_map_seed_value(&());
        let mut __state_is_odd = __wf_op_map_seed_state(&());
        let mut __v_is_odd = __wf_op_map_seed_value(&());
        let mut __state_wf_anon_2 = __wf_op_filter_seed_state(&());
        let mut __v_wf_anon_2 = __wf_op_filter_seed_value(&());
        let mut __state_odd_str = __wf_op_map_seed_state(&());
        let mut __v_odd_str = __wf_op_map_seed_value(&());
        let mut __state_wf_anon_3 = __wf_op_filter_seed_state(&());
        let mut __v_wf_anon_3 = __wf_op_filter_seed_value(&());
        let mut __state_even_str = __wf_op_map_seed_state(&());
        let mut __v_even_str = __wf_op_map_seed_value(&());
        let mut __state_labelled = __wf_op_merge_seed_state(&());
        let mut __v_labelled = __wf_op_merge_seed_value(&());
        let mut __state_wf_anon_4 = __wf_op_with_time_seed_state(&());
        let mut __v_wf_anon_4 = __wf_op_with_time_seed_value(&());
        let mut __state_sink = __wf_op_for_each_seed_state(&());
        let mut __v_sink = __wf_op_for_each_seed_value(&());
        let mut __first_err:
                ::core::option::Option<::wingfoil::anyhow::Error> =
            ::core::option::Option::None;
        let __run =
            (|| -> ::wingfoil::anyhow::Result<()>
                        {
                            {
                                let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 0usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_ticker_start(&mut __cfg_wf_anon_1,
                                            &mut __state_wf_anon_1, &mut __ctx),
                                        "node 0 (wf_anon_1: ticker) start")?;
                            }
                            {
                                let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 1usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_count_start(&mut (),
                                            &mut __state_count, &mut __ctx),
                                        "node 1 (count: count) start")?;
                            }
                            {
                                let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 2usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_map_start_owned(&mut (),
                                            &mut __state_is_even, &mut __ctx),
                                        "node 2 (is_even: map) start")?;
                            }
                            {
                                let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 3usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_map_start_owned(&mut (),
                                            &mut __state_is_odd, &mut __ctx),
                                        "node 3 (is_odd: map) start")?;
                            }
                            {
                                let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 4usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_filter_start(&mut (),
                                            &mut __state_wf_anon_2, &mut __ctx),
                                        "node 4 (wf_anon_2: filter) start")?;
                            }
                            {
                                let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 5usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_map_start_owned(&mut (),
                                            &mut __state_odd_str, &mut __ctx),
                                        "node 5 (odd_str: map) start")?;
                            }
                            {
                                let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 6usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_filter_start(&mut (),
                                            &mut __state_wf_anon_3, &mut __ctx),
                                        "node 6 (wf_anon_3: filter) start")?;
                            }
                            {
                                let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 7usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_map_start_owned(&mut (),
                                            &mut __state_even_str, &mut __ctx),
                                        "node 7 (even_str: map) start")?;
                            }
                            {
                                let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 8usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_merge_start(&mut (),
                                            &mut __state_labelled, &mut __ctx),
                                        "node 8 (labelled: merge) start")?;
                            }
                            {
                                let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 9usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_with_time_start(&mut (),
                                            &mut __state_wf_anon_4, &mut __ctx),
                                        "node 9 (wf_anon_4: with_time) start")?;
                            }
                            {
                                let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 10usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_for_each_start_owned(&mut (),
                                            &mut __state_sink, &mut __ctx),
                                        "node 10 (sink: for_each) start")?;
                            }
                            let mut __dirty = [false; 11usize];
                            while __k.begin_cycle(&mut __dirty) {
                                let __t_wf_anon_1 =
                                    (__WF_OP_TICKER_ACTIVATION.always ||
                                                (__WF_OP_TICKER_ACTIVATION.callback_activated() &&
                                                        __dirty[0usize])) &&
                                        {
                                            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 0usize);
                                            match ::wingfoil::anyhow::Context::context(__wf_op_ticker_cycle(&mut __cfg_wf_anon_1,
                                                            &mut __state_wf_anon_1, (), &mut __ctx),
                                                        "node 0 (wf_anon_1: ticker) cycle")? {
                                                ::wingfoil::op::Tick::Value(__val) => {
                                                    __v_wf_anon_1 = __val;
                                                    true
                                                }
                                                ::wingfoil::op::Tick::Silent(__val) => {
                                                    __v_wf_anon_1 = __val;
                                                    false
                                                }
                                                ::wingfoil::op::Tick::Quiet => false,
                                            }
                                        };
                                let _ = __t_wf_anon_1;
                                let __t_count =
                                    ((((__WF_OP_COUNT_PASSIVE >> 0u32) & 1) == 0 &&
                                                            __t_wf_anon_1) || __WF_OP_COUNT_ACTIVATION.always ||
                                                (__WF_OP_COUNT_ACTIVATION.callback_activated() &&
                                                        __dirty[1usize])) &&
                                        {
                                            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 1usize);
                                            match ::wingfoil::anyhow::Context::context(__wf_op_count_cycle(&mut (),
                                                            &mut __state_count, ((&__v_wf_anon_1, __t_wf_anon_1),),
                                                            &mut __ctx), "node 1 (count: count) cycle")? {
                                                ::wingfoil::op::Tick::Value(__val) => {
                                                    __v_count = __val;
                                                    true
                                                }
                                                ::wingfoil::op::Tick::Silent(__val) => {
                                                    __v_count = __val;
                                                    false
                                                }
                                                ::wingfoil::op::Tick::Quiet => false,
                                            }
                                        };
                                let _ = __t_count;
                                let __t_is_even =
                                    ((((__WF_OP_MAP_PASSIVE >> 0u32) & 1) == 0 && __t_count) ||
                                                    __WF_OP_MAP_ACTIVATION.always ||
                                                (__WF_OP_MAP_ACTIVATION.callback_activated() &&
                                                        __dirty[2usize])) &&
                                        {
                                            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 2usize);
                                            match ::wingfoil::anyhow::Context::context(__wf_op_map_cycle_owned(|i|
                                                                i.is_multiple_of(2), &mut (), &mut __state_is_even,
                                                            ((&__v_count, __t_count),), &mut __ctx),
                                                        "node 2 (is_even: map) cycle")? {
                                                ::wingfoil::op::Tick::Value(__val) => {
                                                    __v_is_even = __val;
                                                    true
                                                }
                                                ::wingfoil::op::Tick::Silent(__val) => {
                                                    __v_is_even = __val;
                                                    false
                                                }
                                                ::wingfoil::op::Tick::Quiet => false,
                                            }
                                        };
                                let _ = __t_is_even;
                                let __t_is_odd =
                                    ((((__WF_OP_MAP_PASSIVE >> 0u32) & 1) == 0 && __t_is_even)
                                                    || __WF_OP_MAP_ACTIVATION.always ||
                                                (__WF_OP_MAP_ACTIVATION.callback_activated() &&
                                                        __dirty[3usize])) &&
                                        {
                                            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 3usize);
                                            match ::wingfoil::anyhow::Context::context(__wf_op_map_cycle_owned(|b|
                                                                !b, &mut (), &mut __state_is_odd,
                                                            ((&__v_is_even, __t_is_even),), &mut __ctx),
                                                        "node 3 (is_odd: map) cycle")? {
                                                ::wingfoil::op::Tick::Value(__val) => {
                                                    __v_is_odd = __val;
                                                    true
                                                }
                                                ::wingfoil::op::Tick::Silent(__val) => {
                                                    __v_is_odd = __val;
                                                    false
                                                }
                                                ::wingfoil::op::Tick::Quiet => false,
                                            }
                                        };
                                let _ = __t_is_odd;
                                let __t_wf_anon_2 =
                                    ((((__WF_OP_FILTER_PASSIVE >> 0u32) & 1) == 0 && __t_count)
                                                        ||
                                                        (((__WF_OP_FILTER_PASSIVE >> 1u32) & 1) == 0 && __t_is_odd)
                                                    || __WF_OP_FILTER_ACTIVATION.always ||
                                                (__WF_OP_FILTER_ACTIVATION.callback_activated() &&
                                                        __dirty[4usize])) &&
                                        {
                                            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 4usize);
                                            match ::wingfoil::anyhow::Context::context(__wf_op_filter_cycle(&mut (),
                                                            &mut __state_wf_anon_2,
                                                            ((&__v_count, __t_count), (&__v_is_odd, __t_is_odd)),
                                                            &mut __ctx), "node 4 (wf_anon_2: filter) cycle")? {
                                                ::wingfoil::op::Tick::Value(__val) => {
                                                    __v_wf_anon_2 = __val;
                                                    true
                                                }
                                                ::wingfoil::op::Tick::Silent(__val) => {
                                                    __v_wf_anon_2 = __val;
                                                    false
                                                }
                                                ::wingfoil::op::Tick::Quiet => false,
                                            }
                                        };
                                let _ = __t_wf_anon_2;
                                let __t_odd_str =
                                    ((((__WF_OP_MAP_PASSIVE >> 0u32) & 1) == 0 && __t_wf_anon_2)
                                                    || __WF_OP_MAP_ACTIVATION.always ||
                                                (__WF_OP_MAP_ACTIVATION.callback_activated() &&
                                                        __dirty[5usize])) &&
                                        {
                                            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 5usize);
                                            match ::wingfoil::anyhow::Context::context(__wf_op_map_cycle_owned(|i|
                                                                ::alloc::__export::must_use({
                                                                        ::alloc::fmt::format(format_args!("{0} is odd", i))
                                                                    }), &mut (), &mut __state_odd_str,
                                                            ((&__v_wf_anon_2, __t_wf_anon_2),), &mut __ctx),
                                                        "node 5 (odd_str: map) cycle")? {
                                                ::wingfoil::op::Tick::Value(__val) => {
                                                    __v_odd_str = __val;
                                                    true
                                                }
                                                ::wingfoil::op::Tick::Silent(__val) => {
                                                    __v_odd_str = __val;
                                                    false
                                                }
                                                ::wingfoil::op::Tick::Quiet => false,
                                            }
                                        };
                                let _ = __t_odd_str;
                                let __t_wf_anon_3 =
                                    ((((__WF_OP_FILTER_PASSIVE >> 0u32) & 1) == 0 && __t_count)
                                                        ||
                                                        (((__WF_OP_FILTER_PASSIVE >> 1u32) & 1) == 0 && __t_is_even)
                                                    || __WF_OP_FILTER_ACTIVATION.always ||
                                                (__WF_OP_FILTER_ACTIVATION.callback_activated() &&
                                                        __dirty[6usize])) &&
                                        {
                                            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 6usize);
                                            match ::wingfoil::anyhow::Context::context(__wf_op_filter_cycle(&mut (),
                                                            &mut __state_wf_anon_3,
                                                            ((&__v_count, __t_count), (&__v_is_even, __t_is_even)),
                                                            &mut __ctx), "node 6 (wf_anon_3: filter) cycle")? {
                                                ::wingfoil::op::Tick::Value(__val) => {
                                                    __v_wf_anon_3 = __val;
                                                    true
                                                }
                                                ::wingfoil::op::Tick::Silent(__val) => {
                                                    __v_wf_anon_3 = __val;
                                                    false
                                                }
                                                ::wingfoil::op::Tick::Quiet => false,
                                            }
                                        };
                                let _ = __t_wf_anon_3;
                                let __t_even_str =
                                    ((((__WF_OP_MAP_PASSIVE >> 0u32) & 1) == 0 && __t_wf_anon_3)
                                                    || __WF_OP_MAP_ACTIVATION.always ||
                                                (__WF_OP_MAP_ACTIVATION.callback_activated() &&
                                                        __dirty[7usize])) &&
                                        {
                                            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 7usize);
                                            match ::wingfoil::anyhow::Context::context(__wf_op_map_cycle_owned(|i|
                                                                ::alloc::__export::must_use({
                                                                        ::alloc::fmt::format(format_args!("{0} is even", i))
                                                                    }), &mut (), &mut __state_even_str,
                                                            ((&__v_wf_anon_3, __t_wf_anon_3),), &mut __ctx),
                                                        "node 7 (even_str: map) cycle")? {
                                                ::wingfoil::op::Tick::Value(__val) => {
                                                    __v_even_str = __val;
                                                    true
                                                }
                                                ::wingfoil::op::Tick::Silent(__val) => {
                                                    __v_even_str = __val;
                                                    false
                                                }
                                                ::wingfoil::op::Tick::Quiet => false,
                                            }
                                        };
                                let _ = __t_even_str;
                                let __t_labelled =
                                    ((((__WF_OP_MERGE_PASSIVE >> 0u32) & 1) == 0 && __t_odd_str)
                                                        ||
                                                        (((__WF_OP_MERGE_PASSIVE >> 1u32) & 1) == 0 && __t_even_str)
                                                    || __WF_OP_MERGE_ACTIVATION.always ||
                                                (__WF_OP_MERGE_ACTIVATION.callback_activated() &&
                                                        __dirty[8usize])) &&
                                        {
                                            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 8usize);
                                            match ::wingfoil::anyhow::Context::context(__wf_op_merge_cycle(&mut (),
                                                            &mut __state_labelled,
                                                            ((&__v_odd_str, __t_odd_str),
                                                                (&__v_even_str, __t_even_str)), &mut __ctx),
                                                        "node 8 (labelled: merge) cycle")? {
                                                ::wingfoil::op::Tick::Value(__val) => {
                                                    __v_labelled = __val;
                                                    true
                                                }
                                                ::wingfoil::op::Tick::Silent(__val) => {
                                                    __v_labelled = __val;
                                                    false
                                                }
                                                ::wingfoil::op::Tick::Quiet => false,
                                            }
                                        };
                                let _ = __t_labelled;
                                let __t_wf_anon_4 =
                                    ((((__WF_OP_WITH_TIME_PASSIVE >> 0u32) & 1) == 0 &&
                                                            __t_labelled) || __WF_OP_WITH_TIME_ACTIVATION.always ||
                                                (__WF_OP_WITH_TIME_ACTIVATION.callback_activated() &&
                                                        __dirty[9usize])) &&
                                        {
                                            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 9usize);
                                            match ::wingfoil::anyhow::Context::context(__wf_op_with_time_cycle(&mut (),
                                                            &mut __state_wf_anon_4, ((&__v_labelled, __t_labelled),),
                                                            &mut __ctx), "node 9 (wf_anon_4: with_time) cycle")? {
                                                ::wingfoil::op::Tick::Value(__val) => {
                                                    __v_wf_anon_4 = __val;
                                                    true
                                                }
                                                ::wingfoil::op::Tick::Silent(__val) => {
                                                    __v_wf_anon_4 = __val;
                                                    false
                                                }
                                                ::wingfoil::op::Tick::Quiet => false,
                                            }
                                        };
                                let _ = __t_wf_anon_4;
                                let __t_sink =
                                    ((((__WF_OP_FOR_EACH_PASSIVE >> 0u32) & 1) == 0 &&
                                                            __t_wf_anon_4) || __WF_OP_FOR_EACH_ACTIVATION.always ||
                                                (__WF_OP_FOR_EACH_ACTIVATION.callback_activated() &&
                                                        __dirty[10usize])) &&
                                        {
                                            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 10usize);
                                            match ::wingfoil::anyhow::Context::context(__wf_op_for_each_cycle_owned(|(t,
                                                                    s): &(NanoTime, String)|
                                                                {
                                                                    {
                                                                        {
                                                                            {
                                                                                let lvl = ::log::Level::Info;
                                                                                if lvl <= ::log::STATIC_MAX_LEVEL &&
                                                                                        lvl <= ::log::max_level() {
                                                                                    ::log::__private_api::log({
                                                                                            ::log::__private_api::GlobalLogger
                                                                                        }, format_args!("{0} odds/evens {1:?}", t.pretty(), s), lvl,
                                                                                        &("wingfoil", "dual_mode::odds_evens",
                                                                                                ::log::__private_api::loc()), ());
                                                                                }
                                                                            }
                                                                        }
                                                                    };
                                                                    Ok(())
                                                                }, &mut (), &mut __state_sink,
                                                            ((&__v_wf_anon_4, __t_wf_anon_4),), &mut __ctx),
                                                        "node 10 (sink: for_each) cycle")? {
                                                ::wingfoil::op::Tick::Value(__val) => {
                                                    __v_sink = __val;
                                                    true
                                                }
                                                ::wingfoil::op::Tick::Silent(__val) => {
                                                    __v_sink = __val;
                                                    false
                                                }
                                                ::wingfoil::op::Tick::Quiet => false,
                                            }
                                        };
                                let _ = __t_sink;
                                __k.end_cycle(&mut __dirty);
                            }
                            ::core::result::Result::Ok(())
                        })();
        if let ::core::result::Result::Err(__e) = __run {
            __first_err = ::core::option::Option::Some(__e);
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 0usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_ticker_stop(&mut __cfg_wf_anon_1,
                            &mut __state_wf_anon_1, (), &mut __ctx),
                        "node 0 (wf_anon_1: ticker) stop") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 1usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_count_stop(&mut (),
                            &mut __state_count, ((&__v_wf_anon_1, false),), &mut __ctx),
                        "node 1 (count: count) stop") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 2usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_map_stop_owned(|i|
                                i.is_multiple_of(2), &mut (), &mut __state_is_even,
                            ((&__v_count, false),), &mut __ctx),
                        "node 2 (is_even: map) stop") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 3usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_map_stop_owned(|b|
                                !b, &mut (), &mut __state_is_odd, ((&__v_is_even, false),),
                            &mut __ctx), "node 3 (is_odd: map) stop") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 4usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_filter_stop(&mut (),
                            &mut __state_wf_anon_2,
                            ((&__v_count, false), (&__v_is_odd, false)), &mut __ctx),
                        "node 4 (wf_anon_2: filter) stop") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 5usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_map_stop_owned(|i|
                                ::alloc::__export::must_use({
                                        ::alloc::fmt::format(format_args!("{0} is odd", i))
                                    }), &mut (), &mut __state_odd_str,
                            ((&__v_wf_anon_2, false),), &mut __ctx),
                        "node 5 (odd_str: map) stop") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 6usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_filter_stop(&mut (),
                            &mut __state_wf_anon_3,
                            ((&__v_count, false), (&__v_is_even, false)), &mut __ctx),
                        "node 6 (wf_anon_3: filter) stop") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 7usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_map_stop_owned(|i|
                                ::alloc::__export::must_use({
                                        ::alloc::fmt::format(format_args!("{0} is even", i))
                                    }), &mut (), &mut __state_even_str,
                            ((&__v_wf_anon_3, false),), &mut __ctx),
                        "node 7 (even_str: map) stop") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 8usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_merge_stop(&mut (),
                            &mut __state_labelled,
                            ((&__v_odd_str, false), (&__v_even_str, false)),
                            &mut __ctx), "node 8 (labelled: merge) stop") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 9usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_with_time_stop(&mut (),
                            &mut __state_wf_anon_4, ((&__v_labelled, false),),
                            &mut __ctx), "node 9 (wf_anon_4: with_time) stop") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 10usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_for_each_stop_owned(|(t,
                                    s): &(NanoTime, String)|
                                {
                                    {
                                        {
                                            {
                                                let lvl = ::log::Level::Info;
                                                if lvl <= ::log::STATIC_MAX_LEVEL &&
                                                        lvl <= ::log::max_level() {
                                                    ::log::__private_api::log({
                                                            ::log::__private_api::GlobalLogger
                                                        }, format_args!("{0} odds/evens {1:?}", t.pretty(), s), lvl,
                                                        &("wingfoil", "dual_mode::odds_evens",
                                                                ::log::__private_api::loc()), ());
                                                }
                                            }
                                        }
                                    };
                                    Ok(())
                                }, &mut (), &mut __state_sink, ((&__v_wf_anon_4, false),),
                            &mut __ctx), "node 10 (sink: for_each) stop") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 0usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_ticker_teardown(&mut __cfg_wf_anon_1,
                            &mut __state_wf_anon_1, (), &mut __ctx),
                        "node 0 (wf_anon_1: ticker) teardown") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 1usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_count_teardown(&mut (),
                            &mut __state_count, ((&__v_wf_anon_1, false),), &mut __ctx),
                        "node 1 (count: count) teardown") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 2usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_map_teardown_owned(|i|
                                i.is_multiple_of(2), &mut (), &mut __state_is_even,
                            ((&__v_count, false),), &mut __ctx),
                        "node 2 (is_even: map) teardown") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 3usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_map_teardown_owned(|b|
                                !b, &mut (), &mut __state_is_odd, ((&__v_is_even, false),),
                            &mut __ctx), "node 3 (is_odd: map) teardown") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 4usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_filter_teardown(&mut (),
                            &mut __state_wf_anon_2,
                            ((&__v_count, false), (&__v_is_odd, false)), &mut __ctx),
                        "node 4 (wf_anon_2: filter) teardown") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 5usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_map_teardown_owned(|i|
                                ::alloc::__export::must_use({
                                        ::alloc::fmt::format(format_args!("{0} is odd", i))
                                    }), &mut (), &mut __state_odd_str,
                            ((&__v_wf_anon_2, false),), &mut __ctx),
                        "node 5 (odd_str: map) teardown") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 6usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_filter_teardown(&mut (),
                            &mut __state_wf_anon_3,
                            ((&__v_count, false), (&__v_is_even, false)), &mut __ctx),
                        "node 6 (wf_anon_3: filter) teardown") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 7usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_map_teardown_owned(|i|
                                ::alloc::__export::must_use({
                                        ::alloc::fmt::format(format_args!("{0} is even", i))
                                    }), &mut (), &mut __state_even_str,
                            ((&__v_wf_anon_3, false),), &mut __ctx),
                        "node 7 (even_str: map) teardown") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 8usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_merge_teardown(&mut (),
                            &mut __state_labelled,
                            ((&__v_odd_str, false), (&__v_even_str, false)),
                            &mut __ctx), "node 8 (labelled: merge) teardown") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 9usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_with_time_teardown(&mut (),
                            &mut __state_wf_anon_4, ((&__v_labelled, false),),
                            &mut __ctx), "node 9 (wf_anon_4: with_time) teardown") {
                __first_err.get_or_insert(__e);
            }
        }
        {
            let mut __ctx = ::wingfoil::op::Ctx::new(&mut __k, 10usize);
            if let ::core::result::Result::Err(__e) =
                    ::wingfoil::anyhow::Context::context(__wf_op_for_each_teardown_owned(|(t,
                                    s): &(NanoTime, String)|
                                {
                                    {
                                        {
                                            {
                                                let lvl = ::log::Level::Info;
                                                if lvl <= ::log::STATIC_MAX_LEVEL &&
                                                        lvl <= ::log::max_level() {
                                                    ::log::__private_api::log({
                                                            ::log::__private_api::GlobalLogger
                                                        }, format_args!("{0} odds/evens {1:?}", t.pretty(), s), lvl,
                                                        &("wingfoil", "dual_mode::odds_evens",
                                                                ::log::__private_api::loc()), ());
                                                }
                                            }
                                        }
                                    };
                                    Ok(())
                                }, &mut (), &mut __state_sink, ((&__v_wf_anon_4, false),),
                            &mut __ctx), "node 10 (sink: for_each) teardown") {
                __first_err.get_or_insert(__e);
            }
        }
        match __first_err {
            ::core::option::Option::Some(__e) =>
                ::core::result::Result::Err(__e),
            ::core::option::Option::None => {
                ::core::result::Result::Ok((__v_labelled,))
            }
        }
    }
    #[doc = r" Run the graph on `tier`, returning each output's final value."]
    #[doc = r""]
    #[doc =
    r" The two engines are held to identical values *and* tick times, so"]
    #[doc = r" the tier changes only *how* the graph runs. Develop against"]
    #[doc =
    r" [`Tier::Interpreted`](::wingfoil::Tier::Interpreted) — it carries"]
    #[doc =
    r" per-node error context and honours the `instrument-*` span features"]
    #[doc = r" — and deploy on"]
    #[doc = r" [`Tier::Compiled`](::wingfoil::Tier::Compiled);"]
    #[doc =
    r" `Tier::default()` picks between them from `WINGFOIL_TIER` and the"]
    #[doc = r" build profile."]
    pub fn run(tier: ::wingfoil::Tier, run_mode: ::wingfoil::RunMode,
        run_for: ::wingfoil::RunFor)
        -> ::wingfoil::anyhow::Result<(String,)> {
        let __run_mode = run_mode;
        let __run_for = run_for;
        match tier {
            ::wingfoil::Tier::Interpreted => {
                let (mut __runner, labelled) = interpreted();
                __runner.run(__run_mode, __run_for)?;
                ::core::result::Result::Ok((__runner.value(labelled),))
            }
            ::wingfoil::Tier::Compiled => compiled(__run_mode, __run_for),
        }
    }
    #[doc = r" The whole graph mounted as a **single compiled node** in an"]
    #[doc =
    r" interpreted graph under construction. The outer engine pays one"]
    #[doc =
    r" dyn call per activation for the entire sub-graph; inside, state"]
    #[doc =
    r" lives in one closure and dispatch is monomorphized straight-line"]
    #[doc =
    r" code — the same code `compiled` emits. Inner schedules (tickers,"]
    #[doc = r" delays) are demultiplexed through a private queue; only the"]
    #[doc = r" earliest is forwarded to the outer kernel."]
    #[allow(unused_mut, unused_variables, unused_assignments)]
    pub fn nested(__g: &::wingfoil::fluent::GraphBuilder)
        -> ::wingfoil::fluent::Stream<String> {
        let mut __cfg_wf_anon_1 = PERIOD;
        let mut __state_wf_anon_1 =
            __wf_op_ticker_seed_state(&__cfg_wf_anon_1);
        let mut __v_wf_anon_1 = __wf_op_ticker_seed_value(&__cfg_wf_anon_1);
        let mut __state_count = __wf_op_count_seed_state(&());
        let mut __v_count = __wf_op_count_seed_value(&());
        let mut __state_is_even = __wf_op_map_seed_state(&());
        let mut __v_is_even = __wf_op_map_seed_value(&());
        let mut __state_is_odd = __wf_op_map_seed_state(&());
        let mut __v_is_odd = __wf_op_map_seed_value(&());
        let mut __state_wf_anon_2 = __wf_op_filter_seed_state(&());
        let mut __v_wf_anon_2 = __wf_op_filter_seed_value(&());
        let mut __state_odd_str = __wf_op_map_seed_state(&());
        let mut __v_odd_str = __wf_op_map_seed_value(&());
        let mut __state_wf_anon_3 = __wf_op_filter_seed_state(&());
        let mut __v_wf_anon_3 = __wf_op_filter_seed_value(&());
        let mut __state_even_str = __wf_op_map_seed_state(&());
        let mut __v_even_str = __wf_op_map_seed_value(&());
        let mut __state_labelled = __wf_op_merge_seed_state(&());
        let mut __v_labelled = __wf_op_merge_seed_value(&());
        let mut __state_wf_anon_4 = __wf_op_with_time_seed_state(&());
        let mut __v_wf_anon_4 = __wf_op_with_time_seed_value(&());
        let mut __state_sink = __wf_op_for_each_seed_state(&());
        let mut __v_sink = __wf_op_for_each_seed_value(&());
        let mut __q = ::wingfoil::TimeQueue::<usize>::new();
        let mut __dirty = [false; 11usize];
        let mut __active: ::std::vec::Vec<usize> = ::std::vec::Vec::new();
        let mut __passive: ::std::vec::Vec<usize> = ::std::vec::Vec::new();
        __g.__composite(__active, __passive,
            false || __WF_OP_TICKER_ACTIVATION.callback_activated() ||
                                                    __WF_OP_COUNT_ACTIVATION.callback_activated() ||
                                                __WF_OP_MAP_ACTIVATION.callback_activated() ||
                                            __WF_OP_MAP_ACTIVATION.callback_activated() ||
                                        __WF_OP_FILTER_ACTIVATION.callback_activated() ||
                                    __WF_OP_MAP_ACTIVATION.callback_activated() ||
                                __WF_OP_FILTER_ACTIVATION.callback_activated() ||
                            __WF_OP_MAP_ACTIVATION.callback_activated() ||
                        __WF_OP_MERGE_ACTIVATION.callback_activated() ||
                    __WF_OP_WITH_TIME_ACTIVATION.callback_activated() ||
                __WF_OP_FOR_EACH_ACTIVATION.callback_activated(),
            (false || __WF_OP_TICKER_ACTIVATION.always ||
                                                        __WF_OP_COUNT_ACTIVATION.always ||
                                                    __WF_OP_MAP_ACTIVATION.always ||
                                                __WF_OP_MAP_ACTIVATION.always ||
                                            __WF_OP_FILTER_ACTIVATION.always ||
                                        __WF_OP_MAP_ACTIVATION.always ||
                                    __WF_OP_FILTER_ACTIVATION.always ||
                                __WF_OP_MAP_ACTIVATION.always ||
                            __WF_OP_MERGE_ACTIVATION.always ||
                        __WF_OP_WITH_TIME_ACTIVATION.always ||
                    __WF_OP_FOR_EACH_ACTIVATION.always),
            move |__ctx, __phase|
                {
                    let __now = __ctx.time();
                    let __wall_time = __ctx.wall_time();
                    let __start_time = __ctx.start_time();
                    match __phase {
                        ::wingfoil::op::CompositePhase::Start => {
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 0usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_ticker_start(&mut __cfg_wf_anon_1,
                                            &mut __state_wf_anon_1, &mut __ctx),
                                        "island node 0 (wf_anon_1: ticker) start")?;
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 1usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_count_start(&mut (),
                                            &mut __state_count, &mut __ctx),
                                        "island node 1 (count: count) start")?;
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 2usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_map_start_owned(&mut (),
                                            &mut __state_is_even, &mut __ctx),
                                        "island node 2 (is_even: map) start")?;
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 3usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_map_start_owned(&mut (),
                                            &mut __state_is_odd, &mut __ctx),
                                        "island node 3 (is_odd: map) start")?;
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 4usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_filter_start(&mut (),
                                            &mut __state_wf_anon_2, &mut __ctx),
                                        "island node 4 (wf_anon_2: filter) start")?;
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 5usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_map_start_owned(&mut (),
                                            &mut __state_odd_str, &mut __ctx),
                                        "island node 5 (odd_str: map) start")?;
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 6usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_filter_start(&mut (),
                                            &mut __state_wf_anon_3, &mut __ctx),
                                        "island node 6 (wf_anon_3: filter) start")?;
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 7usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_map_start_owned(&mut (),
                                            &mut __state_even_str, &mut __ctx),
                                        "island node 7 (even_str: map) start")?;
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 8usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_merge_start(&mut (),
                                            &mut __state_labelled, &mut __ctx),
                                        "island node 8 (labelled: merge) start")?;
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 9usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_with_time_start(&mut (),
                                            &mut __state_wf_anon_4, &mut __ctx),
                                        "island node 9 (wf_anon_4: with_time) start")?;
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 10usize);
                                ::wingfoil::anyhow::Context::context(__wf_op_for_each_start_owned(&mut (),
                                            &mut __state_sink, &mut __ctx),
                                        "island node 10 (sink: for_each) start")?;
                            }
                            if let Some(__t) = __q.next_time() { __ctx.schedule(__t); }
                            return ::core::result::Result::Ok(::wingfoil::op::Tick::Quiet);
                        }
                        ::wingfoil::op::CompositePhase::Stop => {
                            let mut __first_err:
                                    ::core::option::Option<::wingfoil::anyhow::Error> =
                                ::core::option::Option::None;
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 0usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_ticker_stop(&mut __cfg_wf_anon_1,
                                                &mut __state_wf_anon_1, (), &mut __ctx),
                                            "island node 0 (wf_anon_1: ticker) stop") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 1usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_count_stop(&mut (),
                                                &mut __state_count, ((&__v_wf_anon_1, false),), &mut __ctx),
                                            "island node 1 (count: count) stop") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 2usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_map_stop_owned(|i|
                                                    i.is_multiple_of(2), &mut (), &mut __state_is_even,
                                                ((&__v_count, false),), &mut __ctx),
                                            "island node 2 (is_even: map) stop") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 3usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_map_stop_owned(|b|
                                                    !b, &mut (), &mut __state_is_odd, ((&__v_is_even, false),),
                                                &mut __ctx), "island node 3 (is_odd: map) stop") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 4usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_filter_stop(&mut (),
                                                &mut __state_wf_anon_2,
                                                ((&__v_count, false), (&__v_is_odd, false)), &mut __ctx),
                                            "island node 4 (wf_anon_2: filter) stop") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 5usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_map_stop_owned(|i|
                                                    ::alloc::__export::must_use({
                                                            ::alloc::fmt::format(format_args!("{0} is odd", i))
                                                        }), &mut (), &mut __state_odd_str,
                                                ((&__v_wf_anon_2, false),), &mut __ctx),
                                            "island node 5 (odd_str: map) stop") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 6usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_filter_stop(&mut (),
                                                &mut __state_wf_anon_3,
                                                ((&__v_count, false), (&__v_is_even, false)), &mut __ctx),
                                            "island node 6 (wf_anon_3: filter) stop") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 7usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_map_stop_owned(|i|
                                                    ::alloc::__export::must_use({
                                                            ::alloc::fmt::format(format_args!("{0} is even", i))
                                                        }), &mut (), &mut __state_even_str,
                                                ((&__v_wf_anon_3, false),), &mut __ctx),
                                            "island node 7 (even_str: map) stop") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 8usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_merge_stop(&mut (),
                                                &mut __state_labelled,
                                                ((&__v_odd_str, false), (&__v_even_str, false)),
                                                &mut __ctx), "island node 8 (labelled: merge) stop") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 9usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_with_time_stop(&mut (),
                                                &mut __state_wf_anon_4, ((&__v_labelled, false),),
                                                &mut __ctx), "island node 9 (wf_anon_4: with_time) stop") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 10usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_for_each_stop_owned(|(t,
                                                        s): &(NanoTime, String)|
                                                    {
                                                        {
                                                            {
                                                                {
                                                                    let lvl = ::log::Level::Info;
                                                                    if lvl <= ::log::STATIC_MAX_LEVEL &&
                                                                            lvl <= ::log::max_level() {
                                                                        ::log::__private_api::log({
                                                                                ::log::__private_api::GlobalLogger
                                                                            }, format_args!("{0} odds/evens {1:?}", t.pretty(), s), lvl,
                                                                            &("wingfoil", "dual_mode::odds_evens",
                                                                                    ::log::__private_api::loc()), ());
                                                                    }
                                                                }
                                                            }
                                                        };
                                                        Ok(())
                                                    }, &mut (), &mut __state_sink, ((&__v_wf_anon_4, false),),
                                                &mut __ctx), "island node 10 (sink: for_each) stop") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            return match __first_err {
                                    ::core::option::Option::Some(__e) => {
                                        ::core::result::Result::Err(__e)
                                    }
                                    ::core::option::Option::None => {
                                        ::core::result::Result::Ok(::wingfoil::op::Tick::Quiet)
                                    }
                                };
                        }
                        ::wingfoil::op::CompositePhase::Teardown => {
                            let mut __first_err:
                                    ::core::option::Option<::wingfoil::anyhow::Error> =
                                ::core::option::Option::None;
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 0usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_ticker_teardown(&mut __cfg_wf_anon_1,
                                                &mut __state_wf_anon_1, (), &mut __ctx),
                                            "island node 0 (wf_anon_1: ticker) teardown") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 1usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_count_teardown(&mut (),
                                                &mut __state_count, ((&__v_wf_anon_1, false),), &mut __ctx),
                                            "island node 1 (count: count) teardown") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 2usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_map_teardown_owned(|i|
                                                    i.is_multiple_of(2), &mut (), &mut __state_is_even,
                                                ((&__v_count, false),), &mut __ctx),
                                            "island node 2 (is_even: map) teardown") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 3usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_map_teardown_owned(|b|
                                                    !b, &mut (), &mut __state_is_odd, ((&__v_is_even, false),),
                                                &mut __ctx), "island node 3 (is_odd: map) teardown") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 4usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_filter_teardown(&mut (),
                                                &mut __state_wf_anon_2,
                                                ((&__v_count, false), (&__v_is_odd, false)), &mut __ctx),
                                            "island node 4 (wf_anon_2: filter) teardown") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 5usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_map_teardown_owned(|i|
                                                    ::alloc::__export::must_use({
                                                            ::alloc::fmt::format(format_args!("{0} is odd", i))
                                                        }), &mut (), &mut __state_odd_str,
                                                ((&__v_wf_anon_2, false),), &mut __ctx),
                                            "island node 5 (odd_str: map) teardown") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 6usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_filter_teardown(&mut (),
                                                &mut __state_wf_anon_3,
                                                ((&__v_count, false), (&__v_is_even, false)), &mut __ctx),
                                            "island node 6 (wf_anon_3: filter) teardown") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 7usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_map_teardown_owned(|i|
                                                    ::alloc::__export::must_use({
                                                            ::alloc::fmt::format(format_args!("{0} is even", i))
                                                        }), &mut (), &mut __state_even_str,
                                                ((&__v_wf_anon_3, false),), &mut __ctx),
                                            "island node 7 (even_str: map) teardown") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 8usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_merge_teardown(&mut (),
                                                &mut __state_labelled,
                                                ((&__v_odd_str, false), (&__v_even_str, false)),
                                                &mut __ctx), "island node 8 (labelled: merge) teardown") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 9usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_with_time_teardown(&mut (),
                                                &mut __state_wf_anon_4, ((&__v_labelled, false),),
                                                &mut __ctx),
                                            "island node 9 (wf_anon_4: with_time) teardown") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 10usize);
                                if let ::core::result::Result::Err(__e) =
                                        ::wingfoil::anyhow::Context::context(__wf_op_for_each_teardown_owned(|(t,
                                                        s): &(NanoTime, String)|
                                                    {
                                                        {
                                                            {
                                                                {
                                                                    let lvl = ::log::Level::Info;
                                                                    if lvl <= ::log::STATIC_MAX_LEVEL &&
                                                                            lvl <= ::log::max_level() {
                                                                        ::log::__private_api::log({
                                                                                ::log::__private_api::GlobalLogger
                                                                            }, format_args!("{0} odds/evens {1:?}", t.pretty(), s), lvl,
                                                                            &("wingfoil", "dual_mode::odds_evens",
                                                                                    ::log::__private_api::loc()), ());
                                                                    }
                                                                }
                                                            }
                                                        };
                                                        Ok(())
                                                    }, &mut (), &mut __state_sink, ((&__v_wf_anon_4, false),),
                                                &mut __ctx), "island node 10 (sink: for_each) teardown") {
                                    __first_err.get_or_insert(__e);
                                }
                            }
                            return match __first_err {
                                    ::core::option::Option::Some(__e) => {
                                        ::core::result::Result::Err(__e)
                                    }
                                    ::core::option::Option::None => {
                                        ::core::result::Result::Ok(::wingfoil::op::Tick::Quiet)
                                    }
                                };
                        }
                        ::wingfoil::op::CompositePhase::Cycle => {}
                    }
                    for __d in __dirty.iter_mut() { *__d = false; }
                    while let Some(__ix) = __q.pop_if_pending(__now) {
                        __dirty[__ix] = true;
                    }
                    let __t_wf_anon_1 =
                        (__WF_OP_TICKER_ACTIVATION.always ||
                                    (__WF_OP_TICKER_ACTIVATION.callback_activated() &&
                                            __dirty[0usize])) &&
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 0usize);
                                match ::wingfoil::anyhow::Context::context(__wf_op_ticker_cycle(&mut __cfg_wf_anon_1,
                                                &mut __state_wf_anon_1, (), &mut __ctx),
                                            "island node 0 (wf_anon_1: ticker) cycle")? {
                                    ::wingfoil::op::Tick::Value(__val) => {
                                        __v_wf_anon_1 = __val;
                                        true
                                    }
                                    ::wingfoil::op::Tick::Silent(__val) => {
                                        __v_wf_anon_1 = __val;
                                        false
                                    }
                                    ::wingfoil::op::Tick::Quiet => false,
                                }
                            };
                    let _ = __t_wf_anon_1;
                    let __t_count =
                        ((((__WF_OP_COUNT_PASSIVE >> 0u32) & 1) == 0 &&
                                                __t_wf_anon_1) || __WF_OP_COUNT_ACTIVATION.always ||
                                    (__WF_OP_COUNT_ACTIVATION.callback_activated() &&
                                            __dirty[1usize])) &&
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 1usize);
                                match ::wingfoil::anyhow::Context::context(__wf_op_count_cycle(&mut (),
                                                &mut __state_count, ((&__v_wf_anon_1, __t_wf_anon_1),),
                                                &mut __ctx), "island node 1 (count: count) cycle")? {
                                    ::wingfoil::op::Tick::Value(__val) => {
                                        __v_count = __val;
                                        true
                                    }
                                    ::wingfoil::op::Tick::Silent(__val) => {
                                        __v_count = __val;
                                        false
                                    }
                                    ::wingfoil::op::Tick::Quiet => false,
                                }
                            };
                    let _ = __t_count;
                    let __t_is_even =
                        ((((__WF_OP_MAP_PASSIVE >> 0u32) & 1) == 0 && __t_count) ||
                                        __WF_OP_MAP_ACTIVATION.always ||
                                    (__WF_OP_MAP_ACTIVATION.callback_activated() &&
                                            __dirty[2usize])) &&
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 2usize);
                                match ::wingfoil::anyhow::Context::context(__wf_op_map_cycle_owned(|i|
                                                    i.is_multiple_of(2), &mut (), &mut __state_is_even,
                                                ((&__v_count, __t_count),), &mut __ctx),
                                            "island node 2 (is_even: map) cycle")? {
                                    ::wingfoil::op::Tick::Value(__val) => {
                                        __v_is_even = __val;
                                        true
                                    }
                                    ::wingfoil::op::Tick::Silent(__val) => {
                                        __v_is_even = __val;
                                        false
                                    }
                                    ::wingfoil::op::Tick::Quiet => false,
                                }
                            };
                    let _ = __t_is_even;
                    let __t_is_odd =
                        ((((__WF_OP_MAP_PASSIVE >> 0u32) & 1) == 0 && __t_is_even)
                                        || __WF_OP_MAP_ACTIVATION.always ||
                                    (__WF_OP_MAP_ACTIVATION.callback_activated() &&
                                            __dirty[3usize])) &&
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 3usize);
                                match ::wingfoil::anyhow::Context::context(__wf_op_map_cycle_owned(|b|
                                                    !b, &mut (), &mut __state_is_odd,
                                                ((&__v_is_even, __t_is_even),), &mut __ctx),
                                            "island node 3 (is_odd: map) cycle")? {
                                    ::wingfoil::op::Tick::Value(__val) => {
                                        __v_is_odd = __val;
                                        true
                                    }
                                    ::wingfoil::op::Tick::Silent(__val) => {
                                        __v_is_odd = __val;
                                        false
                                    }
                                    ::wingfoil::op::Tick::Quiet => false,
                                }
                            };
                    let _ = __t_is_odd;
                    let __t_wf_anon_2 =
                        ((((__WF_OP_FILTER_PASSIVE >> 0u32) & 1) == 0 && __t_count)
                                            ||
                                            (((__WF_OP_FILTER_PASSIVE >> 1u32) & 1) == 0 && __t_is_odd)
                                        || __WF_OP_FILTER_ACTIVATION.always ||
                                    (__WF_OP_FILTER_ACTIVATION.callback_activated() &&
                                            __dirty[4usize])) &&
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 4usize);
                                match ::wingfoil::anyhow::Context::context(__wf_op_filter_cycle(&mut (),
                                                &mut __state_wf_anon_2,
                                                ((&__v_count, __t_count), (&__v_is_odd, __t_is_odd)),
                                                &mut __ctx), "island node 4 (wf_anon_2: filter) cycle")? {
                                    ::wingfoil::op::Tick::Value(__val) => {
                                        __v_wf_anon_2 = __val;
                                        true
                                    }
                                    ::wingfoil::op::Tick::Silent(__val) => {
                                        __v_wf_anon_2 = __val;
                                        false
                                    }
                                    ::wingfoil::op::Tick::Quiet => false,
                                }
                            };
                    let _ = __t_wf_anon_2;
                    let __t_odd_str =
                        ((((__WF_OP_MAP_PASSIVE >> 0u32) & 1) == 0 && __t_wf_anon_2)
                                        || __WF_OP_MAP_ACTIVATION.always ||
                                    (__WF_OP_MAP_ACTIVATION.callback_activated() &&
                                            __dirty[5usize])) &&
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 5usize);
                                match ::wingfoil::anyhow::Context::context(__wf_op_map_cycle_owned(|i|
                                                    ::alloc::__export::must_use({
                                                            ::alloc::fmt::format(format_args!("{0} is odd", i))
                                                        }), &mut (), &mut __state_odd_str,
                                                ((&__v_wf_anon_2, __t_wf_anon_2),), &mut __ctx),
                                            "island node 5 (odd_str: map) cycle")? {
                                    ::wingfoil::op::Tick::Value(__val) => {
                                        __v_odd_str = __val;
                                        true
                                    }
                                    ::wingfoil::op::Tick::Silent(__val) => {
                                        __v_odd_str = __val;
                                        false
                                    }
                                    ::wingfoil::op::Tick::Quiet => false,
                                }
                            };
                    let _ = __t_odd_str;
                    let __t_wf_anon_3 =
                        ((((__WF_OP_FILTER_PASSIVE >> 0u32) & 1) == 0 && __t_count)
                                            ||
                                            (((__WF_OP_FILTER_PASSIVE >> 1u32) & 1) == 0 && __t_is_even)
                                        || __WF_OP_FILTER_ACTIVATION.always ||
                                    (__WF_OP_FILTER_ACTIVATION.callback_activated() &&
                                            __dirty[6usize])) &&
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 6usize);
                                match ::wingfoil::anyhow::Context::context(__wf_op_filter_cycle(&mut (),
                                                &mut __state_wf_anon_3,
                                                ((&__v_count, __t_count), (&__v_is_even, __t_is_even)),
                                                &mut __ctx), "island node 6 (wf_anon_3: filter) cycle")? {
                                    ::wingfoil::op::Tick::Value(__val) => {
                                        __v_wf_anon_3 = __val;
                                        true
                                    }
                                    ::wingfoil::op::Tick::Silent(__val) => {
                                        __v_wf_anon_3 = __val;
                                        false
                                    }
                                    ::wingfoil::op::Tick::Quiet => false,
                                }
                            };
                    let _ = __t_wf_anon_3;
                    let __t_even_str =
                        ((((__WF_OP_MAP_PASSIVE >> 0u32) & 1) == 0 && __t_wf_anon_3)
                                        || __WF_OP_MAP_ACTIVATION.always ||
                                    (__WF_OP_MAP_ACTIVATION.callback_activated() &&
                                            __dirty[7usize])) &&
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 7usize);
                                match ::wingfoil::anyhow::Context::context(__wf_op_map_cycle_owned(|i|
                                                    ::alloc::__export::must_use({
                                                            ::alloc::fmt::format(format_args!("{0} is even", i))
                                                        }), &mut (), &mut __state_even_str,
                                                ((&__v_wf_anon_3, __t_wf_anon_3),), &mut __ctx),
                                            "island node 7 (even_str: map) cycle")? {
                                    ::wingfoil::op::Tick::Value(__val) => {
                                        __v_even_str = __val;
                                        true
                                    }
                                    ::wingfoil::op::Tick::Silent(__val) => {
                                        __v_even_str = __val;
                                        false
                                    }
                                    ::wingfoil::op::Tick::Quiet => false,
                                }
                            };
                    let _ = __t_even_str;
                    let __t_labelled =
                        ((((__WF_OP_MERGE_PASSIVE >> 0u32) & 1) == 0 && __t_odd_str)
                                            ||
                                            (((__WF_OP_MERGE_PASSIVE >> 1u32) & 1) == 0 && __t_even_str)
                                        || __WF_OP_MERGE_ACTIVATION.always ||
                                    (__WF_OP_MERGE_ACTIVATION.callback_activated() &&
                                            __dirty[8usize])) &&
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 8usize);
                                match ::wingfoil::anyhow::Context::context(__wf_op_merge_cycle(&mut (),
                                                &mut __state_labelled,
                                                ((&__v_odd_str, __t_odd_str),
                                                    (&__v_even_str, __t_even_str)), &mut __ctx),
                                            "island node 8 (labelled: merge) cycle")? {
                                    ::wingfoil::op::Tick::Value(__val) => {
                                        __v_labelled = __val;
                                        true
                                    }
                                    ::wingfoil::op::Tick::Silent(__val) => {
                                        __v_labelled = __val;
                                        false
                                    }
                                    ::wingfoil::op::Tick::Quiet => false,
                                }
                            };
                    let _ = __t_labelled;
                    let __t_wf_anon_4 =
                        ((((__WF_OP_WITH_TIME_PASSIVE >> 0u32) & 1) == 0 &&
                                                __t_labelled) || __WF_OP_WITH_TIME_ACTIVATION.always ||
                                    (__WF_OP_WITH_TIME_ACTIVATION.callback_activated() &&
                                            __dirty[9usize])) &&
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 9usize);
                                match ::wingfoil::anyhow::Context::context(__wf_op_with_time_cycle(&mut (),
                                                &mut __state_wf_anon_4, ((&__v_labelled, __t_labelled),),
                                                &mut __ctx), "island node 9 (wf_anon_4: with_time) cycle")?
                                    {
                                    ::wingfoil::op::Tick::Value(__val) => {
                                        __v_wf_anon_4 = __val;
                                        true
                                    }
                                    ::wingfoil::op::Tick::Silent(__val) => {
                                        __v_wf_anon_4 = __val;
                                        false
                                    }
                                    ::wingfoil::op::Tick::Quiet => false,
                                }
                            };
                    let _ = __t_wf_anon_4;
                    let __t_sink =
                        ((((__WF_OP_FOR_EACH_PASSIVE >> 0u32) & 1) == 0 &&
                                                __t_wf_anon_4) || __WF_OP_FOR_EACH_ACTIVATION.always ||
                                    (__WF_OP_FOR_EACH_ACTIVATION.callback_activated() &&
                                            __dirty[10usize])) &&
                            {
                                let mut __ctx =
                                    ::wingfoil::op::Ctx::nested(__now, __wall_time,
                                        __start_time, &mut __q, 10usize);
                                match ::wingfoil::anyhow::Context::context(__wf_op_for_each_cycle_owned(|(t,
                                                        s): &(NanoTime, String)|
                                                    {
                                                        {
                                                            {
                                                                {
                                                                    let lvl = ::log::Level::Info;
                                                                    if lvl <= ::log::STATIC_MAX_LEVEL &&
                                                                            lvl <= ::log::max_level() {
                                                                        ::log::__private_api::log({
                                                                                ::log::__private_api::GlobalLogger
                                                                            }, format_args!("{0} odds/evens {1:?}", t.pretty(), s), lvl,
                                                                            &("wingfoil", "dual_mode::odds_evens",
                                                                                    ::log::__private_api::loc()), ());
                                                                    }
                                                                }
                                                            }
                                                        };
                                                        Ok(())
                                                    }, &mut (), &mut __state_sink,
                                                ((&__v_wf_anon_4, __t_wf_anon_4),), &mut __ctx),
                                            "island node 10 (sink: for_each) cycle")? {
                                    ::wingfoil::op::Tick::Value(__val) => {
                                        __v_sink = __val;
                                        true
                                    }
                                    ::wingfoil::op::Tick::Silent(__val) => {
                                        __v_sink = __val;
                                        false
                                    }
                                    ::wingfoil::op::Tick::Quiet => false,
                                }
                            };
                    let _ = __t_sink;
                    if let Some(__t) = __q.next_time() { __ctx.schedule(__t); }
                    ::core::result::Result::Ok(if __t_labelled {
                            ::wingfoil::op::Tick::Value(::core::clone::Clone::clone(&__v_labelled))
                        } else { ::wingfoil::op::Tick::Quiet })
                })
    }
}
fn main() -> anyhow::Result<()> {
    env_logger::init();
    let tier = Tier::default();
    let (last,) = odds_evens::run(tier, HISTORICAL, RunFor::Cycles(CYCLES))?;
    {
        ::std::io::_print(format_args!("ran {0} cycles on the {1} tier; last label: {2:?}\n",
                CYCLES, tier, last));
    };
    Ok(())
}
