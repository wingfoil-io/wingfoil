//! **SPIKE (#726 step 2)** — does closure quotation survive `nitro!`?
//!
//! Two things had to hold for the `func!` design to be buildable at all, and
//! only the first was ever argued in the decision doc:
//!
//! 1. **Coherence** — a blanket `OpFn` impl over `Fn`, plus a specific impl for
//!    `QuotedFn`, must not overlap. (Proven by this crate compiling.)
//! 2. **Inference through the compiled path** — this is the one the doc does
//!    not address. `nitro!` classifies a *literal closure* argument as a
//!    by-value config and routes it through `_cycle_owned`, precisely so rustc
//!    defers the closure's signature inference until the sibling `input`
//!    argument has resolved the op's value types. `func!(|x| ..)` is a **macro
//!    call, not a closure literal**, so the macro classifies it as a plain
//!    config instead — hoisted to a `__cfg_<name>` local and passed by `&mut`,
//!    the position where that deferral explicitly does *not* apply.
//!
//! So the quoted form takes a different path through the macro than the
//! unquoted form. That is what these tests are really probing.

use std::time::Duration;

use wingfoil::func;
use wingfoil::prelude::*;
use wingfoil::quote::{OpFn, QuotedFn};
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(1);
const RUN: RunFor = RunFor::Cycles(5);

// ---------------------------------------------------------------------------
// The quotation primitive on its own, before any graph is involved.
// ---------------------------------------------------------------------------

/// `stringify!` on an `$f:expr` metavariable returns the **verbatim source
/// snippet**, spacing intact — not the token stream re-printed with normalised
/// spacing (`| x : & f64 | x * 2.0`), which is what `stringify!` does to a raw
/// token sequence. Worth pinning: §5 wants the generated artifact to be
/// "reviewable plain Rust", and this is what decides whether it reads like
/// hand-written code. It also means a `macro_rules!` `func!` is sufficient —
/// no proc macro with `Span::source_text()` needed for the source-recovery
/// half of the design.
#[test]
fn func_keeps_the_tokens_it_was_written_with_verbatim() {
    let q = func!(|x: &f64| x * 2.0);
    assert_eq!(8.0, OpFn::call(&q, &4.0), "the closure still runs");
    assert_eq!(
        Some("|x: &f64| x * 2.0"),
        OpFn::<f64, f64>::src(&q),
        "source text comes back exactly as written"
    );
    let (file, line) = OpFn::<f64, f64>::loc(&q).expect("quoted fns carry a location");
    assert!(file.ends_with("quotation_spike.rs"), "file was {file}");
    assert!(line > 0);
}

/// The property that makes the whole design safe: the string and the closure
/// come from the *same tokens*, so what a generator would emit re-parses to
/// what actually ran. Drift is structurally impossible, rather than merely
/// discouraged — the failure mode that sank the deleted legacy `codegen`
/// retrofit, where a human re-stated each closure by hand.
#[test]
fn emitted_source_reparses_to_the_behaviour_that_ran() {
    let q = func!(|x: &f64| x * 2.0);
    let src = OpFn::<f64, f64>::src(&q).expect("quoted");
    // What the generator would splice into the artifact, re-stated here as a
    // closure literal — the same tokens, at a different call site.
    let respliced = |x: &f64| x * 2.0;
    assert_eq!("|x: &f64| x * 2.0", src);
    for probe in [0.0f64, 1.5, -3.25, 1e9] {
        assert_eq!(
            OpFn::call(&q, &probe),
            respliced(&probe),
            "quoted and re-spliced forms diverged at {probe}"
        );
    }
}

/// The blanket impl: a plain closure is still a valid config, and reports no
/// source — which is the signal a generator would use to name an ineligible
/// node and its call site.
#[test]
fn a_plain_closure_reports_no_source() {
    let f = |x: &f64| x * 3.0;
    assert_eq!(12.0, OpFn::call(&f, &4.0));
    assert_eq!(None, OpFn::<f64, f64>::src(&f));
}

// ---------------------------------------------------------------------------
// Through `nitro!`: the unquoted form (literal closure -> `_cycle_owned`).
// ---------------------------------------------------------------------------

wingfoil::nitro! {
    fn unquoted(g: &GraphBuilder) -> Stream<f64> {
        let src = g.ticker(PERIOD).count().map(|i: &u64| *i as f64);
        let out = src.quoted_map(|x: &f64| x * 2.0);
        out
    }
}

#[test]
fn unquoted_closure_through_opfn_bound_matches_across_tiers() {
    let (mut runner, out) = unquoted::interpreted();
    runner.run(HISTORICAL, RUN).unwrap();
    let interpreted = runner.value(out);
    assert_eq!(10.0, interpreted, "5 ticks, counted, doubled");

    let (compiled,) = unquoted::compiled(HISTORICAL, RUN).unwrap();
    assert_eq!(interpreted, compiled, "compiled must match interpreted");

    let g = GraphBuilder::new();
    let island = unquoted::nested(&g);
    let mut runner = g.build();
    runner.run(HISTORICAL, RUN).unwrap();
    assert_eq!(interpreted, runner.value(island), "nested must match");
}

// ---------------------------------------------------------------------------
// Through `nitro!`: the quoted form. `func!(..)` is not a closure literal, so
// this rides the plain-config path (`&mut cfg`) rather than `_cycle_owned`.
// ---------------------------------------------------------------------------

wingfoil::nitro! {
    fn quoted(g: &GraphBuilder) -> Stream<f64> {
        let src = g.ticker(PERIOD).count().map(|i: &u64| *i as f64);
        let out = src.quoted_map(func!(|x: &f64| x * 2.0));
        out
    }
}

#[test]
fn quoted_closure_matches_across_tiers() {
    let (mut runner, out) = quoted::interpreted();
    runner.run(HISTORICAL, RUN).unwrap();
    let interpreted = runner.value(out);
    assert_eq!(
        10.0, interpreted,
        "quoted graph must compute the same values"
    );

    let (compiled,) = quoted::compiled(HISTORICAL, RUN).unwrap();
    assert_eq!(interpreted, compiled, "compiled must match interpreted");

    let g = GraphBuilder::new();
    let island = quoted::nested(&g);
    let mut runner = g.build();
    runner.run(HISTORICAL, RUN).unwrap();
    assert_eq!(interpreted, runner.value(island), "nested must match");
}

// ---------------------------------------------------------------------------
// The fluent (interpreted) layer accepts both forms interchangeably — the
// "regular closures stay first-class" property of §4.
// ---------------------------------------------------------------------------

#[test]
fn fluent_accepts_quoted_and_plain_interchangeably() {
    let g = GraphBuilder::new();
    let src = g.ticker(PERIOD).count().map(|i: &u64| *i as f64);
    let plain = src.quoted_map(|x: &f64| x + 1.0);
    let quoted = src.quoted_map(func!(|x: &f64| x + 1.0));
    let mut runner = g.build();
    runner.run(HISTORICAL, RUN).unwrap();
    assert_eq!(runner.value(plain), runner.value(quoted));
}

/// A `QuotedFn` is `Copy` when its closure is, so a config can be rebuilt per
/// cycle the way the compiled expansion rebuilds zero-capture closures.
#[test]
fn quoted_fn_is_copy() {
    fn takes_copy<T: Copy>(_: T) {}
    takes_copy(func!(|x: &f64| x * 2.0));
    let q: QuotedFn<fn(&f64) -> f64> = func!(|x: &f64| x * 2.0);
    let _also = q;
    assert_eq!(
        4.0,
        OpFn::call(&q, &2.0),
        "original still usable after copy"
    );
}
