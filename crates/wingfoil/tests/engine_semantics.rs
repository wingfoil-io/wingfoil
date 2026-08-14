//! Semantic regression tests recording the *legacy* wingfoil engine's
//! observable behaviour: for each of these graphs the engine must produce the
//! tick times, values and run-bound handling that legacy did. Each test names
//! the legacy test it mirrors, and every expectation is a pinned constant
//! captured from that engine — so they outlived the tree they came from.
//! Wired through the fluent layer, which also exercises the underlying
//! `Builder`.

use std::time::Duration;

use wingfoil::op::{Activation, Op};
use wingfoil::ops::{Map, Ticker};
use wingfoil::prelude::*;

use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// Mirrors `long_delay_works` in the legacy engine (delay.rs): a 10ns
/// ticker counted then delayed 100ns, run for 120ns, emits [1, 2, 3, 4].
#[test]
fn delay_matches_legacy_engine() {
    let g = GraphBuilder::new();
    let acc = g
        .ticker(Duration::from_nanos(10))
        .count()
        .delay(Duration::from_nanos(100))
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(Duration::from_nanos(120)))
        .unwrap();
    assert_eq!(vec![1, 2, 3, 4], r.value(&acc));
}

/// Mirrors the legacy `constant` + `sample` behaviour: a constant ticks
/// once; sampling it on a ticker re-emits it each trigger tick.
#[test]
fn constant_and_sample_match_legacy_engine() {
    let g = GraphBuilder::new();
    let tick = g.ticker(Duration::from_nanos(100));
    let acc = g.constant(7u64).sample(&tick).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![7, 7, 7], r.value(&acc));
}

/// Filter suppresses quiet cycles: only even counts pass.
#[test]
fn filter_suppresses_like_legacy_engine() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(100)).count();
    let is_even = count.map(|i| i.is_multiple_of(2));
    let acc = count.filter(&is_even).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    assert_eq!(vec![2, 4, 6], r.value(&acc));
}

/// Join combines the current values of both inputs whenever either ticks.
#[test]
fn join_combines_current_values() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(100)).count();
    let doubled = count.map(|i| i * 2);
    let acc = count.join(&doubled, |a, b| a + b).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![3, 6, 9], r.value(&acc));
}

/// An external source fed from another thread wakes the realtime kernel and
/// emits bursts — every value, grouped, never coalesced. With a generous
/// bound all five arrive, in order (values landing between cycles ride one
/// burst rather than being dropped latest-wins).
#[test]
fn external_source_ticks_the_graph() {
    let g = GraphBuilder::new();
    let (values, source) = g.external::<u64>();
    let acc = values.collapse_accumulate();
    let mut r = g.build();
    let producer = std::thread::spawn(move || {
        for i in 1..=5 {
            source.send(i);
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    r.run(RunMode::RealTime, RunFor::Cycles(50)).unwrap();
    producer.join().expect("producer thread");
    let got = r.value(&acc);
    assert_eq!((1..=5).collect::<Vec<u64>>(), got, "all values, in order");
}

/// A sink runs its side effect once per source tick, in tick order.
#[test]
fn for_each_observes_every_tick() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(100)).count();
    let _done = count.for_each(move |v| {
        sink.borrow_mut().push(*v);
        Ok(())
    });
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1, 2, 3], *seen.borrow());
}

/// What the legacy engine counts for this graph, **captured from it** on
/// 2026-08-12 while it was still here to ask.
const LEGACY_DURATION_BOUND_COUNT: u64 = 6;

/// The duration bound must terminate exactly like the legacy engine's —
/// both engines run the same trailing-cycle semantics (a 100ns ticker under
/// a 305ns bound runs cycles at 0..=500: the bound is checked against the
/// *previous* cycle's time, then one marked-last cycle still runs).
///
/// **The expectation is pinned, not just compared.** This was a pairwise
/// `assert_eq!(legacy, wingfoil)`, which has two problems: it passes if both
/// engines drift the same way, and it cannot outlive the oracle — so the
/// cutover runbook had the whole file down as a deletion. Asserting against the
/// captured constant is strictly stronger *and* it is what let the legacy half
/// be lifted out with the tree. Every other test in this file already works
/// this way, citing the legacy test it mirrors.
#[test]
fn duration_bound_matches_legacy_engine() {
    let period = Duration::from_nanos(100);
    let bound = Duration::from_nanos(305);

    let g = GraphBuilder::new();
    let next = g.ticker(period).count();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(bound)).unwrap();

    assert_eq!(LEGACY_DURATION_BOUND_COUNT, r.value(&next));
}

/// The activation contract is `const`, so it can be checked at compile time
/// — the assertions below are evaluated by rustc, not at runtime. This is
/// what lets engines specialise on activation with zero cost.
#[test]
fn activation_is_declared_statically() {
    const {
        assert!(Ticker::ACTIVATION.schedules);
        assert!(!<Map<u64, bool, fn(&u64) -> bool> as Op>::ACTIVATION.callback_activated());
        assert!(matches!(
            <Map<u64, bool, fn(&u64) -> bool> as Op>::ACTIVATION,
            Activation {
                schedules: false,
                threaded: false,
                always: false,
            }
        ));
    }
}
