//! Focused tests for the shared adapter-support primitives on the fluent layer:
//! `GraphBuilder::replay_results` (the historical replay engine behind the
//! `lines`/`csv` sources) and `StreamOps::for_each_mut` (the `&mut`-writer sink
//! behind their file sinks). The adapter tests cover them end-to-end through
//! files; these exercise the primitives directly.

use std::cell::RefCell;
use std::rc::Rc;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::Burst;
use wingfoil_next::prelude::*;

/// `replay_results` queues each `(value, time)` onto a historical source and
/// groups same-instant rows into one atomic burst, delivering them on the graph
/// clock at their timestamps.
#[test]
fn replay_results_groups_same_instant_rows_into_bursts() {
    let g = GraphBuilder::new();
    let rows: Vec<anyhow::Result<(u32, NanoTime)>> = vec![
        Ok((1, NanoTime::new(10))),
        Ok((2, NanoTime::new(10))), // same instant → same burst
        Ok((3, NanoTime::new(20))),
    ];
    let src = g.replay_results(rows);
    let stamped = src.with_time().accumulate();

    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    let got: Vec<(NanoTime, Vec<u32>)> = r
        .value(&stamped)
        .into_iter()
        .map(|(t, b)| (t, b.iter().copied().collect()))
        .collect();
    assert_eq!(
        got,
        vec![
            (NanoTime::new(10), vec![1, 2]),
            (NanoTime::new(20), vec![3]),
        ],
    );
}

/// A mid-sequence `Err` row is forwarded via `send_error` and aborts the run
/// with that error's context; rows after it are never queued.
#[test]
fn replay_results_forwards_error_and_stops() {
    let g = GraphBuilder::new();
    let rows: Vec<anyhow::Result<(u32, NanoTime)>> = vec![
        Ok((1, NanoTime::new(10))),
        Err(anyhow::anyhow!("replay boom")),
        Ok((2, NanoTime::new(20))), // never reached
    ];
    let src = g.replay_results(rows);
    let _acc = src.accumulate();

    let mut r = g.build();
    let err = r
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .expect_err("the forwarded error must abort the run");
    assert!(
        format!("{err:#}").contains("replay boom"),
        "unexpected error: {err:#}"
    );
}

/// `for_each_mut` moves an owned resource in and hands `f` a `&mut` to it each
/// tick. A retained `Rc` clone (the writer shares one) lets the test read what
/// was written after the run.
#[test]
fn for_each_mut_gives_a_mutable_writer_each_tick() {
    #[derive(Clone, Default)]
    struct Recorder(Rc<RefCell<Vec<u32>>>);

    let recorder = Recorder::default();

    let g = GraphBuilder::new();
    let src = g.replay_results(vec![
        Ok((10u32, NanoTime::new(1))),
        Ok((20, NanoTime::new(2))),
        Ok((30, NanoTime::new(3))),
    ]);
    let _sink = src.for_each_mut(recorder.clone(), |w: &mut Recorder, burst: &Burst<u32>| {
        for v in burst.iter() {
            w.0.borrow_mut().push(*v);
        }
        Ok(())
    });

    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    assert_eq!(*recorder.0.borrow(), vec![10, 20, 30]);
}

/// An `Err` from the `for_each_mut` closure aborts the run with context.
#[test]
fn for_each_mut_error_aborts_the_run() {
    let g = GraphBuilder::new();
    let src = g.replay_results(vec![Ok((1u32, NanoTime::new(1)))]);
    let _sink = src.for_each_mut(Vec::<u32>::new(), |w: &mut Vec<u32>, burst: &Burst<u32>| {
        w.extend(burst.iter().copied());
        anyhow::bail!("sink boom")
    });

    let mut r = g.build();
    let err = r
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .expect_err("a sink error must abort the run");
    assert!(
        format!("{err:#}").contains("sink boom"),
        "unexpected error: {err:#}"
    );
}
