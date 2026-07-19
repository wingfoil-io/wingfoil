//! End-to-end tests for `wingfoil::codegen`: the checked-in generated runner
//! in `tests/generated/odds_evens.rs` must stay in sync with the generator
//! (golden test) and must produce results identical to the interpreted engine
//! (parity tests). This proves generated output compiles and runs — not just
//! that the generator emits plausible text.

use std::rc::Rc;
use std::time::Duration;
use wingfoil::codegen::{CodegenOptions, generate};
use wingfoil::*;

#[path = "generated/odds_evens.rs"]
mod odds_evens_generated;

const GOLDEN_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/generated/odds_evens.rs");

/// The odds/evens graph from the crate docs, with an accumulator instead of
/// `print` so results can be asserted. Both the golden file and the parity
/// tests are built from this exact wiring.
fn wire() -> (Vec<Rc<dyn Node>>, Rc<dyn Stream<Vec<String>>>) {
    let period = Duration::from_millis(10);
    let source = ticker(period).count();
    let is_even = source.map(|i| i % 2 == 0);
    let odds = source.filter(is_even.not()).map(|i| format!("{i} is odd"));
    let evens = source.filter(is_even).map(|i| format!("{i} is even"));
    let acc = merge(vec![odds, evens]).accumulate();
    (vec![acc.clone().as_node()], acc)
}

#[test]
fn golden_generated_runner_is_up_to_date() {
    let (roots, _) = wire();
    let src = generate(roots, &CodegenOptions::default()).unwrap();
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(GOLDEN_PATH, &src).unwrap();
    }
    assert_eq!(
        include_str!("generated/odds_evens.rs"),
        src,
        "tests/generated/odds_evens.rs is stale — refresh it with \
         `UPDATE_GOLDEN=1 cargo test -p wingfoil --test codegen` and re-run"
    );
}

#[test]
fn generated_runner_matches_interpreted_engine() {
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

    let (roots, values) = wire();
    odds_evens_generated::run(
        roots,
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(12),
    )
    .unwrap();
    assert_eq!(expected, values.peek_value());
}

#[test]
fn generated_runner_works_in_realtime() {
    let (roots, values) = wire();
    odds_evens_generated::run(roots, RunMode::RealTime, RunFor::Cycles(4)).unwrap();
    assert!(!values.peek_value().is_empty());
}

#[test]
fn generated_runner_rejects_changed_wiring() {
    // A different graph fed to the same runner must trip the topology guard.
    let other = ticker(Duration::from_millis(1)).count();
    let err = odds_evens_generated::run(
        vec![other.as_node()],
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(1),
    )
    .expect_err("changed wiring must be rejected");
    assert!(format!("{err:#}").contains("regenerate"));
}
