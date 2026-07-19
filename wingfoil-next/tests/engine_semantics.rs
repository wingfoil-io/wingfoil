//! Semantic cross-validation: the prototype engines must reproduce the
//! *classic* wingfoil engine's observable behaviour for equivalent graphs —
//! same tick times, same values, same run-bound handling. Wired through the
//! fluent layer, which also exercises the underlying `Builder`.

use std::time::Duration;

use wingfoil_next::fluent::GraphBuilder;
use wingfoil_next::op::{Caps, Op};
use wingfoil_next::ops::{Map, Ticker};

use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// Mirrors `long_delay_works` in the classic engine (delay.rs): a 10ns
/// ticker counted then delayed 100ns, run for 120ns, emits [1, 2, 3, 4].
#[test]
fn delay_matches_classic_engine() {
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

/// Mirrors the classic `constant` + `sample` behaviour: a constant ticks
/// once; sampling it on a ticker re-emits it each trigger tick.
#[test]
fn constant_and_sample_match_classic_engine() {
    let g = GraphBuilder::new();
    let tick = g.ticker(Duration::from_nanos(100));
    let acc = g.constant(7u64).sample(&tick).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![7, 7, 7], r.value(&acc));
}

/// Filter suppresses quiet cycles: only even counts pass.
#[test]
fn filter_suppresses_like_classic_engine() {
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

/// An external source fed from another thread wakes the realtime kernel;
/// the run terminates once all producers are gone. Sends that land between
/// cycles coalesce (latest wins), so under scheduler load fewer than five
/// cycles may fire — the assertions accept any coalescing but require the
/// values that do arrive to be in order and to include the final send.
#[test]
fn external_source_ticks_the_graph() {
    let g = GraphBuilder::new();
    let (values, source) = g.external::<u64>();
    let acc = values.accumulate();
    let mut r = g.build();
    let producer = std::thread::spawn(move || {
        for i in 1..=5 {
            source.send(i);
            // Space sends out so each usually arrives in its own cycle.
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    r.run(RunMode::RealTime, RunFor::Cycles(5)).unwrap();
    producer.join().expect("producer thread");
    let got = r.value(&acc);
    assert!(!got.is_empty(), "at least one send must arrive");
    assert!(got.windows(2).all(|w| w[0] < w[1]), "in order: {got:?}");
    assert_eq!(Some(&5), got.last(), "final send must arrive: {got:?}");
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

/// The duration bound must terminate exactly like the classic engine's —
/// both engines run the same trailing-cycle semantics (a 100ns ticker under
/// a 305ns bound runs cycles at 0..=500: the bound is checked against the
/// *previous* cycle's time, then one marked-last cycle still runs).
#[test]
fn duration_bound_matches_classic_engine() {
    use wingfoil::NodeOperators;
    let period = Duration::from_nanos(100);
    let bound = Duration::from_nanos(305);

    let classic = wingfoil::ticker(period).count();
    classic
        .run(HISTORICAL, RunFor::Duration(bound))
        .expect("classic run");

    let g = GraphBuilder::new();
    let next = g.ticker(period).count();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Duration(bound)).unwrap();

    assert_eq!(classic.peek_value(), r.value(&next));
}

/// The capability contract is `const`, so it can be checked at compile time
/// — the assertions below are evaluated by rustc, not at runtime. This is
/// what lets engines specialise on capabilities with zero cost.
#[test]
fn caps_are_declared_statically() {
    const {
        assert!(Ticker::CAPS.schedules);
        assert!(!<Map<u64, bool, fn(&u64) -> bool> as Op>::CAPS.callback_activated());
        assert!(matches!(
            <Map<u64, bool, fn(&u64) -> bool> as Op>::CAPS,
            Caps {
                schedules: false,
                threaded: false,
                always: false,
            }
        ));
    }
}
