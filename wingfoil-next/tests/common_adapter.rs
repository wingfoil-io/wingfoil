//! `common` adapter parity tests.
//!
//! The classic `wingfoil::adapters::common` module is a set of **pure helpers**
//! (no graph node, no `Op`, no `cycle`), so these are value-parity tests over
//! the same inputs the classic crate's own unit tests exercise: they assert the
//! ported [`TimeWindow`] / [`WindowFilter`] and the [`compute_time_slices`] /
//! [`compute_validated_time_slices`] time slicer produce byte-identical results.
//! Being pure, they need no engine run (there are no tick times to compare —
//! the helpers never touch the graph clock); the classic time-slicing tests are
//! ported verbatim below.

use std::time::Duration;

use wingfoil::NanoTime;
use wingfoil_next::adapters::common::{
    TimeWindow, WindowFilter, compute_time_slices, compute_validated_time_slices,
};

const DAY_NANOS: u64 = 86_400_000_000_000;

fn kdb_epoch() -> NanoTime {
    // 2000-01-01 midnight = kdb_date 0
    NanoTime::from_kdb_timestamp(0)
}

// ---- TimeWindow / WindowFilter -------------------------------------------

#[test]
fn time_window_contains_is_half_open() {
    let w = TimeWindow::clamp(
        NanoTime::new(10),
        NanoTime::new(20),
        NanoTime::ZERO,
        NanoTime::new(100),
    );
    assert!(!w.contains(NanoTime::new(9)));
    assert!(w.contains(NanoTime::new(10)), "lo is inclusive");
    assert!(w.contains(NanoTime::new(19)));
    assert!(!w.contains(NanoTime::new(20)), "hi is exclusive");
    assert_eq!(w.lo(), NanoTime::new(10));
    assert_eq!(w.hi(), NanoTime::new(20));
}

#[test]
fn time_window_clamp_tightens_to_run_bounds() {
    // Slice [5, 200) clamped to run bounds [10, 100) → [10, 100).
    let w = TimeWindow::clamp(
        NanoTime::new(5),
        NanoTime::new(200),
        NanoTime::new(10),
        NanoTime::new(100),
    );
    assert_eq!(w.lo(), NanoTime::new(10), "lo raised to start");
    assert_eq!(w.hi(), NanoTime::new(100), "hi lowered to end");
}

#[test]
fn window_filter_keeps_in_window_and_counts_drops() {
    let w = TimeWindow::clamp(
        NanoTime::new(10),
        NanoTime::new(20),
        NanoTime::ZERO,
        NanoTime::new(100),
    );
    let mut filter = WindowFilter::new("test_adapter", w);

    assert!(!filter.keep(NanoTime::new(5)), "before window: dropped");
    assert!(filter.keep(NanoTime::new(10)), "at lo: kept");
    assert!(filter.keep(NanoTime::new(15)), "inside: kept");
    assert!(
        !filter.keep(NanoTime::new(20)),
        "at hi (exclusive): dropped"
    );
    assert!(!filter.keep(NanoTime::new(25)), "after window: dropped");

    assert_eq!(filter.dropped(), 3);
    // finish() consumes self; with drops it prints a single stderr summary.
    filter.finish();
}

#[test]
fn window_filter_no_drops_stays_quiet() {
    let w = TimeWindow::clamp(
        NanoTime::ZERO,
        NanoTime::new(100),
        NanoTime::ZERO,
        NanoTime::new(100),
    );
    let mut filter = WindowFilter::new("test_adapter", w);
    for t in [0u64, 1, 50, 99] {
        assert!(filter.keep(NanoTime::new(t)));
    }
    assert_eq!(filter.dropped(), 0);
    filter.finish();
}

// ---- Time slicing (ported verbatim from classic common.rs) ---------------

#[test]
fn test_compute_time_slices_no_stub() {
    // 8-hour period divides 24h evenly → 3 slices, no stub.
    let epoch = kdb_epoch();
    let period = Duration::from_secs(8 * 3600);
    let start = epoch;
    let end = NanoTime::new(u64::from(epoch) + DAY_NANOS - 1);

    let slices = compute_time_slices(start, end, period);
    assert_eq!(slices.len(), 3, "expected 3 slices for 8h period");

    for &(_, date, _) in &slices {
        assert_eq!(date, 0);
    }

    let period_nanos = period.as_nanos() as u64;

    // Slice 0: [midnight, midnight + period)
    let (t0_0, t1_0) = slices[0].0;
    assert_eq!(u64::from(t0_0), u64::from(epoch));
    assert_eq!(u64::from(t1_0), u64::from(epoch) + period_nanos);

    // Slice 1: [midnight + period, midnight + 2*period)
    let (t0_1, t1_1) = slices[1].0;
    assert_eq!(u64::from(t0_1), u64::from(epoch) + period_nanos);
    assert_eq!(u64::from(t1_1), u64::from(epoch) + 2 * period_nanos);

    // Last slice: [midnight + 2*period, next_midnight) — t1 is next_midnight (round)
    let (t0_2, t1_2) = slices[2].0;
    assert_eq!(u64::from(t0_2), u64::from(epoch) + 2 * period_nanos);
    assert_eq!(u64::from(t1_2), u64::from(epoch) + DAY_NANOS);

    assert_eq!(slices[0].2, 0);
    assert_eq!(slices[1].2, 1);
    assert_eq!(slices[2].2, 2);
}

#[test]
fn test_compute_time_slices_with_stub() {
    // 5-hour period: 24h / 5h = 4 full slices + 4h stub → 5 slices total
    let epoch = kdb_epoch();
    let period = Duration::from_secs(5 * 3600);
    let start = epoch;
    let end = NanoTime::new(u64::from(epoch) + DAY_NANOS - 1);

    let slices = compute_time_slices(start, end, period);
    assert_eq!(slices.len(), 5, "expected 4 full + 1 stub = 5 slices");

    let period_nanos = period.as_nanos() as u64;

    // Stub: t0 = 20h (round), t1 = next midnight (round: 86400000000000j)
    let (stub_t0, stub_t1) = slices[4].0;
    assert_eq!(u64::from(stub_t0), u64::from(epoch) + 4 * period_nanos);
    assert_eq!(u64::from(stub_t1), u64::from(epoch) + DAY_NANOS);

    // Contiguous: t1 of slice i == t0 of slice i+1 (half-open [t0, t1) intervals)
    for i in 0..4 {
        let t1_i = u64::from(slices[i].0.1);
        let t0_next = u64::from(slices[i + 1].0.0);
        assert_eq!(t1_i, t0_next, "boundary mismatch at slice {i}/{}", i + 1);
    }
}

#[test]
fn test_compute_time_slices_two_days() {
    // Two days → iterations reset on day 1
    let epoch = kdb_epoch();
    let period = Duration::from_secs(12 * 3600); // 2 slices/day
    let start = epoch;
    let end = NanoTime::new(u64::from(epoch) + 2 * DAY_NANOS - 1);

    let slices = compute_time_slices(start, end, period);
    assert_eq!(slices.len(), 4); // 2 slices × 2 days

    assert_eq!(slices[0].1, 0);
    assert_eq!(slices[0].2, 0);
    assert_eq!(slices[1].1, 0);
    assert_eq!(slices[1].2, 1);
    assert_eq!(slices[2].1, 1);
    assert_eq!(slices[2].2, 0); // resets to 0 on new day
    assert_eq!(slices[3].1, 1);
    assert_eq!(slices[3].2, 1);
}

/// Start at 23:59:30 on day 0, end at 23:59:59 — only the tail of the day.
///
/// The function must not generate slices from midnight; it should begin at
/// the period boundary that contains `start_time`.
#[test]
fn test_compute_time_slices_mid_day_start() {
    // 23:59:30 = 86370 seconds from midnight in KDB nanos
    const SECS_23_59_30: i64 = 86_370 * 1_000_000_000;
    const SECS_23_59_59: i64 = 86_399 * 1_000_000_000;
    const DAY_NANOS: i64 = 86_400_000_000_000;

    let start = NanoTime::from_kdb_timestamp(SECS_23_59_30);
    let end = NanoTime::from_kdb_timestamp(SECS_23_59_59);
    let next_midnight = NanoTime::from_kdb_timestamp(DAY_NANOS);

    // --- 60s period: one slice [23:59:00, 00:00:00), iteration 1439 ---
    let slices = compute_time_slices(start, end, Duration::from_secs(60));
    assert_eq!(slices.len(), 1, "60s: expected 1 slice");
    let (t0, t1) = slices[0].0;
    assert_eq!(
        t0,
        NanoTime::from_kdb_timestamp(86_340 * 1_000_000_000),
        "60s: t0 should be 23:59:00"
    );
    assert_eq!(t1, next_midnight, "60s: t1 should be midnight");
    assert_eq!(slices[0].2, 1439, "60s: iteration should be 1439");

    // --- 30s period: one slice [23:59:30, 00:00:00), iteration 2879 ---
    let slices = compute_time_slices(start, end, Duration::from_secs(30));
    assert_eq!(slices.len(), 1, "30s: expected 1 slice");
    let (t0, t1) = slices[0].0;
    assert_eq!(t0, start, "30s: t0 should be 23:59:30");
    assert_eq!(t1, next_midnight, "30s: t1 should be midnight");
    assert_eq!(slices[0].2, 2879, "30s: iteration should be 2879");

    // --- 10s period: three slices starting at 23:59:30 ---
    let slices = compute_time_slices(start, end, Duration::from_secs(10));
    assert_eq!(slices.len(), 3, "10s: expected 3 slices");
    assert_eq!(slices[0].0.0, start, "10s: first t0 should be 23:59:30");
    assert_eq!(
        slices[0].0.1,
        NanoTime::from_kdb_timestamp(86_380 * 1_000_000_000),
        "10s: first t1 should be 23:59:40"
    );
    assert_eq!(
        slices[1].0.0,
        NanoTime::from_kdb_timestamp(86_380 * 1_000_000_000),
        "10s: second t0 should be 23:59:40"
    );
    assert_eq!(
        slices[2].0.1, next_midnight,
        "10s: last t1 should be midnight"
    );
    assert_eq!(slices[0].2, 8637, "10s: first iteration should be 8637");
}

/// When `end_time` lands exactly on a midnight boundary, no extra empty slice
/// for the following day should be generated.
#[test]
fn test_compute_time_slices_exact_midnight_boundary() {
    let epoch = kdb_epoch();
    let period = Duration::from_secs(8 * 3600); // 3 slices per day

    let end = NanoTime::new(u64::from(epoch) + DAY_NANOS);
    let slices = compute_time_slices(epoch, end, period);
    assert_eq!(
        slices.len(),
        3,
        "exact midnight end should yield 3 slices (day 0 only)"
    );
    for &(_, date, _) in &slices {
        assert_eq!(date, 0, "all slices should be on day 0");
    }
}

/// end_time just past midnight must generate exactly one slice on day 1, not a full day.
#[test]
fn test_compute_time_slices_end_past_midnight() {
    const HOUR_NANOS: u64 = 3_600_000_000_000;
    let epoch = kdb_epoch();
    let period = Duration::from_secs(3600);
    let start = epoch;
    // 30 minutes into day 1
    let end = NanoTime::new(u64::from(epoch) + DAY_NANOS + 30 * 60 * 1_000_000_000);

    let slices = compute_time_slices(start, end, period);

    // 24 full-hour slices on day 0 + 1 slice [00:00,01:00) on day 1
    assert_eq!(slices.len(), 25, "expected 25 slices");

    let last = slices.last().expect("25 slices means a last one exists");
    assert_eq!(last.1, 1, "last slice should be on kdb_date 1");
    assert_eq!(last.2, 0, "last slice should be iteration 0 of day 1");
    assert_eq!(
        last.0.0,
        NanoTime::new(u64::from(epoch) + DAY_NANOS),
        "last slice t0 should be day-1 midnight"
    );
    assert_eq!(
        last.0.1,
        NanoTime::new(u64::from(epoch) + DAY_NANOS + HOUR_NANOS),
        "last slice t1 should be day-1 01:00"
    );
}

/// Crossing midnight with a mid-day start must not over-generate.
#[test]
fn test_compute_time_slices_cross_midnight() {
    const HOUR_NANOS: u64 = 3_600_000_000_000;
    let epoch = kdb_epoch();
    let period = Duration::from_secs(3600);
    let start = NanoTime::new(u64::from(epoch) + 23 * HOUR_NANOS);
    let end = NanoTime::new(u64::from(epoch) + DAY_NANOS + 30 * 60 * 1_000_000_000);

    let slices = compute_time_slices(start, end, period);

    assert_eq!(slices.len(), 2, "expected 2 slices");

    // Slice 0: [23:00, midnight) on day 0
    assert_eq!(slices[0].1, 0);
    assert_eq!(slices[0].2, 23);
    assert_eq!(slices[0].0.0, start);
    assert_eq!(slices[0].0.1, NanoTime::new(u64::from(epoch) + DAY_NANOS));

    // Slice 1: [midnight, 01:00) on day 1
    assert_eq!(slices[1].1, 1);
    assert_eq!(slices[1].2, 0);
    assert_eq!(slices[1].0.0, NanoTime::new(u64::from(epoch) + DAY_NANOS));
    assert_eq!(
        slices[1].0.1,
        NanoTime::new(u64::from(epoch) + DAY_NANOS + HOUR_NANOS)
    );
}

#[test]
fn test_validated_rejects_zero_period() {
    let err = compute_validated_time_slices(
        "test_adapter",
        kdb_epoch(),
        Ok(NanoTime::new(u64::from(kdb_epoch()) + DAY_NANOS)),
        Duration::ZERO,
    )
    .unwrap_err();
    assert!(err.to_string().contains("period must be greater than zero"));
    assert!(err.to_string().contains("test_adapter"));
}

#[test]
fn test_validated_rejects_zero_start() {
    let err = compute_validated_time_slices(
        "test_adapter",
        NanoTime::ZERO,
        Ok(NanoTime::new(DAY_NANOS)),
        Duration::from_secs(3600),
    )
    .unwrap_err();
    assert!(err.to_string().contains("start_time is NanoTime::ZERO"));
}

#[test]
fn test_validated_rejects_forever_and_cycles() {
    // RunFor::Forever arrives as Ok(NanoTime::MAX).
    let err = compute_validated_time_slices(
        "test_adapter",
        kdb_epoch(),
        Ok(NanoTime::MAX),
        Duration::from_secs(3600),
    )
    .unwrap_err();
    assert!(err.to_string().contains("RunFor::Forever"));

    // RunFor::Cycles arrives as Err.
    let err = compute_validated_time_slices(
        "test_adapter",
        kdb_epoch(),
        Err(anyhow::anyhow!("end_time not available for RunFor::Cycles")),
        Duration::from_secs(3600),
    )
    .unwrap_err();
    assert!(err.to_string().contains("RunFor::Cycles"));
}

#[test]
fn test_validated_passes_through_to_compute() {
    let slices = compute_validated_time_slices(
        "test_adapter",
        kdb_epoch(),
        Ok(NanoTime::new(u64::from(kdb_epoch()) + DAY_NANOS)),
        Duration::from_secs(8 * 3600),
    )
    .unwrap();
    assert_eq!(slices.len(), 3);
}

/// end_time exactly on a period boundary, one period into day 1.
#[test]
fn test_compute_time_slices_end_on_period_boundary_day1() {
    const HOUR_NANOS: u64 = 3_600_000_000_000;
    let epoch = kdb_epoch();
    let period = Duration::from_secs(2 * 3600); // 2-hour periods
    let start = epoch;
    // end exactly at 02:00 on day 1 (on a period boundary)
    let end = NanoTime::new(u64::from(epoch) + DAY_NANOS + 2 * HOUR_NANOS);

    let slices = compute_time_slices(start, end, period);

    // 12 slices on day 0 + 1 slice [00:00, 02:00) on day 1
    assert_eq!(slices.len(), 13, "expected 13 slices");
    assert_eq!(slices.last().expect("13 slices").1, 1);
    assert_eq!(slices.last().expect("13 slices").2, 0);
}
