//! Shared time-slicing logic for time-partitioned database reads.
//!
//! Both the KDB+ (`kdb_read`) and PostgreSQL (`postgres_read`) adapters drive
//! historical reads by splitting the run's `[start, end)` window into contiguous,
//! half-open time slices and issuing one query per slice. This module owns that
//! computation so the two adapters share identical slicing semantics.
//!
//! Slices are returned as `((t0, t1), day, iteration)`:
//! - `[t0, t1)` is the half-open time interval — use `time >= t0, time < t1` in
//!   the query filter so boundaries are clean, round numbers.
//! - `day` is the KDB-style date integer (days since 2000-01-01) that contains
//!   the slice — useful for date-partitioned tables; adapters that don't
//!   partition by date can ignore it.
//! - `iteration` is the slice index within its day (resets to 0 each new day).

use crate::types::NanoTime;

/// Validate the run window and period, then compute the time slices.
///
/// Shared front door for time-sliced readers (`kdb_read`, `kdb_read_cached`,
/// `postgres_read`): enforces the preconditions of [`compute_time_slices`] with
/// uniform error messages (prefixed with `adapter` so callers see the function
/// they actually called), then delegates to it.
///
/// * `period` must be non-zero (a zero period cannot advance through the window).
/// * `start_time` must be non-zero — i.e. `RunMode::HistoricalFrom` with an explicit start.
/// * `end_time` must be a bounded time from `RunFor::Duration`; `Forever` arrives as
///   `Ok(NanoTime::MAX)` and `Cycles` as `Err`, both rejected.
pub(crate) fn compute_validated_time_slices(
    adapter: &str,
    start_time: NanoTime,
    end_time: anyhow::Result<NanoTime>,
    period: std::time::Duration,
) -> anyhow::Result<Vec<((NanoTime, NanoTime), i32, usize)>> {
    if period.is_zero() {
        anyhow::bail!("{adapter}: period must be greater than zero");
    }
    if start_time == NanoTime::ZERO {
        anyhow::bail!(
            "{adapter}: start_time is NanoTime::ZERO; \
            use RunMode::HistoricalFrom with an explicit start time"
        );
    }
    let end_time = match end_time {
        Ok(t) if t == NanoTime::MAX => anyhow::bail!(
            "{adapter} requires RunFor::Duration; \
            RunFor::Forever would generate an unbounded number of slices"
        ),
        Ok(t) => t,
        Err(_) => anyhow::bail!(
            "{adapter} requires RunFor::Duration; \
            RunFor::Cycles does not provide an end time"
        ),
    };
    Ok(compute_time_slices(start_time, end_time, period))
}

/// Split `[start_time, end_time)` into contiguous half-open slices of length `period`.
///
/// Slices never straddle a midnight boundary: the final slice of each day clamps
/// its end to the next midnight (also a round number), and slicing resumes at
/// midnight on the following day with `iteration` reset to 0. On the first day,
/// slicing begins at the period boundary that contains `start_time` rather than
/// at midnight, so a mid-day start does not generate empty leading slices.
///
/// `start_time` must be non-zero (callers validate this) and `end_time` must be a
/// bounded time (i.e. `RunFor::Duration`, not `Forever`).
pub(crate) fn compute_time_slices(
    start_time: NanoTime,
    end_time: NanoTime,
    period: std::time::Duration,
) -> Vec<((NanoTime, NanoTime), i32, usize)> {
    const DAY_NANOS: i64 = 86_400_000_000_000;
    let period_nanos = period.as_nanos() as i64;

    let start_kdb = start_time.to_kdb_timestamp();
    let end_kdb = end_time.to_kdb_timestamp();

    let start_day = start_kdb.div_euclid(DAY_NANOS);
    // Subtract 1 before dividing so that an end_time that falls exactly on midnight
    // does not pull in an extra (empty) day. Note end_kdb can be negative for
    // pre-2000 windows (KDB epoch is 2000-01-01) — div_euclid keeps day
    // arithmetic correct there, so don't replace it with plain `/` division.
    let end_day = (end_kdb - 1).div_euclid(DAY_NANOS);

    let mut result = Vec::new();

    for day in start_day..=end_day {
        let kdb_date = day as i32;
        let midnight_kdb = day * DAY_NANOS;
        let next_midnight_kdb = midnight_kdb + DAY_NANOS;

        // For the first day, begin at the period boundary that contains start_time
        // rather than always starting at midnight.
        let mut iteration = if day == start_day {
            ((start_kdb - midnight_kdb) / period_nanos) as usize
        } else {
            0
        };

        loop {
            // Half-open intervals [t0, t1): caller uses `time >= t0, time < t1`.
            // t0 and t1 are always round multiples of period (or midnight),
            // so queries contain only clean numbers with no ±1 adjustments.
            let t0 = midnight_kdb + iteration as i64 * period_nanos;

            // On the last day, stop once the slice start has reached or passed
            // end_time — any further slice would be entirely outside the range.
            if day == end_day && t0 >= end_kdb {
                break;
            }

            let natural_t1 = t0 + period_nanos;
            // For the final slice of the day, t1 clamps to next midnight (also a round
            // number: 86400000000000j per day). For non-final slices t1 = t0 + period.
            let t1 = natural_t1.min(next_midnight_kdb);

            result.push((
                (
                    NanoTime::from_kdb_timestamp(t0),
                    NanoTime::from_kdb_timestamp(t1),
                ),
                kdb_date,
                iteration,
            ));

            if natural_t1 >= next_midnight_kdb {
                break;
            }

            iteration += 1;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kdb_epoch() -> NanoTime {
        // 2000-01-01 midnight = kdb_date 0
        NanoTime::from_kdb_timestamp(0)
    }

    const DAY_NANOS: u64 = 86_400_000_000_000;

    #[test]
    fn test_compute_time_slices_no_stub() {
        // 8-hour period divides 24h evenly → 3 slices, no stub.
        let epoch = kdb_epoch();
        let period = std::time::Duration::from_secs(8 * 3600);
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
        let period = std::time::Duration::from_secs(5 * 3600);
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
        let period = std::time::Duration::from_secs(12 * 3600); // 2 slices/day
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
        let slices = compute_time_slices(start, end, std::time::Duration::from_secs(60));
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
        let slices = compute_time_slices(start, end, std::time::Duration::from_secs(30));
        assert_eq!(slices.len(), 1, "30s: expected 1 slice");
        let (t0, t1) = slices[0].0;
        assert_eq!(t0, start, "30s: t0 should be 23:59:30");
        assert_eq!(t1, next_midnight, "30s: t1 should be midnight");
        assert_eq!(slices[0].2, 2879, "30s: iteration should be 2879");

        // --- 10s period: three slices starting at 23:59:30 ---
        let slices = compute_time_slices(start, end, std::time::Duration::from_secs(10));
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
        let period = std::time::Duration::from_secs(8 * 3600); // 3 slices per day

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
        let period = std::time::Duration::from_secs(3600);
        let start = epoch;
        // 30 minutes into day 1
        let end = NanoTime::new(u64::from(epoch) + DAY_NANOS + 30 * 60 * 1_000_000_000);

        let slices = compute_time_slices(start, end, period);

        // 24 full-hour slices on day 0 + 1 slice [00:00,01:00) on day 1
        assert_eq!(slices.len(), 25, "expected 25 slices");

        let last = slices.last().unwrap();
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
        let period = std::time::Duration::from_secs(3600);
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
            std::time::Duration::ZERO,
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
            std::time::Duration::from_secs(3600),
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
            std::time::Duration::from_secs(3600),
        )
        .unwrap_err();
        assert!(err.to_string().contains("RunFor::Forever"));

        // RunFor::Cycles arrives as Err.
        let err = compute_validated_time_slices(
            "test_adapter",
            kdb_epoch(),
            Err(anyhow::anyhow!("end_time not available for RunFor::Cycles")),
            std::time::Duration::from_secs(3600),
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
            std::time::Duration::from_secs(8 * 3600),
        )
        .unwrap();
        assert_eq!(slices.len(), 3);
    }

    /// end_time exactly on a period boundary, one period into day 1.
    #[test]
    fn test_compute_time_slices_end_on_period_boundary_day1() {
        const HOUR_NANOS: u64 = 3_600_000_000_000;
        let epoch = kdb_epoch();
        let period = std::time::Duration::from_secs(2 * 3600); // 2-hour periods
        let start = epoch;
        // end exactly at 02:00 on day 1 (on a period boundary)
        let end = NanoTime::new(u64::from(epoch) + DAY_NANOS + 2 * HOUR_NANOS);

        let slices = compute_time_slices(start, end, period);

        // 12 slices on day 0 + 1 slice [00:00, 02:00) on day 1
        assert_eq!(slices.len(), 13, "expected 13 slices");
        assert_eq!(slices.last().unwrap().1, 1);
        assert_eq!(slices.last().unwrap().2, 0);
    }
}
