//! **The `_q` twins**: `func!` as the only quotation vocabulary, on ordinary
//! chainable methods.
//!
//! `map` itself cannot take a quotation: its bound must be `Fn(&T) -> B` for a
//! closure literal to infer, and [`QuotedFn`](wingfoil::quote::QuotedFn) is a
//! struct that cannot implement `Fn` on stable Rust. So the quotation needs its
//! own method — and it needs to travel as a *value*, which is what lets one
//! quotation be bound to a variable and reused across a loop.

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode, func};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(1);
const RUN: RunFor = RunFor::Cycles(5);

/// **The annotation is not the twins' fault, and is worth knowing about.**
/// `func!` binds the closure with no expected type in sight, so it is typed
/// before any `Fn` bound is in view and comes out with a *specific* lifetime
/// rather than a higher-ranked one. Any form that names the quotation before
/// wiring it pays this, including a wrapping macro — which is why a macro
/// would have bought nothing for the syntax it cost.
///
/// Only the annotated form is testable here; the unannotated one is a compile
/// error, which is exactly why the equivalence was easy to assume and wrong.
#[test]
fn a_quotation_records_through_an_ordinary_method() {
    let g = GraphBuilder::new();
    let doubled = g.ticker(PERIOD).count().map_q(func!(|i: &u64| i * 2));

    assert_eq!(Some("|i: &u64| i * 2"), doubled.src().as_deref());
}

/// The property the macro could not give: **chaining**.
#[test]
fn the_twins_chain() {
    let g = GraphBuilder::new();
    let out = g
        .ticker(PERIOD)
        .count()
        .map_q(func!(|i: &u64| i * 2))
        .filter_value_q(func!(|v: &u64| *v > 4))
        .map_q(func!(|v: &u64| v + 1));

    let srcs: Vec<_> = g.describe().iter().filter_map(|n| n.src.clone()).collect();
    assert_eq!(
        vec!["|i: &u64| i * 2", "|v: &u64| *v > 4", "|v: &u64| v + 1"],
        srcs
    );
    assert!(out.src().is_some());
}

/// Every arity: one input, two (`join`), a seed (`fold`), and a sink.
#[test]
fn the_twins_cover_every_shape() {
    let g = GraphBuilder::new();
    let a = g.ticker(PERIOD).count();
    let b = a.map_q(func!(|i: &u64| i * 10));
    let joined = a.join_q(&b, func!(|x: &u64, y: &u64| x + y));
    let total = joined.fold_q(0u64, func!(|acc: &mut u64, v: &u64| *acc += v));
    let sunk = total.for_each_q(func!(|_v: &u64| wingfoil::anyhow::Ok(())));

    assert_eq!(Some("|i: &u64| i * 10"), b.src().as_deref());
    assert_eq!(Some("|x: &u64, y: &u64| x + y"), joined.src().as_deref());
    assert_eq!(
        Some("|acc: &mut u64, v: &u64| *acc += v"),
        total.src().as_deref()
    );
    assert!(sunk.src().is_some(), "a sink records too");
}

/// Tier-2 captures ride the same method — `func!` is the only vocabulary.
#[test]
fn captures_need_no_separate_form() {
    let g = GraphBuilder::new();
    let fee = 2.5f64;
    let net = g
        .ticker(PERIOD)
        .count()
        .map_q(func!(|n: &u64| *n as f64))
        .map_q(func!([fee] move |p: &f64| p - fee));

    assert_eq!(
        Some("{ let fee = 2.5f64; move |p: &f64| p - fee }"),
        net.src().as_deref()
    );
}

/// The twin records exactly what the manual pair recorded, so it is a
/// convenience and not a second mechanism.
#[test]
fn a_twin_matches_the_manual_pair() {
    let manual = {
        let g = GraphBuilder::new();
        let q = func!(|i: &u64| i * 2);
        g.ticker(PERIOD).count().map(q.f).with_src(&q).src()
    };
    let twin = {
        let g = GraphBuilder::new();
        g.ticker(PERIOD).count().map_q(func!(|i: &u64| i * 2)).src()
    };
    assert_eq!(manual, twin);
}

/// Quotation stays wiring-time only.
#[test]
fn the_twins_do_not_change_behaviour() {
    let plain = {
        let g = GraphBuilder::new();
        let out = g.ticker(PERIOD).count().map(|i: &u64| i * 3).accumulate();
        let mut r = g.build();
        r.run(HISTORICAL, RUN).unwrap();
        r.value(out)
    };
    let quoted = {
        let g = GraphBuilder::new();
        let out = g
            .ticker(PERIOD)
            .count()
            .map_q(func!(|i: &u64| i * 3))
            .accumulate();
        let mut r = g.build();
        r.run(HISTORICAL, RUN).unwrap();
        r.value(out)
    };
    assert_eq!(vec![3u64, 6, 9, 12, 15], plain);
    assert_eq!(plain, quoted);
}

/// Inherent, so no trait import is needed to quote — `prelude::*` is a
/// prerequisite for the plain combinators, not for their twins.
#[test]
fn the_twins_need_no_trait_in_scope() {
    mod isolated {
        use wingfoil::fluent::{GraphBuilder, SourceOps};
        use wingfoil::func;

        pub fn wire() -> Option<String> {
            let g = GraphBuilder::new();
            // Not even `StreamOps` — `count` and `map_q` are both reachable
            // without it here; the twins are inherent by construction.
            let out = g
                .ticker(std::time::Duration::from_millis(1))
                .count()
                .map_q(func!(|i: &u64| i * 2));
            out.src()
        }
    }
    assert_eq!(Some("|i: &u64| i * 2"), isolated::wire().as_deref());
}
