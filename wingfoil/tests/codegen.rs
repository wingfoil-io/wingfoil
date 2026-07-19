//! End-to-end tests for `wingfoil::codegen`: the checked-in generated runners
//! in `tests/generated/` must stay in sync with the generators (golden tests)
//! and must produce results identical to the interpreted engine (parity
//! tests). This proves generated output compiles and runs — not just that the
//! generators emit plausible text.
//!
//! Three runners are generated from the same odds/evens wiring:
//! - `odds_evens.rs` — inline mode (static dispatch via typed handles)
//! - `odds_evens_dispatch.rs` — dynamic-dispatch mode (`inline: false`)
//! - `odds_evens_standalone.rs` — fully monomorphized standalone runner
//!
//! Plus `delayed.rs`, an inline-mode runner for a graph with state-needing
//! nodes (`ticker`/`delay` via `cycle_typed` static dispatch).

use std::rc::Rc;
use std::time::Duration;
use wingfoil::codegen::{CodegenOptions, generate, generate_standalone};
use wingfoil::*;

#[path = "generated/odds_evens.rs"]
mod odds_evens_inline;

#[path = "generated/odds_evens_dispatch.rs"]
mod odds_evens_dispatch;

#[path = "generated/odds_evens_standalone.rs"]
mod odds_evens_standalone;

#[path = "generated/delayed.rs"]
mod delayed_generated;

const GOLDEN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/generated");

/// The odds/evens graph from the crate docs, with an accumulator instead of
/// `print` so results can be asserted. The golden files and the parity tests
/// are all built from this exact wiring.
fn wire() -> (Vec<Rc<dyn Node>>, Rc<dyn Stream<Vec<String>>>) {
    let period = Duration::from_millis(10);
    let source = ticker(period).count();
    let is_even = source.map(|i| i.is_multiple_of(2));
    let odds = source.filter(is_even.not()).map(|i| format!("{i} is odd"));
    let evens = source.filter(is_even).map(|i| format!("{i} is even"));
    let acc = merge(vec![odds, evens]).accumulate();
    (vec![acc.clone().as_node()], acc)
}

/// The standalone runner's `Inputs`: the same closures/configuration the
/// wiring above bakes into its nodes, in node-index order.
fn standalone_inputs() -> odds_evens_standalone::Inputs<
    impl FnMut(&mut u64, u64),
    impl FnMut(u64) -> bool,
    impl FnMut(bool) -> bool,
    impl FnMut(u64) -> String,
    impl FnMut(u64) -> String,
    impl FnMut(&mut Vec<String>, String),
> {
    odds_evens_standalone::Inputs {
        tick_0: NanoTime::from(Duration::from_millis(10)),
        constant_1: 1,
        fold_3: |acc: &mut u64, v: u64| *acc += v,
        map_4: |i: u64| i.is_multiple_of(2),
        map_5: |b: bool| !b,
        map_7: |i: u64| format!("{i} is odd"),
        map_9: |i: u64| format!("{i} is even"),
        fold_11: |acc: &mut Vec<String>, v: String| acc.push(v),
    }
}

fn check_golden(file: &str, generated: String) {
    let path = format!("{GOLDEN_DIR}/{file}");
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &generated).unwrap();
    }
    let golden = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        golden, generated,
        "tests/generated/{file} is stale — refresh with \
         `UPDATE_GOLDEN=1 cargo test -p wingfoil --test codegen golden` and re-run"
    );
}

#[test]
fn golden_inline_runner_is_up_to_date() {
    let (roots, _) = wire();
    check_golden(
        "odds_evens.rs",
        generate(roots, &CodegenOptions::default()).unwrap(),
    );
}

#[test]
fn golden_dispatch_runner_is_up_to_date() {
    let (roots, _) = wire();
    let opts = CodegenOptions {
        inline: false,
        ..Default::default()
    };
    check_golden("odds_evens_dispatch.rs", generate(roots, &opts).unwrap());
}

/// A graph whose interesting nodes need `GraphState` — `delay` schedules
/// itself at t+delay and checks its upstream's tick, `ticker` drives the
/// clock. Both run through `cycle_typed` (static dispatch, real state) in the
/// generated inline runner.
fn wire_delayed() -> (Vec<Rc<dyn Node>>, Rc<dyn Stream<Vec<u64>>>) {
    let acc = ticker(Duration::from_nanos(10))
        .count()
        .delay(Duration::from_nanos(100))
        .accumulate();
    (vec![acc.clone().as_node()], acc)
}

#[test]
fn golden_delayed_runner_is_up_to_date() {
    let (roots, _) = wire_delayed();
    check_golden(
        "delayed.rs",
        generate(roots, &CodegenOptions::default()).unwrap(),
    );
}

#[test]
fn delayed_runner_matches_interpreted_engine() {
    let run_mode = RunMode::HistoricalFrom(NanoTime::ZERO);
    let run_for = RunFor::Duration(Duration::from_nanos(120));

    let (roots, values) = wire_delayed();
    Graph::new(roots, run_mode, run_for).run().unwrap();
    let expected = values.peek_value();
    // Delayed emissions land from t=100 (see delay.rs long_delay_works).
    assert_eq!(vec![1, 2, 3, 4], expected);

    let (roots, values) = wire_delayed();
    delayed_generated::run(roots, run_mode, run_for).unwrap();
    assert_eq!(expected, values.peek_value());
}

#[test]
fn golden_standalone_runner_is_up_to_date() {
    let (roots, _) = wire();
    check_golden(
        "odds_evens_standalone.rs",
        generate_standalone(roots, "run").unwrap(),
    );
}

/// Expected output from the interpreted engine — the reference for all three
/// generated runners.
fn interpreted_expected() -> Vec<String> {
    let (roots, values) = wire();
    Graph::new(
        roots,
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(12),
    )
    .run()
    .unwrap();
    let expected = values.peek_value();
    assert!(!expected.is_empty());
    assert_eq!("1 is odd", expected[0]);
    assert_eq!("2 is even", expected[1]);
    expected
}

#[test]
fn inline_runner_matches_interpreted_engine() {
    let expected = interpreted_expected();
    let (roots, values) = wire();
    odds_evens_inline::run(
        roots,
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(12),
    )
    .unwrap();
    assert_eq!(expected, values.peek_value());
}

#[test]
fn dispatch_runner_matches_interpreted_engine() {
    let expected = interpreted_expected();
    let (roots, values) = wire();
    odds_evens_dispatch::run(
        roots,
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(12),
    )
    .unwrap();
    assert_eq!(expected, values.peek_value());
}

#[test]
fn standalone_runner_matches_interpreted_engine() {
    let expected = interpreted_expected();
    let (out,) = odds_evens_standalone::run(
        standalone_inputs(),
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(12),
    );
    assert_eq!(expected, out);
}

#[test]
fn inline_runner_works_in_realtime() {
    let (roots, values) = wire();
    odds_evens_inline::run(roots, RunMode::RealTime, RunFor::Cycles(4)).unwrap();
    assert!(!values.peek_value().is_empty());
}

#[test]
fn standalone_runner_works_in_realtime() {
    let (out,) =
        odds_evens_standalone::run(standalone_inputs(), RunMode::RealTime, RunFor::Cycles(4));
    assert!(!out.is_empty());
}

#[test]
fn inline_runner_rejects_changed_wiring() {
    // A different graph fed to the same runner must trip the topology guard.
    let other = ticker(Duration::from_millis(1)).count();
    let err = odds_evens_inline::run(
        vec![other.as_node()],
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(1),
    )
    .expect_err("changed wiring must be rejected");
    assert!(format!("{err:#}").contains("regenerate"));
}

#[test]
fn standalone_generator_rejects_unsupported_nodes() {
    // delay is not in the standalone-supported kind set.
    let root = ticker(Duration::from_millis(1))
        .count()
        .delay(Duration::from_millis(1))
        .as_node();
    let err = generate_standalone(vec![root], "run")
        .expect_err("delay must be rejected by generate_standalone");
    let msg = format!("{err:#}");
    assert!(msg.contains("DelayStream"), "unexpected error: {msg}");
}
