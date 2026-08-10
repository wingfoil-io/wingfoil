//! **`#[op(emit_cfg)]`**: data configs record themselves.
//!
//! A closure is *erased*, so recovering one needs a call-site macro. A data
//! config is not erased at all — the value sits in the op's `Cfg` cell — it
//! just never reached the node's type-free metadata. That is plumbing, so it
//! needs no user vocabulary.

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(1);

/// Ordinary wiring, config recorded.
#[test]
fn a_concrete_config_is_recorded_without_annotation() {
    let g = GraphBuilder::new();
    let ticks = g.ticker(PERIOD);
    assert_eq!(
        Some("::core::time::Duration::new(0u64, 1000000u32)"),
        ticks.cfg_src().as_deref()
    );
}

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

/// **The line the bound draws.** `EmitLiteral` lands on the op's *public*
/// signature, so it can only go on ops whose config type is concrete. Requiring
/// it of `fold`'s generic seed would forbid folding into any accumulator the
/// generator cannot render — a breaking change for users not buying generation.
/// Pinned so the asymmetry reads as a decision rather than an oversight.
#[test]
fn a_generic_config_still_needs_the_manual_form() {
    let g = GraphBuilder::new();
    let count = g.ticker(PERIOD).count();

    let bare = count.fold(0u64, |acc: &mut u64, v: &u64| *acc += v);
    assert_eq!(None, bare.cfg_src(), "a generic seed is not auto-recorded");

    let recorded = count
        .fold(0u64, |acc: &mut u64, v: &u64| *acc += v)
        .with_cfg(&0u64);
    assert_eq!(Some("0u64"), recorded.cfg_src().as_deref());
}

/// Recording is inert.
#[test]
fn auto_recording_does_not_change_behaviour() {
    let g = GraphBuilder::new();
    let out = g.ticker(PERIOD).count().limit(3).accumulate();
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(5)).unwrap();
    assert_eq!(vec![1u64, 2, 3], runner.value(out));
}
