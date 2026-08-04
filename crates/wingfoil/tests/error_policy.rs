//! Per-node error containment: `Stream::on_error` and [`ErrorAction`].
//!
//! Without a handler, any node's `cycle` error aborts the whole run — the
//! engine's long-standing behaviour, and the right one for a backtest. These
//! tests pin the opt-in alternatives, which exist for the case the default
//! serves badly: a live graph where one leg's decode failure should not take
//! down the leg managing risk on another.
//!
//! Everything here runs `HistoricalFrom(ZERO)` and asserts **values and tick
//! times**, so a containment bug that showed up as a shifted schedule rather
//! than a wrong value would still be caught.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::bail;
use wingfoil::interp::{ErrorAction, NodeError};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const STEP: Duration = Duration::from_nanos(100);

/// A counter stream that fails on the counts in `fail_on`.
fn failing_counter(g: &GraphBuilder, fail_on: &'static [u64]) -> Stream<u64> {
    g.ticker(STEP).count().try_map(move |n: &u64| {
        if fail_on.contains(n) {
            bail!("synthetic failure at {n}");
        }
        Ok(*n)
    })
}

/// The default is unchanged: no handler, so the first error aborts the run and
/// is reported with node context.
#[test]
fn without_a_handler_an_error_still_aborts_the_run() {
    let g = GraphBuilder::new();
    let out = failing_counter(&g, &[3]).accumulate();
    let mut r = g.build();

    let err = r
        .run(HISTORICAL, RunFor::Cycles(6))
        .expect_err("the run must abort");
    let chain = format!("{err:#}");
    assert!(
        chain.contains("cycle") && chain.contains("synthetic failure at 3"),
        "error lost its node context: {chain}"
    );
    // Everything before the failure still reached the accumulator.
    assert_eq!(vec![1, 2], r.value(&out));
}

/// `SkipCycle` treats the failing cycle as `Tick::Quiet`: the value is dropped,
/// the node stays live, and the next activation is cycled normally.
#[test]
fn skip_cycle_drops_the_value_and_keeps_the_node_live() {
    let seen: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let recorder = seen.clone();

    let g = GraphBuilder::new();
    let out = failing_counter(&g, &[2, 4])
        .on_error(move |e: &NodeError<'_>| {
            recorder.borrow_mut().push(e.index);
            ErrorAction::SkipCycle
        })
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).expect("run completes");

    // Counts 2 and 4 are dropped; 5 and 6 prove the node was still cycled
    // after two failures.
    let got = r.value(&out);
    let values: Vec<u64> = got.iter().map(|(_, v)| *v).collect();
    assert_eq!(vec![1, 3, 5, 6], values);

    // And the surviving ticks kept their original times — a dropped cycle must
    // not reschedule the ones after it.
    let times: Vec<NanoTime> = got.iter().map(|(t, _)| *t).collect();
    assert_eq!(
        vec![
            NanoTime::new(0),
            NanoTime::new(200),
            NanoTime::new(400),
            NanoTime::new(500),
        ],
        times
    );

    // The handler was called once per failure, naming the same node both times.
    let calls = seen.borrow();
    assert_eq!(2, calls.len());
    assert_eq!(calls[0], calls[1]);
}

/// `Quarantine` retires the node: it is never cycled again, so it never ticks
/// and its downstream goes quiet — while the rest of the graph runs on.
#[test]
fn quarantine_silences_one_branch_and_leaves_the_rest_running() {
    let calls = Rc::new(RefCell::new(0usize));
    let counted = calls.clone();

    let g = GraphBuilder::new();
    let ticks = g.ticker(STEP).count();

    // The branch that fails, quarantined on its first error.
    let risky = ticks
        .try_map(|n: &u64| {
            if *n == 3 {
                bail!("this leg is done");
            }
            Ok(*n)
        })
        .on_error(move |_: &NodeError<'_>| {
            *counted.borrow_mut() += 1;
            ErrorAction::Quarantine
        });
    let risky_out = risky.with_time().accumulate();

    // A sibling branch off the same source, which must be untouched.
    let healthy_out = ticks.map(|n: &u64| n * 10).with_time().accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).expect("run completes");

    // The risky branch stops at its failure and never resumes.
    let risky_vals: Vec<u64> = r.value(&risky_out).iter().map(|(_, v)| *v).collect();
    assert_eq!(vec![1, 2], risky_vals);

    // The healthy branch ran the whole way, on the original schedule.
    let healthy = r.value(&healthy_out);
    assert_eq!(
        vec![10, 20, 30, 40, 50, 60],
        healthy.iter().map(|(_, v)| *v).collect::<Vec<u64>>()
    );
    assert_eq!(
        vec![
            NanoTime::new(0),
            NanoTime::new(100),
            NanoTime::new(200),
            NanoTime::new(300),
            NanoTime::new(400),
            NanoTime::new(500),
        ],
        healthy.iter().map(|(t, _)| *t).collect::<Vec<NanoTime>>()
    );

    // Quarantined means quarantined: the handler fired exactly once, not once
    // per remaining activation.
    assert_eq!(1, *calls.borrow());
}

/// A handler that returns `Abort` is the explicit spelling of the default, and
/// still gets to see the error on the way past — the shape you want for
/// "log it, then die".
#[test]
fn a_handler_can_still_choose_to_abort() {
    let saw: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let recorder = saw.clone();

    let g = GraphBuilder::new();
    let _out = failing_counter(&g, &[2]).on_error(move |e: &NodeError<'_>| {
        *recorder.borrow_mut() = Some(format!("{} @ {}", e.error, e.label));
        ErrorAction::Abort
    });
    let mut r = g.build();

    r.run(HISTORICAL, RunFor::Cycles(6))
        .expect_err("Abort still aborts");
    let seen = saw.borrow();
    let seen = seen.as_ref().expect("the handler ran before aborting");
    assert!(seen.starts_with("synthetic failure at 2 @ "));
    assert!(!seen.ends_with("@ "), "the node label was empty: {seen}");
}

/// One handler, many nodes — the kill-switch shape. `ErrorHandler` is an `Rc`
/// precisely so a single closure can serve a whole graph and still tell which
/// node spoke.
#[test]
fn one_handler_can_serve_several_nodes_and_still_identify_them() {
    let seen: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    let g = GraphBuilder::new();
    let ticks = g.ticker(STEP).count();

    let mut outs = Vec::new();
    for modulus in [2u64, 3] {
        let recorder = seen.clone();
        outs.push(
            ticks
                .try_map(move |n: &u64| {
                    if n.is_multiple_of(modulus) {
                        bail!("mod {modulus}");
                    }
                    Ok(*n)
                })
                .on_error(move |e: &NodeError<'_>| {
                    recorder.borrow_mut().push(e.index);
                    ErrorAction::SkipCycle
                })
                .accumulate(),
        );
    }

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(6)).expect("run completes");

    assert_eq!(vec![1, 3, 5], r.value(&outs[0]));
    assert_eq!(vec![1, 2, 4, 5], r.value(&outs[1]));

    // Two distinct node indices reported through the one closure.
    let calls = seen.borrow();
    let mut distinct: Vec<usize> = calls.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(2, distinct.len(), "both nodes reported through one handler");
}

/// A quarantine belongs to the run that imposed it. A re-run of a re-runnable
/// graph starts clean, exactly as node state does.
#[test]
fn a_re_run_clears_a_quarantine() {
    let g = GraphBuilder::new();
    let out = g
        .ticker(STEP)
        .count()
        .try_map(|n: &u64| {
            if *n == 2 {
                bail!("boom");
            }
            Ok(*n)
        })
        .on_error(|_: &NodeError<'_>| ErrorAction::Quarantine)
        .accumulate();
    let mut r = g.build();

    r.run(HISTORICAL, RunFor::Cycles(5)).expect("first run");
    assert_eq!(vec![1], r.value(&out));

    r.run(HISTORICAL, RunFor::Cycles(5)).expect("second run");
    assert_eq!(
        vec![1],
        r.value(&out),
        "the second run must reproduce the first exactly, not start quarantined"
    );
}

/// A **sink** is containable too, which is the case that motivated all this: a
/// publish that fails should be able to drop the message (or take that leg out)
/// without stopping the graph that is still managing the position.
#[test]
fn a_failing_sink_can_be_contained_without_stopping_the_graph() {
    let published: Rc<RefCell<Vec<u64>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = published.clone();

    let g = GraphBuilder::new();
    let ticks = g.ticker(STEP).count();
    // The sink fails on every third value, as a flaky downstream would.
    ticks
        .for_each(move |n: &u64| {
            if n.is_multiple_of(3) {
                bail!("publish rejected");
            }
            sink.borrow_mut().push(*n);
            Ok(())
        })
        .on_error(|_: &NodeError<'_>| ErrorAction::SkipCycle);
    // A second consumer of the same source stands in for the risk leg.
    let position = ticks.fold(0u64, |acc, n: &u64| *acc += n);

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(7)).expect("run completes");

    assert_eq!(vec![1, 2, 4, 5, 7], *published.borrow());
    assert_eq!(
        1 + 2 + 3 + 4 + 5 + 6 + 7,
        r.value(&position),
        "the risk leg saw every tick, including the ones the sink rejected"
    );
}
