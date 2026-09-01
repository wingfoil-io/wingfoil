//! Three-engine parity for the stateful / timer / fallible catalog ops that
//! reach `nitro!` purely through `#[op]` — no per-op macro table row (the old
//! `OpKind`/`OpInfo` table is gone; #496). Each op carries `#[op(build = ..)]`
//! next to its `Op` impl, which emits the naming-convention forwarders
//! (`__wf_op_<name>_*`) and the `__WF_OP_<NAME>_ACTIVATION` const the generic
//! fallback dispatches through. These tests prove `interpreted()`,
//! `compiled()`, and `nested()` (a source island in an interpreted graph) all
//! agree, exactly:
//!
//! - `skip` / `skip_while` / `step_by` / `take_while` / `throttle` /
//!   `start_with` / `audit` / `window` — stateful single-input ops. `throttle`
//!   and `window` are timer ops (`ACTIVATION::NONE`, they read
//!   `ctx.time()`/`is_last_cycle()` but never self-schedule); `start_with` and
//!   `audit` use `ACTIVATION::SCHEDULES`. `start_with`, `audit` and `window`
//!   also exercise `#[op]`'s `start`-hook forwarding. Tick **times** are
//!   asserted via `.ticked_at()` or `.with_time()`, and the runs are sized to
//!   end on a natural flush boundary so `is_last_cycle` is a no-op — that signal is
//!   deliberately not propagated into a nested island (`Ctx::nested` hard-codes
//!   it false), so ending on a boundary keeps all three engines identical.
//! - `join3` / `try_join3` — three active input edges classified by the
//!   argument convention (`&stream` → edge). `try_join` — two edges, fallible.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_nanos(10);
const INTERVAL: Duration = Duration::from_nanos(25);

/// Assert `interpreted() == compiled() == nested()` for a self-contained
/// (source) graph module whose single output is an accumulated sequence. The
/// nested check mounts the graph as a source island in an outer interpreted
/// graph, driven by the island's own ticker, and reads the island's output.
macro_rules! assert_three_engines {
    ($module:ident, $run_for:expr, $expected:expr) => {{
        let run_for = $run_for;

        let (mut runner, out) = $module::interpreted();
        runner.run(HISTORICAL, run_for).unwrap();
        let interpreted = runner.value(out);
        assert_eq!($expected, interpreted, "interpreted value mismatch");

        let (compiled,) = $module::compiled(HISTORICAL, run_for).unwrap();
        assert_eq!(interpreted, compiled, "compiled must match interpreted");

        let g = GraphBuilder::new();
        let island = $module::nested(&g);
        let mut r = g.build();
        r.run(HISTORICAL, run_for).unwrap();
        assert_eq!(
            interpreted,
            r.value(&island),
            "nested island must match interpreted"
        );
    }};
}

// --- skip: suppress an initial value prefix --------------------------------

wingfoil::nitro! {
    fn skip_values_and_times(g: &GraphBuilder) -> Stream<Vec<(NanoTime, u64)>> {
        let acc = g
            .ticker(PERIOD)
            .count()
            .skip(3)
            .with_time()
            .accumulate();
        acc
    }
}

/// The 10ns counter ticks 1..7 at t = 0,10,..,60. `skip(3)` suppresses the
/// first three values and preserves every later value's original tick time.
#[test]
fn skip_agrees_across_engines() {
    assert_three_engines!(
        skip_values_and_times,
        RunFor::Cycles(7),
        vec![
            (NanoTime::new(30), 4u64),
            (NanoTime::new(40), 5),
            (NanoTime::new(50), 6),
            (NanoTime::new(60), 7),
        ]
    );
}

// --- skip_while: suppress until a predicate first rejects -----------------

static SKIP_WHILE_PREDICATE_CALLS: AtomicUsize = AtomicUsize::new(0);

wingfoil::nitro! {
    fn skip_while_values_and_times(g: &GraphBuilder) -> Stream<Vec<(NanoTime, u64)>> {
        let values = g.ticker(PERIOD).count().map(|i| match *i {
            1 => 1,
            2 => 2,
            3 => 5,
            _ => 1,
        });
        let acc = values
            .skip_while(|value: &u64| {
                SKIP_WHILE_PREDICATE_CALLS.fetch_add(1, Ordering::SeqCst);
                *value < 5
            })
            .with_time()
            .accumulate();
        acc
    }
}

/// The final `1` satisfies the predicate again but must pass because `5`
/// permanently opened the latch. Values and original tick times agree across
/// interpreted, compiled, and nested execution.
#[test]
fn skip_while_agrees_across_engines() {
    SKIP_WHILE_PREDICATE_CALLS.store(0, Ordering::SeqCst);
    assert_three_engines!(
        skip_while_values_and_times,
        RunFor::Cycles(4),
        vec![(NanoTime::new(20), 5u64), (NanoTime::new(30), 1)]
    );
    // Each engine calls the predicate for 1, 2, and 5, but not for the final
    // 1 after the latch has opened.
    assert_eq!(SKIP_WHILE_PREDICATE_CALLS.load(Ordering::SeqCst), 3 * 3);
}

// --- step_by: emit every nth input value -----------------------------------

wingfoil::nitro! {
    fn step_by_values_and_times(g: &GraphBuilder) -> Stream<Vec<(NanoTime, u64)>> {
        let acc = g
            .ticker(PERIOD)
            .count()
            .step_by(3)
            .with_time()
            .accumulate();
        acc
    }
}

#[test]
fn step_by_agrees_across_engines() {
    assert_three_engines!(
        step_by_values_and_times,
        RunFor::Cycles(7),
        vec![
            (NanoTime::new(0), 1u64),
            (NanoTime::new(30), 4),
            (NanoTime::new(60), 7),
        ]
    );
}

wingfoil::nitro! {
    fn step_by_zero(g: &GraphBuilder) -> Stream<u64> {
        // The input never ticks, so only the lifecycle hook can reject zero.
        let stepped = g.ticker(PERIOD).count().limit(0).step_by(0);
        stepped
    }
}

#[test]
fn step_by_zero_start_error_reaches_all_engines() {
    let run_for = RunFor::Cycles(1);

    let (mut runner, _) = step_by_zero::interpreted();
    let interpreted = runner
        .run(HISTORICAL, run_for)
        .expect_err("interpreted start must reject step_by(0)");
    assert!(
        format!("{interpreted:#}").contains("step_by requires n > 0"),
        "unexpected interpreted error: {interpreted:#}"
    );

    let compiled = step_by_zero::compiled(HISTORICAL, run_for)
        .expect_err("compiled start must reject step_by(0)");
    assert!(
        format!("{compiled:#}").contains("step_by requires n > 0"),
        "unexpected compiled error: {compiled:#}"
    );

    let g = GraphBuilder::new();
    let _island = step_by_zero::nested(&g);
    let mut runner = g.build();
    let nested = runner
        .run(HISTORICAL, run_for)
        .expect_err("nested start must reject step_by(0)");
    assert!(
        format!("{nested:#}").contains("step_by requires n > 0"),
        "unexpected nested error: {nested:#}"
    );
}

// --- take_while: latch quiet after the first rejected value ----------------

wingfoil::nitro! {
    fn take_while_values_and_times(g: &GraphBuilder) -> Stream<Vec<(NanoTime, u64)>> {
        let values = g.ticker(PERIOD).count().map(|n| match n {
            1 | 2 => *n,
            3 => 9,
            _ => 1,
        });
        let acc = values.take_while(|value| *value < 5).with_time().accumulate();
        acc
    }
}

#[test]
fn take_while_agrees_across_engines() {
    assert_three_engines!(
        take_while_values_and_times,
        RunFor::Cycles(4),
        vec![(NanoTime::ZERO, 1u64), (NanoTime::new(10), 2)]
    );
}

// --- pairwise: emit pairs of consecutive values -------------------------

wingfoil::nitro! {
    fn pairwise_values_and_times(g: &GraphBuilder) -> Stream<Vec<(NanoTime, (u64,u64)) >> {
        let acc = g.ticker(PERIOD).count().pairwise().with_time().accumulate();
        acc
    }
}

#[test]
fn pairwise_agrees_across_engines() {
    assert_three_engines!(
        pairwise_values_and_times,
        RunFor::Cycles(4),
        vec![
            (NanoTime::new(10), (1u64, 2u64)),
            (NanoTime::new(20), (2u64, 3u64)),
            (NanoTime::new(30), (3u64, 4u64)),
        ]
    );
}

// --- enumerate: attach a zero-based per-stream index ----------------------

wingfoil::nitro! {
    fn enumerate_values_and_times(g: &GraphBuilder) -> Stream<Vec<(NanoTime, (u64, u64))>> {
        let out = g.ticker(PERIOD).count().enumerate().with_time().accumulate();
        out
    }
}

#[test]
fn enumerate_agrees_across_engines() {
    assert_three_engines!(
        enumerate_values_and_times,
        RunFor::Cycles(4),
        vec![
            (NanoTime::ZERO, (0, 1u64)),
            (NanoTime::new(10), (1, 2)),
            (NanoTime::new(20), (2, 3)),
            (NanoTime::new(30), (3, 4)),
        ]
    );
}

// --- throttle: rate-limit a per-cycle counter ------------------------------

wingfoil::nitro! {
    fn throttle_values(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let acc = g
            .ticker(PERIOD)
            .count()
            .throttle(INTERVAL)
            .accumulate();
        acc
    }
}

wingfoil::nitro! {
    fn throttle_times(g: &GraphBuilder) -> Stream<Vec<NanoTime>> {
        let acc = g
            .ticker(PERIOD)
            .count()
            .throttle(INTERVAL)
            .ticked_at()
            .accumulate();
        acc
    }
}

/// The 10ns counter ticks 1..7 at t = 0,10,..,60. `throttle(25ns)` emits the
/// first value (count 1 at t=0), then suppresses until 25ns have elapsed since
/// the last emit: next at t=30 (count 4), then t=60 (count 7).
#[test]
fn throttle_values_agree_across_engines() {
    assert_three_engines!(throttle_values, RunFor::Cycles(7), vec![1u64, 4, 7]);
}

/// The emission **times** for the same run: 0, 30, 60ns.
#[test]
fn throttle_times_agree_across_engines() {
    assert_three_engines!(
        throttle_times,
        RunFor::Cycles(7),
        vec![NanoTime::new(0), NanoTime::new(30), NanoTime::new(60)]
    );
}

// --- start_with: initial real tick unless the source wins at start ---------

wingfoil::nitro! {
    fn start_with_values_and_times(g: &GraphBuilder) -> Stream<Vec<(NanoTime, u64)>> {
        let acc = g
            .constant(7u64)
            .delay(Duration::from_nanos(5))
            .start_with(1)
            .with_time()
            .accumulate();
        acc
    }
}

wingfoil::nitro! {
    fn start_with_source_wins_tie(g: &GraphBuilder) -> Stream<Vec<(NanoTime, u64)>> {
        let acc = g
            .constant(7u64)
            .start_with(1)
            .with_time()
            .accumulate();
        acc
    }
}

#[test]
fn start_with_agrees_across_engines() {
    assert_three_engines!(
        start_with_values_and_times,
        RunFor::Cycles(2),
        vec![(NanoTime::ZERO, 1u64), (NanoTime::new(5), 7)]
    );
}

#[test]
fn start_with_source_wins_tie_across_engines() {
    assert_three_engines!(
        start_with_source_wins_tie,
        RunFor::Cycles(1),
        vec![(NanoTime::ZERO, 7u64)]
    );
}

// --- audit: fixed-window trailing-edge rate limiting ----------------------

wingfoil::nitro! {
    fn audit_values_and_times(g: &GraphBuilder) -> Stream<Vec<(NanoTime, u64)>> {
        let acc = g
            .ticker(PERIOD)
            .count()
            .limit(5)
            .audit(Duration::from_nanos(20))
            .with_time()
            .accumulate();
        acc
    }
}

/// Source ticks at t=20 and t=40 collide with audit deadlines. They seed the
/// following windows while counts 2 and 4 close the previous ones. Limiting
/// the source to five values leaves the t=60 deadline with no input, so the run
/// ends on a natural flush and the nested island does not depend on the outer
/// runner's `is_last_cycle` signal.
#[test]
fn audit_agrees_across_engines() {
    assert_three_engines!(
        audit_values_and_times,
        RunFor::Cycles(7),
        vec![
            (NanoTime::new(20), 2u64),
            (NanoTime::new(40), 4),
            (NanoTime::new(60), 5),
        ]
    );
}

// --- window: fixed-time-boundary buffering ---------------------------------

wingfoil::nitro! {
    fn window_values(g: &GraphBuilder) -> Stream<Vec<Vec<u64>>> {
        let acc = g.ticker(PERIOD).count().window(INTERVAL).accumulate();
        acc
    }
}

wingfoil::nitro! {
    fn window_times(g: &GraphBuilder) -> Stream<Vec<NanoTime>> {
        let acc = g
            .ticker(PERIOD)
            .count()
            .window(INTERVAL)
            .ticked_at()
            .accumulate();
        acc
    }
}

/// The first boundary is at t=25 (start + interval); with 10ns samples the
/// window flushes [1,2,3] at t=30, advances to t=50, and flushes [4,5] at
/// t=50. Running exactly 6 cycles (last cycle t=50) ends on that boundary, so
/// the run-end `is_last_cycle` flush is a no-op and every engine agrees —
/// including the nested island, where `is_last_cycle` is always false.
#[test]
fn window_values_agree_across_engines() {
    assert_three_engines!(
        window_values,
        RunFor::Cycles(6),
        vec![vec![1u64, 2, 3], vec![4, 5]]
    );
}

/// The window emission **times** for the same run: 30, 50ns.
#[test]
fn window_times_agree_across_engines() {
    assert_three_engines!(
        window_times,
        RunFor::Cycles(6),
        vec![NanoTime::new(30), NanoTime::new(50)]
    );
}

// --- join3 / try_join / try_join3: multi-input edges -----------------------

wingfoil::nitro! {
    fn join3_sum(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let a = g.ticker(PERIOD).count();
        let b = a.map(|i| i * 2);
        let c = a.map(|i| i * 3);
        let acc = a.join3(&b, &c, |x, y, z| x + y + z).accumulate();
        acc
    }
}

/// Three active edges (receiver + two `&stream` args). At count c the sum is
/// c + 2c + 3c = 6c → 6, 12, 18.
#[test]
fn join3_agrees_across_engines() {
    assert_three_engines!(join3_sum, RunFor::Cycles(3), vec![6u64, 12, 18]);
}

wingfoil::nitro! {
    fn try_join_sum(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let a = g.ticker(PERIOD).count();
        let b = a.map(|i| i * 10);
        let acc = a
            .try_join(&b, |x: &u64, y: &u64| Ok(x + y))
            .accumulate();
        acc
    }
}

/// Two edges, fallible closure returning `Ok`: c + 10c = 11c → 11, 22, 33.
#[test]
fn try_join_agrees_across_engines() {
    assert_three_engines!(try_join_sum, RunFor::Cycles(3), vec![11u64, 22, 33]);
}

wingfoil::nitro! {
    fn try_join3_sum(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let a = g.ticker(PERIOD).count();
        let b = a.map(|i| i * 2);
        let c = a.map(|i| i * 3);
        let acc = a
            .try_join3(&b, &c, |x: &u64, y: &u64, z: &u64| Ok(x + y + z))
            .accumulate();
        acc
    }
}

/// Three edges, fallible closure returning `Ok`: 6c → 6, 12, 18.
#[test]
fn try_join3_agrees_across_engines() {
    assert_three_engines!(try_join3_sum, RunFor::Cycles(3), vec![6u64, 12, 18]);
}

// --- fallible propagation: a returned Err aborts every engine ---------------

wingfoil::nitro! {
    fn try_join_fails(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let a = g.ticker(PERIOD).count();
        let b = a.map(|i| i * 10);
        let acc = a
            .try_join(&b, |_: &u64, _: &u64| -> anyhow::Result<u64> {
                anyhow::bail!("boom")
            })
            .accumulate();
        acc
    }
}

/// The emitted cycle threads `?`, so a returned `Err` aborts the run on both
/// standalone engines identically.
#[test]
fn try_join_error_aborts_both_engines() {
    let (mut runner, _out) = try_join_fails::interpreted();
    let interp_err = runner.run(HISTORICAL, RunFor::Cycles(3));
    assert!(interp_err.is_err(), "interpreted must abort on Err");

    let compiled_err = try_join_fails::compiled(HISTORICAL, RunFor::Cycles(3));
    assert!(compiled_err.is_err(), "compiled must abort on Err");
}

// --- delay / delay_with_reset: the `Duration` hoisted into `start` ----------
//
// `throttle`, `window`, `delay` and `delay_with_reset` all take their interval
// as a `Duration` and convert it to engine nanoseconds **once, in `start`** —
// the shape `TickerState` documents. `throttle` and `window` are covered
// above; these two blocks cover the other pair, and they matter more than the
// conversion's cost suggests.
//
// `delay_with_reset` in particular is wired through `DelayWithResetFwd`, a
// forwarder that restates the real op's `In` in the two-edge form `#[op]` can
// parse and then *delegates*. A delegating forwarder has to delegate `start`
// too, and if it silently does not, the hoisted delay stays at its `Default`
// of zero — i.e. every `delay_with_reset` in the tree degrades to a
// pass-through. That failure is invisible to an interpreted-vs-compiled parity
// assertion, because both tiers reach the op through the same forwarder and so
// break together. Only an absolute expectation catches it, which is what these
// pin.

wingfoil::nitro! {
    fn delay_values_and_times(g: &GraphBuilder) -> Stream<Vec<(NanoTime, u64)>> {
        let acc = g
            .ticker(PERIOD)
            .count()
            .delay(INTERVAL)
            .with_time()
            .accumulate();
        acc
    }
}

/// The 10ns counter ticks 1,2,3,… at t = 0,10,20,…; `delay(25ns)` re-emits each
/// value 25ns later, so the delay's own schedule interleaves cycles at
/// t = 25,35,45,… between the ticker's. Values are unchanged and every one is
/// shifted by exactly the interval — a delay that had lost its interval (the
/// hoist landing in the wrong place) would emit at the source's own times.
#[test]
fn delay_agrees_across_engines() {
    assert_three_engines!(
        delay_values_and_times,
        RunFor::Cycles(8),
        vec![
            (NanoTime::new(25), 1u64),
            (NanoTime::new(35), 2),
            (NanoTime::new(45), 3),
        ]
    );
}

wingfoil::nitro! {
    fn delay_with_reset_values_and_times(g: &GraphBuilder) -> Stream<Vec<(NanoTime, u64)>> {
        let count = g.ticker(PERIOD).count();
        // Ticks exactly once, when the count reaches 3 (t = 20ns).
        let trigger = count.filter_value(|i: &u64| *i == 3);
        let acc = count
            .delay_with_reset(INTERVAL, &trigger)
            .with_time()
            .accumulate();
        acc
    }
}

/// Same counter and interval as [`delay_agrees_across_engines`], plus a reset
/// that fires once at t=20 (count 3). The reset snaps the output to the *live*
/// value and drops everything queued, so the two values already in flight
/// (1 due at 25, 2 due at 35) never arrive; the delay resumes from the next
/// upstream tick, putting count 4 (t=30) out at t=55.
#[test]
fn delay_with_reset_agrees_across_engines() {
    assert_three_engines!(
        delay_with_reset_values_and_times,
        RunFor::Cycles(11),
        vec![
            (NanoTime::new(20), 3u64),
            (NanoTime::new(55), 4),
            (NanoTime::new(65), 5),
        ]
    );
}

// --- filter: the condition edge activates, the tick flag does not ----------

wingfoil::nitro! {
    fn filter_values_and_times(g: &GraphBuilder) -> Stream<Vec<(NanoTime, u64)>> {
        // Source and condition tick on *different* schedules, so the condition
        // ticks alone at t = 30 and t = 90.
        let source = g.ticker(Duration::from_nanos(20)).count();
        let condition = g
            .ticker(Duration::from_nanos(30))
            .count()
            .map(|i: &u64| i.is_multiple_of(2));
        let acc = source.filter(&condition).with_time().accumulate();
        acc
    }
}

/// `filter` declares no tick flag on its condition edge, and this is what
/// proves it does not need one (#834). Resampling on a condition tick comes
/// from that edge being *active* — the engine activates the node, and `cycle`
/// re-emits the held source off the condition's current value. The entries at
/// t=30 and t=90 are cycles in which the **source did not tick at all**: the
/// condition flipped true on its own schedule and the held source value came
/// out. If the flag were load-bearing, those two would be missing.
///
/// Source ticks 1..5 at t = 0,20,40,60,80; the condition is true from t=30
/// (its 2nd tick) and from t=90 (its 4th).
#[test]
fn filter_resamples_on_condition_ticks_across_engines() {
    assert_three_engines!(
        filter_values_and_times,
        RunFor::Cycles(7),
        vec![
            (NanoTime::new(30), 2u64),
            (NanoTime::new(40), 3),
            (NanoTime::new(90), 5),
        ]
    );
}
