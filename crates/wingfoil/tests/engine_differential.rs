//! Differential engine parity: run the **legacy** op and its wingfoil twin over
//! the same scenario in the same process, and compare **values and tick times**
//! exactly — rather than against a hand-transcribed expectation.
//!
//! CLAUDE.md makes the legacy tree the parity oracle: "the wingfoil twin must
//! produce identical values and tick times". Most of the suite encodes that as
//! literals a person derived by reading the legacy node; this file has legacy
//! itself produce them, so the tick-time half of the claim is enforced rather
//! than assumed.
//!
//! `scheduling_ops_sweep` is the point of the harness: once the comparison is a
//! function, covering twelve periods and sixteen window sizes costs nothing, and
//! that is the range a transcribed expectation will never have.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

use legacy_wingfoil::{NodeOperators, StreamOperators};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_nanos(100);
const CYCLES: u32 = 25;

type Series = Vec<(u64, i64)>;

fn legacy_src() -> Rc<dyn legacy_wingfoil::Stream<i64>> {
    legacy_wingfoil::ticker(PERIOD)
        .count()
        .map(|n: u64| ((n * 7) % 13) as i64)
}

fn wingfoil_src(g: &GraphBuilder) -> wingfoil::fluent::Stream<i64> {
    g.ticker(PERIOD)
        .count()
        .map(|n: &u64| ((*n * 7) % 13) as i64)
}

fn legacy_run(
    build: impl Fn(&Rc<dyn legacy_wingfoil::Stream<i64>>) -> Rc<dyn legacy_wingfoil::Stream<i64>>,
) -> Series {
    let out = build(&legacy_src());
    let seen = Rc::new(RefCell::new(Vec::new()));
    let tap = {
        let seen = seen.clone();
        out.for_each(move |v, t| seen.borrow_mut().push((u64::from(t), v)))
    };
    tap.run(HISTORICAL, RunFor::Cycles(CYCLES))
        .expect("legacy run");
    seen.borrow().clone()
}

fn wingfoil_run(
    build: impl Fn(&wingfoil::fluent::Stream<i64>) -> wingfoil::fluent::Stream<i64>,
) -> Series {
    let g = GraphBuilder::new();
    let out = build(&wingfoil_src(&g));
    let acc = out.with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(CYCLES))
        .expect("wingfoil run");
    r.value(&acc)
        .into_iter()
        .map(|(t, v)| (u64::from(t), v))
        .collect()
}

fn compare(name: &str, legacy: Series, next: Series) {
    assert_eq!(
        legacy, next,
        "\n{name}: legacy and wingfoil differ (time, value) pairs\n  legacy  = {legacy:?}\n  wingfoil= {next:?}"
    );
}

#[test]
fn map_parity() {
    compare(
        "map",
        legacy_run(|s| s.map(|v: i64| v * 2)),
        wingfoil_run(|s| s.map(|v: &i64| v * 2)),
    );
}

#[test]
fn filter_value_parity() {
    compare(
        "filter_value / map_filter",
        legacy_run(|s| s.filter_value(|v: &i64| *v > 5)),
        wingfoil_run(|s| s.map_filter(|v: &i64| (*v, *v > 5))),
    );
}

#[test]
fn distinct_parity() {
    compare(
        "distinct",
        legacy_run(|s| s.distinct()),
        wingfoil_run(|s| s.distinct()),
    );
}

#[test]
fn difference_parity() {
    compare(
        "difference",
        legacy_run(|s| s.difference()),
        wingfoil_run(|s| s.difference()),
    );
}

#[test]
fn limit_parity() {
    compare(
        "limit(5)",
        legacy_run(|s| s.limit(5)),
        wingfoil_run(|s| s.limit(5)),
    );
}

#[test]
fn delay_parity() {
    let d = Duration::from_nanos(250);
    compare(
        "delay(250ns)",
        legacy_run(|s| s.delay(d)),
        wingfoil_run(|s| s.delay(d)),
    );
}

#[test]
fn throttle_parity() {
    let i = Duration::from_nanos(300);
    compare(
        "throttle(300ns)",
        legacy_run(|s| s.throttle(i)),
        wingfoil_run(|s| s.throttle(i)),
    );
}

#[test]
fn fold_parity() {
    compare(
        "fold (running sum)",
        legacy_run(|s| s.fold(|acc: &mut i64, v: i64| *acc += v)),
        wingfoil_run(|s| s.fold(0i64, |acc: &mut i64, v: &i64| *acc += *v)),
    );
}

#[test]
fn sample_parity() {
    let trigger = Duration::from_nanos(300);
    compare(
        "sample(300ns ticker)",
        legacy_run(|s| s.sample(legacy_wingfoil::ticker(trigger))),
        wingfoil_run(|s| {
            let g = s.graph();
            s.sample(&g.ticker(trigger))
        }),
    );
}

#[test]
fn window_parity() {
    let w = Duration::from_nanos(300);
    compare(
        "window(300ns) -> summed",
        legacy_run(|s| s.window(w).map(|v: Vec<i64>| v.iter().sum::<i64>())),
        wingfoil_run(|s| s.window(w).map(|v: &Vec<i64>| v.iter().sum::<i64>())),
    );
}

#[test]
fn window_len_parity() {
    let w = Duration::from_nanos(300);
    compare(
        "window(300ns) -> len",
        legacy_run(|s| s.window(w).map(|v: Vec<i64>| v.len() as i64)),
        wingfoil_run(|s| s.window(w).map(|v: &Vec<i64>| v.len() as i64)),
    );
}

#[test]
fn buffer_parity() {
    compare(
        "buffer(4) -> summed",
        legacy_run(|s| s.buffer(4).map(|v: Vec<i64>| v.iter().sum::<i64>())),
        wingfoil_run(|s| s.buffer(4).map(|v: &Vec<i64>| v.iter().sum::<i64>())),
    );
}

#[test]
fn buffer_len_parity() {
    compare(
        "buffer(4) -> len",
        legacy_run(|s| s.buffer(4).map(|v: Vec<i64>| v.len() as i64)),
        wingfoil_run(|s| s.buffer(4).map(|v: &Vec<i64>| v.len() as i64)),
    );
}

#[test]
fn merge_tiebreak_parity() {
    // Two tickers at different periods that coincide every 3rd/2nd tick — the
    // merge tie-break at a shared instant is the interesting part.
    compare(
        "merge(200ns, 300ns)",
        {
            let a = legacy_wingfoil::ticker(Duration::from_nanos(200))
                .count()
                .map(|n: u64| n as i64);
            let b = legacy_wingfoil::ticker(Duration::from_nanos(300))
                .count()
                .map(|n: u64| -(n as i64));
            let out = legacy_wingfoil::merge(vec![a, b]);
            let seen = Rc::new(RefCell::new(Vec::new()));
            let tap = {
                let seen = seen.clone();
                out.for_each(move |v, t| seen.borrow_mut().push((u64::from(t), v)))
            };
            tap.run(HISTORICAL, RunFor::Cycles(CYCLES)).expect("legacy");
            seen.borrow().clone()
        },
        {
            let g = GraphBuilder::new();
            let a = g
                .ticker(Duration::from_nanos(200))
                .count()
                .map(|n: &u64| *n as i64);
            let b = g
                .ticker(Duration::from_nanos(300))
                .count()
                .map(|n: &u64| -(*n as i64));
            let out = a.merge(&b);
            let acc = out.with_time().accumulate();
            let mut r = g.build();
            r.run(HISTORICAL, RunFor::Cycles(CYCLES)).expect("wingfoil");
            let out: Series = r
                .value(&acc)
                .into_iter()
                .map(|(t, v)| (u64::from(t), v))
                .collect();
            out
        },
    );
}

/// Parameter sweep: rather than one value per op, run every scheduling op over
/// a range of periods against the legacy engine and compare (time, value)
/// series exactly. This is where a tick-time deviation would hide.
#[test]
fn scheduling_ops_sweep() {
    let mut failures: Vec<String> = Vec::new();
    for ns in [1u64, 50, 99, 100, 101, 150, 200, 250, 300, 700, 1000, 2500] {
        let d = Duration::from_nanos(ns);

        let l = legacy_run(|s| s.delay(d));
        let w = wingfoil_run(|s| s.delay(d));
        if l != w {
            failures.push(format!("delay({ns}ns)\n  legacy  ={l:?}\n  wingfoil={w:?}"));
        }

        let l = legacy_run(|s| s.throttle(d));
        let w = wingfoil_run(|s| s.throttle(d));
        if l != w {
            failures.push(format!(
                "throttle({ns}ns)\n  legacy  ={l:?}\n  wingfoil={w:?}"
            ));
        }

        let l = legacy_run(|s| s.window(d).map(|v: Vec<i64>| v.len() as i64));
        let w = wingfoil_run(|s| s.window(d).map(|v: &Vec<i64>| v.len() as i64));
        if l != w {
            failures.push(format!(
                "window({ns}ns).len\n  legacy  ={l:?}\n  wingfoil={w:?}"
            ));
        }

        let l = legacy_run(|s| s.sample(legacy_wingfoil::ticker(d)));
        let w = wingfoil_run(|s| {
            let g = s.graph();
            s.sample(&g.ticker(d))
        });
        if l != w {
            failures.push(format!(
                "sample({ns}ns)\n  legacy  ={l:?}\n  wingfoil={w:?}"
            ));
        }
    }
    for n in [0usize, 1, 2, 3, 7, 24, 25, 26, 100] {
        let l = legacy_run(|s| s.buffer(n).map(|v: Vec<i64>| v.len() as i64));
        let w = wingfoil_run(|s| s.buffer(n).map(|v: &Vec<i64>| v.len() as i64));
        if l != w {
            failures.push(format!(
                "buffer({n}).len\n  legacy  ={l:?}\n  wingfoil={w:?}"
            ));
        }
    }
    for n in [0u32, 1, 2, 24, 25, 26, 100] {
        let l = legacy_run(|s| s.limit(n));
        let w = wingfoil_run(|s| s.limit(n));
        if l != w {
            failures.push(format!("limit({n})\n  legacy  ={l:?}\n  wingfoil={w:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} divergence(s) from the legacy engine:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn harness_is_not_vacuous() {
    let base = legacy_run(|s| s.map(|v: i64| v));
    println!("base series ({} pts) = {base:?}", base.len());
    assert!(base.len() >= 20, "source series too short: {}", base.len());

    let d = Duration::from_nanos(250);
    for (name, l, w) in [
        (
            "delay",
            legacy_run(|s| s.delay(d)),
            wingfoil_run(|s| s.delay(d)),
        ),
        (
            "throttle",
            legacy_run(|s| s.throttle(d)),
            wingfoil_run(|s| s.throttle(d)),
        ),
        (
            "window.len",
            legacy_run(|s| s.window(d).map(|v: Vec<i64>| v.len() as i64)),
            wingfoil_run(|s| s.window(d).map(|v: &Vec<i64>| v.len() as i64)),
        ),
    ] {
        println!("{name}: legacy {} pts {:?}", l.len(), &l[..l.len().min(6)]);
        assert!(
            !l.is_empty(),
            "{name}: legacy series empty — harness vacuous"
        );
        assert!(
            !w.is_empty(),
            "{name}: wingfoil series empty — harness vacuous"
        );
    }
}
