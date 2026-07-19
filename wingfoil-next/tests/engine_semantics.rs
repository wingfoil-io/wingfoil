//! Semantic cross-validation: the prototype engines must reproduce the
//! *classic* wingfoil engine's observable behaviour for equivalent graphs —
//! same tick times, same values, same run-bound handling.

use std::time::Duration;

use wingfoil_next::interp::Builder;
use wingfoil_next::op::{Caps, Op};
use wingfoil_next::ops::{Map, Ticker};

use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// Mirrors `long_delay_works` in the classic engine (delay.rs): a 10ns
/// ticker counted then delayed 100ns, run for 120ns, emits [1, 2, 3, 4].
#[test]
fn delay_matches_classic_engine() {
    let mut g = Builder::new();
    let tick = g.ticker(Duration::from_nanos(10));
    let count = g.fold(tick, 0u64, |acc, _: &()| *acc += 1);
    let delayed = g.delay(count, Duration::from_nanos(100));
    let acc = g.fold(delayed, Vec::new(), |acc: &mut Vec<u64>, v: &u64| {
        acc.push(*v)
    });
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(Duration::from_nanos(120)));
    assert_eq!(vec![1, 2, 3, 4], r.value(acc));
}

/// Mirrors the classic `constant` + `sample` behaviour: a constant ticks
/// once; sampling it on a ticker re-emits it each trigger tick.
#[test]
fn constant_and_sample_match_classic_engine() {
    let mut g = Builder::new();
    let tick = g.ticker(Duration::from_nanos(100));
    let c = g.constant(7u64);
    let sampled = g.sample(c, tick);
    let acc = g.fold(sampled, Vec::new(), |acc: &mut Vec<u64>, v: &u64| {
        acc.push(*v)
    });
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3));
    assert_eq!(vec![7, 7, 7], r.value(acc));
}

/// Filter suppresses quiet cycles: only even counts pass.
#[test]
fn filter_suppresses_like_classic_engine() {
    let mut g = Builder::new();
    let tick = g.ticker(Duration::from_nanos(100));
    let count = g.fold(tick, 0u64, |acc, _: &()| *acc += 1);
    let is_even = g.map(count, |i: &u64| i.is_multiple_of(2));
    let evens = g.filter(count, is_even);
    let acc = g.fold(evens, Vec::new(), |acc: &mut Vec<u64>, v: &u64| {
        acc.push(*v)
    });
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6));
    assert_eq!(vec![2, 4, 6], r.value(acc));
}

/// The capability contract is `const`, so it can be checked at compile time
/// — the assertions below are evaluated by rustc, not at runtime. This is
/// what lets engines specialise on capabilities with zero cost.
#[test]
fn caps_are_declared_statically() {
    const {
        assert!(Ticker::CAPS.schedules);
        assert!(!<Map<u64, bool, fn(&u64) -> bool> as Op>::CAPS.schedules);
        assert!(matches!(
            <Map<u64, bool, fn(&u64) -> bool> as Op>::CAPS,
            Caps { schedules: false }
        ));
    }
}
