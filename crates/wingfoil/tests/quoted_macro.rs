//! **`quoted!`**: wire one node and record its closure in a single step.
//!
//! The `func!` + `.with_src(..)` pair is correct but forgettable, and
//! forgetting is silent — the graph runs fine interpreted, and the omission
//! only surfaces later as a generator refusal. These tests pin that the
//! shorthand produces *exactly* what the manual form produces, so it stays a
//! convenience rather than becoming a second mechanism that can drift.

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode, func, quoted};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(1);
const RUN: RunFor = RunFor::Cycles(5);

#[test]
fn quoted_records_what_the_manual_form_records() {
    let manual = {
        let g = GraphBuilder::new();
        let double = func!(|i: &u64| i * 2);
        g.ticker(PERIOD)
            .count()
            .map(double.f)
            .with_src(&double)
            .src()
    };

    let shorthand = {
        let g = GraphBuilder::new();
        let ticks = g.ticker(PERIOD).count();
        quoted!(ticks => map(|i: &u64| i * 2)).src()
    };

    assert_eq!(Some("|i: &u64| i * 2"), shorthand.as_deref());
    assert_eq!(
        manual, shorthand,
        "shorthand must not be a second mechanism"
    );
}

/// **The property the whole `=>` syntax exists to protect.** Source text must
/// survive two `macro_rules!` layers verbatim — `quoted!`'s `$f:expr` into
/// `func!`'s `stringify!($f)` — because a generated artifact carrying
/// `| i : & u64 | i * 2` is one `rustfmt` cannot repair (it does not format
/// inside macro bodies).
#[test]
fn source_stays_verbatim_through_both_macro_layers() {
    let g = GraphBuilder::new();
    let ticks = g.ticker(PERIOD).count();
    let out = quoted!(ticks => map(|i: &u64| i * 2));
    assert_eq!(Some("|i: &u64| i * 2"), out.src().as_deref());
    assert!(
        !out.src().unwrap().contains(" : "),
        "spacing was normalised: {:?}",
        out.src()
    );
}

/// The two-argument form: a stream edge (`join`) or a seed (`fold`) ahead of
/// the closure.
#[test]
fn the_leading_argument_form_covers_join_and_fold() {
    let g = GraphBuilder::new();
    let a = g.ticker(PERIOD).count();
    let b = quoted!(a => map(|i: &u64| i * 10));
    let joined = quoted!(a => join(&b, |x: &u64, y: &u64| x + y));
    let total = quoted!(joined => fold(0u64, |acc: &mut u64, v: &u64| *acc += v));

    assert_eq!(Some("|i: &u64| i * 10"), b.src().as_deref());
    assert_eq!(Some("|x: &u64, y: &u64| x + y"), joined.src().as_deref());
    assert_eq!(
        Some("|acc: &mut u64, v: &u64| *acc += v"),
        total.src().as_deref()
    );
}

/// Each node carries its own quotation, so a graph built out of `quoted!` calls
/// is fully described.
#[test]
fn every_node_built_this_way_is_recorded() {
    let g = GraphBuilder::new();
    let ticks = g.ticker(PERIOD).count();
    let doubled = quoted!(ticks => map(|i: &u64| i * 2));
    let _big = quoted!(doubled => filter_value(|v: &u64| *v > 4));

    let srcs: Vec<_> = g.describe().iter().filter_map(|n| n.src.clone()).collect();
    assert_eq!(vec!["|i: &u64| i * 2", "|v: &u64| *v > 4"], srcs);
}

/// Recording is wiring-time only: the shorthand must not change what the graph
/// computes.
#[test]
fn quoted_does_not_change_behaviour() {
    let plain = {
        let g = GraphBuilder::new();
        let out = g.ticker(PERIOD).count().map(|i: &u64| i * 3).accumulate();
        let mut runner = g.build();
        runner.run(HISTORICAL, RUN).unwrap();
        runner.value(out)
    };

    let shorthand = {
        let g = GraphBuilder::new();
        let ticks = g.ticker(PERIOD).count();
        let out = quoted!(ticks => map(|i: &u64| i * 3)).accumulate();
        let mut runner = g.build();
        runner.run(HISTORICAL, RUN).unwrap();
        runner.value(out)
    };

    assert_eq!(vec![3u64, 6, 9, 12, 15], plain);
    assert_eq!(plain, shorthand);
}

/// It knows nothing about the catalog — it expands to an ordinary fluent call,
/// so it works for any op with a closure config, built-in or user-defined.
#[test]
fn it_is_not_coupled_to_any_particular_op() {
    let g = GraphBuilder::new();
    let ticks = g.ticker(PERIOD).count();
    let inspected = quoted!(ticks => inspect(|_v: &u64| ()));
    assert_eq!(Some("|_v: &u64| ()"), inspected.src().as_deref());
}
