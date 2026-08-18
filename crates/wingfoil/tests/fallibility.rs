//! Phase 0.1 spike: fallible `cycle` + lifecycle hooks, cross-checked
//! against the legacy engine's contract — a cycle error aborts the run and
//! is reported with node context, and `stop`/`teardown` still run afterwards
//! (here observed through a `finally` node) regardless of how the run ended.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::bail;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// A `try_map` that fails mid-run aborts the run; the error names the failing
/// node and carries the op's own context; and a `finally` node's teardown
/// still fires (observing the last value seen before the abort).
#[test]
fn cycle_error_aborts_with_context_and_runs_teardown() {
    let torn_down = Rc::new(RefCell::new(None::<u64>));
    let recorder = torn_down.clone();

    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count();
    // Errors when the count reaches 3 — cycles 1 and 2 succeed, cycle 3 aborts.
    let mapped = count.try_map(|i: &u64| {
        if *i >= 3 {
            bail!("boom at count {i}");
        }
        Ok(*i * 10)
    });
    let acc = mapped.accumulate();
    // Cleanup that must run even though the run aborts.
    let _guard = count.finally(move |last| {
        *recorder.borrow_mut() = Some(*last);
        Ok(())
    });

    let mut r = g.build();
    let result = r.run(HISTORICAL, RunFor::Cycles(10));

    // The run aborted with an error.
    let err = result.expect_err("the run must abort when try_map fails");
    let msg = format!("{err:#}");
    // Node context (label + kind) and the op's own message are both present.
    // Labels are the op type name (via `type_name`), so `try_map` → `TryMap`.
    assert!(msg.contains("TryMap"), "error should name the node: {msg}");
    assert!(
        msg.contains("boom at count 3"),
        "error should chain the cause: {msg}"
    );

    // Only the pre-error values were accumulated (1*10, 2*10).
    assert_eq!(vec![10, 20], r.value(&acc));

    // Teardown ran despite the abort, seeing the last value before it (2).
    assert_eq!(Some(2), *torn_down.borrow());
}

/// `try_filter_map` is `try_map`'s map-and-filter twin: an `Err` aborts the
/// run with context naming the node, same as `try_map` above — the
/// `Ok((_, false))` filtering path is not a substitute for propagating a
/// real error, and this pins that they behave differently.
#[test]
fn try_filter_map_err_aborts_run() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count();
    // Cycles 1 and 2 succeed (odd values kept, even ones filtered); cycle 3
    // errors instead of filtering.
    let filtered = count.try_filter_map(|i: &u64| {
        if *i >= 3 {
            bail!("boom at count {i}");
        }
        Ok((*i * 10, i % 2 == 1))
    });
    let acc = filtered.accumulate();

    let mut r = g.build();
    let result = r.run(HISTORICAL, RunFor::Cycles(10));

    let err = result.expect_err("the run must abort when try_filter_map fails");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("TryFilterMap"),
        "error should name the node: {msg}"
    );
    assert!(
        msg.contains("boom at count 3"),
        "error should chain the cause: {msg}"
    );

    // Only the pre-error, filtered-in value was accumulated (1*10; 2*10 was
    // filtered out, not erred).
    assert_eq!(vec![10], r.value(&acc));
}

/// A clean run runs teardown too — `finally` fires with the last value seen.
#[test]
fn teardown_runs_on_clean_completion() {
    let last_seen = Rc::new(RefCell::new(None::<u64>));
    let recorder = last_seen.clone();

    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count();
    let _guard = count.finally(move |last| {
        *recorder.borrow_mut() = Some(*last);
        Ok(())
    });

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3))
        .expect("clean run must not error");

    assert_eq!(Some(3), *last_seen.borrow());
}

/// A sink (`for_each`) is fallible too: a write error aborts the run with
/// context naming the sink.
#[test]
fn sink_error_aborts_with_context() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count();
    let _sink = count.for_each(|i: &u64| {
        if *i >= 2 {
            bail!("sink write failed at {i}");
        }
        Ok(())
    });

    let mut r = g.build();
    let err = r
        .run(HISTORICAL, RunFor::Cycles(10))
        .expect_err("the run must abort when the sink fails");
    let msg = format!("{err:#}");
    assert!(
        // The `for_each` sink op is `Sink`; labels come from `type_name`.
        msg.contains("Sink"),
        "error should name the sink: {msg}"
    );
    assert!(msg.contains("sink write failed at 2"), "cause: {msg}");
}
