//! Regression tests for the semantic-parity bugs found in the fable review
//! (`docs/fable-review.md`). Each test pins interpreted == compiled == nested
//! for a case that previously drifted between the three execution paths, or
//! pins next's behaviour against classic wingfoil.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

// ===========================================================================
// BUG 1: Fold value-slot seeding drift.
//
// The interpreted engine seeds a fold's output slot with `init.clone()`; the
// compiled/nested emission used to seed every slot with `Default::default()`.
// A fold with `init != Default` read (via a passive/sample edge) before its
// first tick therefore returned `init` interpreted but `0` compiled/nested.
// ===========================================================================

wingfoil_next::graph! {
    fn fold_seed(g: &GraphBuilder) -> Stream<Vec<i64>> {
        // A trigger that ticks from t=0, and a fold whose source is *delayed*
        // so the fold does not tick until t=25. Sampling the fold on the
        // trigger reads its output slot at t=0/10/20 — before its first
        // tick — so the read observes the seed, not a folded value.
        let trig = g.ticker(Duration::from_nanos(10));
        let base = g.ticker(Duration::from_nanos(10)).count().map(|c| *c as i64);
        let delayed = base.delay(Duration::from_nanos(25));
        let acc = delayed.fold(100i64, |a, v| *a += v);
        let sampled = acc.sample(&trig).accumulate();
        sampled
    }
}

#[test]
fn fold_non_default_init_seed_parity() {
    let run_for = RunFor::Cycles(6);

    let (mut runner, sampled) = fold_seed::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(sampled);

    // The fold seeds with init=100 and does not tick until the delay elapses,
    // so the earliest passive reads observe 100 (not Default = 0).
    assert_eq!(100, interpreted[0], "passive read before first tick sees init");

    let (compiled,) = fold_seed::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, compiled, "interpreted == compiled");

    let g = GraphBuilder::new();
    let island = fold_seed::nested(&g);
    let mut r = g.build();
    r.run(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, r.value(&island), "interpreted == nested");
}

// ===========================================================================
// BUG 6: reachable user errors must `bail!`, not panic.
//
// Running a graph with external/poll sources historically, or running a
// realtime-source graph twice, is a reachable caller mistake — it must return
// an `Err`, not `assert!`/`.expect()`-panic.
// ===========================================================================

#[test]
fn external_source_historical_run_is_an_error_not_a_panic() {
    let g = GraphBuilder::new();
    let (values, _src) = g.external::<u64>();
    let _acc = values.collapse_accumulate();
    let mut r = g.build();

    let err = r
        .run(HISTORICAL, RunFor::Cycles(3))
        .expect_err("external source in a historical run must error");
    assert!(
        format!("{err:#}").contains("RunMode::RealTime"),
        "error explains the realtime requirement: {err:#}"
    );
}

// ===========================================================================
// BUG 4: historical channel timestamp policy.
//
// A `send_at` before `start_time` used to rewind the run clock (the kernel
// schedules callbacks verbatim); out-of-order timestamps were silently sorted
// where classic errors. Both are now rejected at the channel's `start` hook.
// ===========================================================================

#[test]
fn historical_channel_rejects_pre_start_timestamp() {
    let start = NanoTime::new(100);
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let _acc = values.collapse_accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send_at(1, NanoTime::new(50)); // before start = 100
        sender.close();
    });
    let err = r
        .run(RunMode::HistoricalFrom(start), RunFor::Forever)
        .expect_err("a pre-start timestamp must error");
    producer.join().expect("producer thread");
    assert!(
        format!("{err:#}").contains("before the run start"),
        "error explains pre-start rejection: {err:#}"
    );
}

#[test]
fn historical_channel_rejects_out_of_order_timestamps() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let _acc = values.collapse_accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send_at(1, NanoTime::new(200));
        sender.send_at(2, NanoTime::new(100)); // out of order
        sender.close();
    });
    let err = r
        .run(HISTORICAL, RunFor::Forever)
        .expect_err("out-of-order timestamps must error (classic parity)");
    producer.join().expect("producer thread");
    assert!(
        format!("{err:#}").contains("out of order"),
        "error explains out-of-order rejection: {err:#}"
    );
}

// ===========================================================================
// BUG 5: realtime close() must end the run even with a live sender clone.
//
// The kernel alone only ends a realtime run when *every* waker clone is
// dropped. A `finished` flag set by the channel node on `EndOfStream` lets
// `close()` terminate the run while a producer keeps a live `ChannelSender`.
// ===========================================================================

#[test]
fn realtime_close_ends_run_with_live_sender_clone() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let acc = values.collapse_accumulate();
    let mut r = g.build();

    // A live clone kept in *this* thread across the whole run: the waker
    // channel never disconnects on its own, so only `close()` can end the
    // `Forever` run.
    let keep_alive = sender.clone();
    let producer = std::thread::spawn(move || {
        sender.send(1);
        sender.send(2);
        std::thread::sleep(Duration::from_millis(5));
        sender.close();
    });

    r.run(RunMode::RealTime, RunFor::Forever)
        .expect("run terminates on close()");
    producer.join().expect("producer thread");
    drop(keep_alive);

    assert_eq!(vec![1, 2], r.value(&acc), "all pre-close values delivered");
}

#[test]
fn poll_source_historical_run_is_an_error_not_a_panic() {
    let g = GraphBuilder::new();
    let _p = g.poll(|| Some(1u64)).accumulate();
    let mut r = g.build();

    let err = r
        .run(HISTORICAL, RunFor::Cycles(3))
        .expect_err("poll source in a historical run must error");
    assert!(
        format!("{err:#}").contains("busy-poll"),
        "error explains the poll/realtime requirement: {err:#}"
    );
}

// ===========================================================================
// BUG 7: the compiled path must evaluate a non-literal closure arg once.
//
// `count.map(make_f())` ran its factory once at wiring (interpreted) but once
// per due cycle (compiled), because the emission inlined the factory call into
// the cycle loop. Non-literal closure configs are now hoisted into a setup
// local evaluated once.
// ===========================================================================

static FACTORY_CALLS: AtomicUsize = AtomicUsize::new(0);

/// A side-effecting factory: each call increments a global counter and returns
/// a fresh (stateless) doubling closure.
fn make_scale() -> impl Fn(&u64) -> u64 {
    FACTORY_CALLS.fetch_add(1, Ordering::SeqCst);
    |v| v * 2
}

wingfoil_next::graph! {
    fn factory_graph(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let acc = g
            .ticker(Duration::from_nanos(10))
            .count()
            .map(make_scale())
            .accumulate();
        acc
    }
}

#[test]
fn compiled_evaluates_closure_factory_once() {
    let run_for = RunFor::Cycles(5);

    FACTORY_CALLS.store(0, Ordering::SeqCst);
    let (compiled,) = factory_graph::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(vec![2, 4, 6, 8, 10], compiled);
    assert_eq!(
        1,
        FACTORY_CALLS.load(Ordering::SeqCst),
        "factory runs once, not once per cycle"
    );

    // Interpreted evaluates it once at wiring, too — and the values agree.
    FACTORY_CALLS.store(0, Ordering::SeqCst);
    let (mut runner, acc) = factory_graph::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    assert_eq!(compiled, runner.value(acc));
    assert_eq!(1, FACTORY_CALLS.load(Ordering::SeqCst));
}

// ===========================================================================
// BUG 8: the macro must not accept inputs it emits broken code for.
//
// A bare alias output (`let out = acc;`), a bare-ticker return (`Stream<()>`),
// and a user stream named like a generated intermediate (`anon1`) all worked
// interpreted but emitted `__v_*`-not-found / collision errors compiled/nested.
// ===========================================================================

wingfoil_next::graph! {
    fn aliased_output(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let acc = g.ticker(Duration::from_nanos(10)).count().accumulate();
        // A bare alias used as the returned output.
        let out = acc;
        out
    }
}

#[test]
fn bare_alias_output_works_in_all_paths() {
    let run_for = RunFor::Cycles(3);
    let (mut runner, out) = aliased_output::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(out);
    assert_eq!(vec![1u64, 2, 3], interpreted);

    let (compiled,) = aliased_output::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, compiled);

    let g = GraphBuilder::new();
    let island = aliased_output::nested(&g);
    let mut r = g.build();
    r.run(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, r.value(&island));
}

wingfoil_next::graph! {
    fn bare_ticker(g: &GraphBuilder) -> Stream<()> {
        let tick = g.ticker(Duration::from_nanos(10));
        tick
    }
}

#[test]
fn bare_ticker_return_works_in_all_paths() {
    let run_for = RunFor::Cycles(3);
    let (mut runner, tick) = bare_ticker::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    // Unit output: the value is `()`; the point is that all three paths compile
    // and run to the same bound.
    let _: () = runner.value(tick);

    let ((),) = bare_ticker::compiled(HISTORICAL, run_for).unwrap();

    let g = GraphBuilder::new();
    let island = bare_ticker::nested(&g);
    let mut r = g.build();
    r.run(HISTORICAL, run_for).unwrap();
    let _: () = r.value(&island);
}

wingfoil_next::graph! {
    // A user stream literally named `anon1` — previously collided with the
    // macro's generated intermediate names (now reserved-prefixed).
    fn anon_collision(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let anon1 = g.ticker(Duration::from_nanos(10)).count().map(|i| i * 2);
        let out = anon1.accumulate();
        out
    }
}

#[test]
fn user_stream_named_anon1_does_not_collide() {
    let run_for = RunFor::Cycles(3);
    let (mut runner, out) = anon_collision::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(out);
    assert_eq!(vec![2u64, 4, 6], interpreted);

    let (compiled,) = anon_collision::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, compiled);
}
