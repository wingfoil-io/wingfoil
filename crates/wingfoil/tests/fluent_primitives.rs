//! Focused tests for the shared adapter-support primitives on the fluent layer:
//! `GraphBuilder::replay_results` (the historical replay engine behind the
//! `lines`/`csv` sources), `StreamOps::for_each_mut` (the `&mut`-writer sink
//! behind their file sinks), and `SourceOps::source_at_start` (the
//! deferred-connection source behind live adapters like `zmq_sub`). The adapter
//! tests cover them end-to-end through files/sockets; these exercise the
//! primitives directly. Also `Stream::build` — `GraphBuilder::build` reached
//! from the end of a chain.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use wingfoil::Burst;
use wingfoil::channel::ChannelSender;
use wingfoil::interp::StopHandle;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

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

/// `source_at_start` performs **no** I/O at wiring: the `setup` closure runs at
/// `start()`, not when the source factory is called. A spy counter proves the
/// deferral — zero after wiring + `build()`, one after a run — and a stop guard
/// whose `Drop` flips a flag proves the returned `StopHandle` is dropped at
/// teardown. This is the acceptance test for the deferred-connection primitive
/// (see `docs/source-lifecycle-defer-to-start.md`).
#[test]
fn source_at_start_defers_setup_to_run_and_stops_at_teardown() {
    /// Its `Drop` (run when the `StopHandle` is dropped at teardown) flips the
    /// shared flag — the generalised `ThreadStopGuard`.
    struct Guard(Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    let g = GraphBuilder::new();
    let setups = Rc::new(Cell::new(0u32));
    let stopped = Arc::new(AtomicBool::new(false));

    let seen = setups.clone();
    let stop_flag = stopped.clone();
    let source = g.source_at_start::<u64, _>(move |sender: ChannelSender<u64>| {
        seen.set(seen.get() + 1);
        // Stand in for "connect + spawn the producer": feed one value, then
        // close so the realtime run terminates promptly.
        sender.send(7);
        sender.close();
        Ok(StopHandle::new(Guard(stop_flag.clone())))
    });
    let acc = source.collapse_accumulate();
    let mut r = g.build();

    // Nothing has run: wiring + build touched no producer.
    assert_eq!(0, setups.get(), "setup must not run at wiring/build");
    assert!(
        !stopped.load(Ordering::Relaxed),
        "no producer to stop before the run"
    );

    r.run(RunMode::RealTime, RunFor::Forever).unwrap();

    assert_eq!(1, setups.get(), "setup runs once, at start");
    assert_eq!(vec![7u64], r.value(&acc), "the deferred producer's value");
    assert!(
        stopped.load(Ordering::Relaxed),
        "the StopHandle guard is dropped at teardown"
    );
}

/// A `setup` error aborts the run at start with node context — a deferred
/// connection failure surfaces when the run begins (legacy-consistent), not at
/// wiring. The factory call itself still succeeds.
#[test]
fn source_at_start_setup_error_aborts_the_run() {
    let g = GraphBuilder::new();
    let source =
        g.source_at_start::<u64, _>(|_sender: ChannelSender<u64>| anyhow::bail!("connect boom"));
    let _acc = source.collapse_accumulate();
    let mut r = g.build();

    let err = r
        .run(RunMode::RealTime, RunFor::Cycles(1))
        .expect_err("a setup error must abort the run");
    let msg = format!("{err:#}");
    assert!(msg.contains("connect boom"), "unexpected error: {msg}");
    assert!(
        msg.contains("start"),
        "error carries node start context: {msg}"
    );
}

/// In historical mode `setup` is composed **ahead** of the channel node's own
/// up-front collect (the `prev_start` composition), so a deferred producer that
/// `send_at`s timestamped values and then `close()`s replays them on the graph
/// clock at their timestamps — exactly like a plain `channel`. This also pins
/// the documented contract that a historical producer must `close()` explicitly:
/// the source retains a live sender, so `EndOfStream` (not sender-drop) is what
/// ends the collect.
#[test]
fn source_at_start_historical_replays_timestamped_sends() {
    let g = GraphBuilder::new();
    let source = g.source_at_start::<u64, _>(|sender: ChannelSender<u64>| {
        // Stand in for a deferred historical producer: emit timestamped values
        // (same-instant ones group into one burst), then close so the up-front
        // collect terminates. Dropping the sender alone would not — the source
        // keeps one alive for the run.
        sender.send_at(1, NanoTime::new(10));
        sender.send_at(2, NanoTime::new(10));
        sender.send_at(3, NanoTime::new(20));
        sender.close();
        Ok(StopHandle::new(()))
    });
    let stamped = source.with_time().accumulate();

    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    let got: Vec<(NanoTime, Vec<u64>)> = r
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
        "deferred historical sends replay grouped, on the graph clock",
    );
}

/// `register_op3` — the three-active-input registration primitive, the general
/// form of the `Join3`-specialised `trimap`. All three upstreams trigger, all
/// three are read by reference, and `cfg`/`state` are engine-owned.
#[test]
fn register_op3_reads_three_actives_and_owns_its_state() {
    let g = GraphBuilder::new();
    let counter = g.ticker(std::time::Duration::from_nanos(100)).count();
    let a = counter.map(|n: &u64| *n as f64);
    let b = counter.map(|n: &u64| *n as f64 * 2.0);
    let c = counter.map(|n: &u64| *n as f64 * 3.0);

    let (b_handle, c_handle) = (b.handle(), c.handle());
    // cfg is the weight; state accumulates across cycles, proving both are
    // engine-owned rather than captured.
    let blended: Stream<f64> = a.wire(move |bld, h| {
        bld.register_op3(
            h,
            b_handle,
            c_handle,
            "blend3",
            Activation::NONE,
            10.0_f64,
            || 0.0_f64,
            |weight: &mut f64, total: &mut f64, x: &f64, y: &f64, z: &f64, _ctx| {
                *total += x + y + z;
                Ok(Tick::Value(*total * *weight))
            },
        )
    });

    let out = blended.with_time().accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();

    // n=1: 1+2+3=6; n=2: +12 -> 18; n=3: +18 -> 36. Weighted by 10.
    assert_eq!(
        vec![
            (NanoTime::ZERO, 60.0),
            (NanoTime::new(100), 180.0),
            (NanoTime::new(200), 360.0),
        ],
        runner.value(&out)
    );
}

/// The `state_init` handed to `register_op3` re-seeds on a re-run, so a second
/// `run()` replays from a clean accumulator rather than continuing the first.
#[test]
fn register_op3_state_re_seeds_between_runs() {
    let g = GraphBuilder::new();
    let counter = g.ticker(std::time::Duration::from_nanos(100)).count();
    let a = counter.map(|n: &u64| *n as f64);
    let (b, c) = (a.clone(), a.clone());

    let (b_handle, c_handle) = (b.handle(), c.handle());
    let summed: Stream<f64> = a.wire(move |bld, h| {
        bld.register_op3(
            h,
            b_handle,
            c_handle,
            "sum3",
            Activation::NONE,
            (),
            || 0.0_f64,
            |_cfg, total: &mut f64, x: &f64, y: &f64, z: &f64, _ctx| {
                *total += x + y + z;
                Ok(Tick::Value(*total))
            },
        )
    });

    let mut runner = g.build();
    let bounds = (RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(2));
    runner.run(bounds.0, bounds.1).unwrap();
    let first = runner.value(&summed);
    runner.run(bounds.0, bounds.1).unwrap();
    assert_eq!(
        first,
        runner.value(&summed),
        "re-run must not continue state"
    );
}

/// `register_op4` — the four-active-input rung. Each arity is its own function
/// because the inputs are heterogeneous static types; this proves the fourth
/// slot is read and triggers like the rest.
#[test]
fn register_op4_reads_four_actives() {
    let g = GraphBuilder::new();
    let counter = g.ticker(std::time::Duration::from_nanos(100)).count();
    let a = counter.map(|n: &u64| *n as f64);
    let b = counter.map(|n: &u64| *n as f64 * 2.0);
    let c = counter.map(|n: &u64| *n as f64 * 3.0);
    let d = counter.map(|n: &u64| *n as f64 * 4.0);

    let (b_h, c_h, d_h) = (b.handle(), c.handle(), d.handle());
    let blended: Stream<f64> = a.wire(move |bld, h| {
        bld.register_op4(
            h,
            b_h,
            c_h,
            d_h,
            "blend4",
            Activation::NONE,
            (),
            || 0.0_f64,
            |_cfg, total: &mut f64, w: &f64, x: &f64, y: &f64, z: &f64, _ctx| {
                *total += w + x + y + z;
                Ok(Tick::Value(*total))
            },
        )
    });

    let out = blended.with_time().accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();

    // n=1: 1+2+3+4 = 10; n=2: +20 -> 30; n=3: +30 -> 60.
    assert_eq!(
        vec![
            (NanoTime::ZERO, 10.0),
            (NanoTime::new(100), 30.0),
            (NanoTime::new(200), 60.0),
        ],
        runner.value(&out)
    );
}

/// `Stream::build` builds the whole graph from the end of a chain, so a program
/// is one expression: wire, build and run without ever naming the builder.
#[test]
fn stream_build_runs_the_whole_graph_from_the_end_of_a_chain() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();

    GraphBuilder::new()
        .ticker(std::time::Duration::from_nanos(100))
        .count()
        .map(|i: &u64| format!("hello, world {i}"))
        .for_each(move |s: &String| {
            sink.borrow_mut().push(s.clone());
            Ok(())
        })
        .build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();

    assert_eq!(
        vec!["hello, world 1", "hello, world 2", "hello, world 3"],
        *seen.borrow(),
    );
}

/// It builds the *graph*, not the stream it is called on: a sibling branch wired
/// from the same builder still runs, and every stream stays readable as a value
/// handle afterwards.
#[test]
fn stream_build_builds_the_graph_not_the_stream() {
    let g = GraphBuilder::new();
    let counter = g.ticker(std::time::Duration::from_nanos(100)).count();
    let doubled = counter.map(|n: &u64| *n * 2).accumulate();
    // Built from one branch; the sibling branch above must still be in the graph.
    let mut runner = counter.map(|n: &u64| *n + 100).accumulate().build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();

    assert_eq!(vec![2, 4, 6], runner.value(&doubled));
}

/// `Stream::build` shares the builder's call-once guard: a second build — from
/// the builder or any other stream — panics with the explanatory message rather
/// than handing back an empty `Runner`.
#[test]
#[should_panic(expected = "GraphBuilder::build() called twice")]
fn stream_build_shares_the_builders_call_once_guard() {
    let g = GraphBuilder::new();
    let s = g.ticker(std::time::Duration::from_nanos(100)).count();
    let _first = s.build();
    let _second = g.build();
}
