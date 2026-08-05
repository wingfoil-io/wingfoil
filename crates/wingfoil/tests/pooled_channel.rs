//! The pooled channel: loan-based producer API over recycled buffers, graph
//! delivery as [`Pooled`] handles. Same channel semantics as `tests/channel.rs`
//! (bursts, both run modes, error propagation) — what these tests add is the
//! pool: buffers **recycle** instead of being reallocated, the bounded pool is
//! the backpressure, and interior capacity survives a round trip.

use std::collections::HashSet;
use std::time::Duration;

use wingfoil::pool::Pooled;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// A producer loans, writes and sends; the graph receives every value, in
/// order — and the recorded buffer addresses prove the pool recycled the same
/// two buffers the whole run rather than allocating per message.
#[test]
fn pooled_channel_delivers_all_values_and_recycles_buffers() {
    let g = GraphBuilder::new();
    let (values, mut sender) = g.pooled_channel::<u64>(2);
    let acc = values
        .map(|b: &Burst<Pooled<u64>>| b.iter().map(|p| **p).collect::<Burst<u64>>())
        .collapse_accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        let mut addrs = HashSet::new();
        for i in 1..=10u64 {
            // Blocks once both buffers are out — the pool's backpressure —
            // and resumes as the graph returns them.
            let mut loan = sender.loan();
            *loan = i;
            addrs.insert(&*loan as *const u64 as usize);
            sender.send(loan);
            std::thread::sleep(Duration::from_millis(2));
        }
        addrs
    });
    r.run(RunMode::RealTime, RunFor::Cycles(100)).unwrap();
    let addrs = producer.join().expect("producer thread");

    assert_eq!(
        (1..=10).collect::<Vec<u64>>(),
        r.value(&acc),
        "all values, in order"
    );
    assert!(
        addrs.len() <= 2,
        "capacity-2 pool must recycle the same buffers, saw {} distinct",
        addrs.len()
    );
}

/// A **flat-out** producer (no pacing, pool much smaller than the message
/// count) must not wedge. This is the regression test for a real deadlock:
/// the producer could loan+send the whole pool before the graph's first
/// cycle, the entire burst then parked in the source's value slot, and with
/// `loan()` blocked no new send would ever overwrite it. The fix is the
/// exhausted-pool checkpoint nudge + the receiver dropping its parked burst
/// on a quiet wake; this test runs the exact wedge scenario to completion.
#[test]
fn flat_out_producer_does_not_wedge() {
    let g = GraphBuilder::new();
    let (values, mut sender) = g.pooled_channel::<u64>(4);
    let acc = values
        .map(|b: &Burst<Pooled<u64>>| b.iter().map(|p| **p).collect::<Burst<u64>>())
        .collapse_accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        for i in 1..=200u64 {
            let mut loan = sender.loan();
            *loan = i;
            sender.send(loan);
        }
        sender.close();
    });
    r.run(RunMode::RealTime, RunFor::Forever).unwrap();
    producer.join().expect("producer thread");

    assert_eq!(
        (1..=200).collect::<Vec<u64>>(),
        r.value(&acc),
        "every value delivered, in order, despite constant pool exhaustion"
    );
}

/// Timestamped loans replay deterministically on the graph clock, with
/// same-time sends grouped into one burst — the channel contract, unchanged
/// by pooling.
#[test]
fn pooled_channel_replays_deterministically_in_historical_mode() {
    let g = GraphBuilder::new();
    let (values, mut sender) = g.pooled_channel::<u64>(4);
    let acc = values
        .map(|b: &Burst<Pooled<u64>>| b.iter().map(|p| **p).collect::<Vec<u64>>())
        .with_time()
        .accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        for (v, t) in [(10, 100), (20, 200), (21, 200), (30, 300)] {
            let mut loan = sender.loan();
            *loan = v;
            sender.send_at(loan, NanoTime::new(t));
        }
        sender.close();
    });
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    producer.join().expect("producer thread");

    assert_eq!(
        vec![
            (NanoTime::new(100), vec![10]),
            (NanoTime::new(200), vec![20, 21]),
            (NanoTime::new(300), vec![30]),
        ],
        r.value(&acc)
    );
}

/// `try_loan` surfaces the loan budget without blocking: `None` at capacity,
/// available again once an outstanding loan is dropped (its buffer rides the
/// return queue back).
#[test]
fn try_loan_reports_exhaustion_and_recovery() {
    let g = GraphBuilder::new();
    let (_values, mut sender) = g.pooled_channel::<u64>(1);

    let first = sender.try_loan();
    assert!(first.is_some(), "one buffer available");
    assert!(
        sender.try_loan().is_none(),
        "capacity 1: second loan refused"
    );
    assert_eq!(0, sender.available());

    drop(first);
    assert_eq!(1, sender.available(), "dropped loan returned to the pool");
    assert!(sender.try_loan().is_some());
}

/// Interior capacity survives recycling — the property that makes a warmed
/// pool allocation-free: `pooled_channel_with` pre-sizes each buffer, and a
/// loan→drop round trip hands back the same heap block, capacity intact.
#[test]
fn interior_capacity_survives_recycling() {
    let g = GraphBuilder::new();
    let (_values, mut sender) = g.pooled_channel_with::<Vec<u64>, _>(1, || Vec::with_capacity(64));

    let mut loan = sender.loan();
    assert!(loan.capacity() >= 64, "pre-sized by init");
    loan.clear();
    loan.extend(0..10);
    drop(loan);

    let recycled = sender.loan();
    assert!(
        recycled.capacity() >= 64,
        "recycling keeps the heap block — capacity {} lost",
        recycled.capacity()
    );
}

/// A producer error aborts the receiving graph's run with channel context —
/// the fallible-cycle path, unchanged by pooling.
#[test]
fn pooled_channel_error_aborts_the_run() {
    let g = GraphBuilder::new();
    let (values, sender) = g.pooled_channel::<u64>(2);
    let _acc = values
        .map(|b: &Burst<Pooled<u64>>| b.len() as u64)
        .accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send_error(anyhow::anyhow!("venue disconnected"));
    });
    let result = r.run(RunMode::RealTime, RunFor::Cycles(50));
    producer.join().expect("producer thread");

    let err = format!(
        "{:#}",
        result.expect_err("producer error must abort the run")
    );
    assert!(
        err.contains("venue disconnected"),
        "error carries the producer's context: {err}"
    );
}

/// A passive (pre-first-tick) read observes the empty default handle — the
/// documented `get() == None` state — rather than junk or a crash: sample a
/// pooled stream on a trigger that fires before the producer sends anything.
#[test]
fn pre_first_tick_passive_read_sees_the_empty_handle() {
    let g = GraphBuilder::new();
    let (values, sender) = g.pooled_channel::<u64>(2);
    let latest = values.map(|b: &Burst<Pooled<u64>>| b.last().cloned().unwrap_or_default());
    let sampled = latest
        .sample(&g.ticker(Duration::from_millis(1)))
        .map(|p: &Pooled<u64>| p.get().copied());
    let acc = sampled.accumulate();
    let mut r = g.build();

    // No sends at all: every sampled read is the pre-first-tick default.
    drop(sender);
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();

    let got = r.value(&acc);
    assert!(!got.is_empty(), "ticker sampled at least once");
    assert!(got.iter().all(|v| v.is_none()), "all reads empty: {got:?}");
}
