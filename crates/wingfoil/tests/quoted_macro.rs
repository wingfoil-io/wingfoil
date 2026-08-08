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

// ---------------------------------------------------------------------------
// The full argument grammar: `cfg` marks a data config, `[..]` a capture list.
// ---------------------------------------------------------------------------

/// `cfg` marks an argument to record as a data config. Since `#[op(emit_cfg)]`
/// landed, `ticker` records itself and needs no marker — this arm exists for the
/// ops whose config type is *generic* (`fold`, `scan`), where the bound cannot
/// go on the public signature. Kept exercised on `ticker` anyway, because the
/// arm should keep working wherever a caller reaches for it.
#[test]
fn cfg_marks_a_data_config() {
    let g = GraphBuilder::new();
    let ticks = quoted!(g => ticker(cfg PERIOD));

    assert_eq!(
        Some("::core::time::Duration::new(0u64, 1000000u32)"),
        ticks.cfg_src().as_deref()
    );
    assert_eq!(None, ticks.src(), "a ticker has no closure");
}

/// A capture list inside `quoted!` forwards to `func!`'s tier-2 arm, so the
/// per-instrument parameter case needs no manual `func!` + `with_src`.
#[test]
fn a_capture_list_is_forwarded_to_func() {
    let g = GraphBuilder::new();
    let fee = 2.5f64;
    let px = g.ticker(PERIOD).count().map(|n: &u64| *n as f64);
    let net = quoted!(px => map([fee] move |p: &f64| p - fee));

    assert_eq!(
        Some("{ let fee = 2.5f64; move |p: &f64| p - fee }"),
        net.src().as_deref()
    );
}

/// Multiple captures, and the manual form it replaces, recording identically.
#[test]
fn captures_match_the_manual_form() {
    let lo = 1u64;
    let hi = 9u64;

    let manual = {
        let g = GraphBuilder::new();
        let q = func!([lo, hi] move |v: &u64| v.clamp(&lo, &hi).to_owned());
        g.ticker(PERIOD).count().map(q.f).with_src(&q).src()
    };
    let shorthand = {
        let g = GraphBuilder::new();
        let ticks = g.ticker(PERIOD).count();
        quoted!(ticks => map([lo, hi] move |v: &u64| v.clamp(&lo, &hi).to_owned())).src()
    };

    assert_eq!(manual, shorthand);
    assert!(
        shorthand
            .unwrap()
            .starts_with("{ let lo = 1u64; let hi = 9u64;")
    );
}

/// `cfg` and a closure together — `fold`'s shape, both recorded from one call.
#[test]
fn cfg_and_a_closure_are_both_recorded() {
    let g = GraphBuilder::new();
    let ticks = g.ticker(PERIOD).count();
    let total = quoted!(ticks => fold(cfg 0u64, |acc: &mut u64, v: &u64| *acc += v));

    assert_eq!(Some("0u64"), total.cfg_src().as_deref());
    assert_eq!(
        Some("|acc: &mut u64, v: &u64| *acc += v"),
        total.src().as_deref()
    );
}

/// A `&stream` edge stays an edge: nothing is recorded about it, because the
/// graph already knows its edges.
#[test]
fn a_stream_edge_is_passed_through_untouched() {
    let g = GraphBuilder::new();
    let a = g.ticker(PERIOD).count();
    let b = quoted!(a => map(|i: &u64| i * 10));
    let joined = quoted!(a => join(&b, |x: &u64, y: &u64| x + y));

    assert_eq!(Some("|x: &u64, y: &u64| x + y"), joined.src().as_deref());
    assert_eq!(None, joined.cfg_src(), "an edge is not a data config");
}

/// The whole per-instrument shape without leaving the macro — the case the
/// generator exists for, and the one that previously needed all three
/// vocabularies by hand.
#[test]
fn a_per_instrument_leg_needs_only_this_macro() {
    struct Instrument {
        /// The synthetic clock's period. Named `period`, not `tick`: in a
        /// trading context a "tick" is a price increment or a single market
        /// update, neither of which is a `Duration`.
        ///
        /// It exists only because `ticker` stands in for a market-data feed —
        /// a real instrument config carries a symbol and a subscription, and
        /// data arrives when it arrives. `external`/`channel` sources are still
        /// excluded from compiled graphs, so the placeholder is load-bearing
        /// for the test rather than representative of the domain.
        period: Duration,
        fee: f64,
    }
    let cfg = [
        Instrument {
            period: Duration::from_millis(1),
            fee: 2.5,
        },
        Instrument {
            period: Duration::from_millis(5),
            fee: 1.0,
        },
    ];

    let g = GraphBuilder::new();
    for inst in &cfg {
        let fee = inst.fee;
        let ticks = g.ticker(inst.period).count();
        let px = quoted!(ticks => map(|n: &u64| *n as f64 * 100.0));
        let _net = quoted!(px => map([fee] move |p: &f64| p - fee));
    }

    // Every node that needs recording has it: both tickers, both maps, both
    // fee legs. `count` needs nothing.
    let nodes = g.describe();
    let unrecorded: Vec<_> = nodes
        .iter()
        .filter(|n| n.takes_closure_cfg && n.src.is_none())
        .collect();
    assert!(unrecorded.is_empty(), "unrecorded closures: {unrecorded:?}");
    assert_eq!(
        2,
        nodes.iter().filter(|n| n.cfg_src.is_some()).count(),
        "both ticker periods recorded"
    );
    assert_eq!(
        2,
        nodes
            .iter()
            .filter(|n| n.src.as_deref().is_some_and(|s| s.contains("let fee =")))
            .count(),
        "both fees frozen"
    );
}

// ---------------------------------------------------------------------------
// Automatic data-config recording (`#[op(emit_cfg)]`).
// ---------------------------------------------------------------------------

/// **An op with a concrete config records itself.** No `cfg` marker, no
/// `with_cfg`, no annotation — `g.ticker(period)` is ordinary wiring and the
/// generator can still emit it. This is what data configs should always have
/// looked like: the value was never erased, it just was not being written down.
#[test]
fn a_concrete_config_is_recorded_without_annotation() {
    let g = GraphBuilder::new();
    let ticks = g.ticker(PERIOD);

    assert_eq!(
        Some("::core::time::Duration::new(0u64, 1000000u32)"),
        ticks.cfg_src().as_deref(),
        "the ticker recorded its own period"
    );
}

/// It covers the concrete-config ops across the catalog, not just `ticker`.
#[test]
fn the_concrete_config_ops_all_record_themselves() {
    let g = GraphBuilder::new();
    let count = g.ticker(PERIOD).count();
    let limited = count.limit(100);
    let delayed = count.delay(Duration::from_millis(25));
    let windowed = count.window(Duration::from_millis(30));
    let buffered = count.buffer(3);
    let throttled = count.throttle(Duration::from_millis(15));

    assert_eq!(Some("100u32"), limited.cfg_src().as_deref());
    assert!(delayed.cfg_src().is_some(), "delay records its duration");
    assert!(windowed.cfg_src().is_some(), "window records its span");
    assert_eq!(Some("3usize"), buffered.cfg_src().as_deref());
    assert!(
        throttled.cfg_src().is_some(),
        "throttle records its interval"
    );
}

/// **The line the bound draws.** A *generic* config cannot be recorded
/// automatically: `EmitLiteral` on `fold`'s seed would forbid folding into any
/// accumulator the generator cannot render — a breaking change for users who
/// are not buying generation. Those keep the manual form, and this pins the
/// asymmetry so it reads as a decision rather than an oversight.
#[test]
fn a_generic_config_still_needs_the_manual_form() {
    let g = GraphBuilder::new();
    let count = g.ticker(PERIOD).count();

    // `fold`'s seed is generic: nothing recorded on its own.
    let bare = count.fold(0u64, |acc: &mut u64, v: &u64| *acc += v);
    assert_eq!(None, bare.cfg_src(), "a generic seed is not auto-recorded");

    // Recording it is explicit, and still works.
    let recorded = count
        .fold(0u64, |acc: &mut u64, v: &u64| *acc += v)
        .with_cfg(&0u64);
    assert_eq!(Some("0u64"), recorded.cfg_src().as_deref());
}

/// Recording is inert: an op that now renders its config must compute exactly
/// what it did before.
#[test]
fn auto_recording_does_not_change_behaviour() {
    let g = GraphBuilder::new();
    let out = g.ticker(PERIOD).count().limit(3).accumulate();
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1u64, 2, 3], runner.value(out));
}
