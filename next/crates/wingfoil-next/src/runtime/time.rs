use chrono::DateTime;
use chrono::naive::NaiveDateTime;
use derive_more::Display;
use derive_new::new;
use formato::Formato;
use quanta::Clock;
use serde::{Deserialize, Serialize};
use std::convert::From;
use std::ops::{Add, Mul, Sub};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type RawTime = u64;

static CLOCK: LazyLock<Clock> = LazyLock::new(Clock::new);

/// Nanoseconds added to a raw `quanta` reading to convert it to nanoseconds
/// since the unix epoch.
///
/// `quanta::Clock::now()` is monotonic (nanoseconds since an arbitrary anchor,
/// effectively boot time), but [`NanoTime`] documents "nanoseconds since the
/// unix epoch". We snap `SystemTime::now()` against the monotonic clock once,
/// at first use, and add the resulting offset to every subsequent reading. This
/// keeps quanta's cheap, TSC-based reads while anchoring them to the documented
/// epoch, so timestamps persisted in real-time mode (kdb/Postgres/CSV writes,
/// cross-host latency stamps) are correct as absolute times.
static EPOCH_OFFSET_NANOS: LazyLock<u64> = LazyLock::new(|| {
    let mono = CLOCK.now().as_u64();
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("invariant: system clock is later than the unix epoch")
        .as_nanos() as u64;
    // Wall clock is ~decades ahead of the monotonic anchor, so this never
    // saturates in practice; `saturating_sub` only guards a clock set to 1970.
    wall.saturating_sub(mono)
});

/// A time in nanoseconds since the unix epoch.
#[derive(
    new,
    Display,
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
)]
pub struct NanoTime(RawTime);

impl NanoTime {
    pub const ZERO: Self = Self(0);
    pub const MAX: Self = Self(RawTime::MAX);
    pub const NANOS_PER_SECOND: RawTime = 1_000_000_000;
    pub const SECONDS_PER_NANO: f64 = 1e-9;

    /// KDB epoch is 2000-01-01, Unix epoch is 1970-01-01
    /// Difference: 946684800 seconds = 946684800000000000 nanoseconds
    const KDB_EPOCH_OFFSET_NANOS: i64 = 946_684_800_000_000_000;

    pub fn now() -> Self {
        Self(CLOCK.now().as_u64() + *EPOCH_OFFSET_NANOS)
    }

    pub fn pretty(&self) -> String {
        (self.0 as f64 * Self::SECONDS_PER_NANO).formato("#,###.000_000")
    }

    /// Create a NanoTime from a KDB timestamp (nanoseconds from 2000-01-01).
    pub fn from_kdb_timestamp(kdb_nanos: i64) -> Self {
        Self::new((kdb_nanos + Self::KDB_EPOCH_OFFSET_NANOS) as u64)
    }

    /// Convert to KDB timestamp (nanoseconds from 2000-01-01).
    pub fn to_kdb_timestamp(self) -> i64 {
        if self.0 == RawTime::MAX {
            i64::MAX
        } else {
            self.0 as i64 - Self::KDB_EPOCH_OFFSET_NANOS
        }
    }
}

impl From<u128> for NanoTime {
    fn from(t: u128) -> Self {
        NanoTime(t as RawTime)
    }
}

impl From<u64> for NanoTime {
    fn from(t: u64) -> Self {
        NanoTime(t as RawTime)
    }
}

impl From<f64> for NanoTime {
    fn from(t: f64) -> Self {
        NanoTime(t as RawTime)
    }
}

impl From<i64> for NanoTime {
    fn from(t: i64) -> Self {
        NanoTime(t as RawTime)
    }
}

impl From<Duration> for NanoTime {
    fn from(dur: Duration) -> Self {
        Self(dur.as_secs() as RawTime * 1_000_000_000 + dur.subsec_nanos() as RawTime)
    }
}

impl From<NaiveDateTime> for NanoTime {
    fn from(date_time: NaiveDateTime) -> Self {
        // `timestamp_nanos_opt` only returns None for dates outside ~1677..2262.
        let t = date_time
            .and_utc()
            .timestamp_nanos_opt()
            .expect("NanoTime supports years 1677..=2262");
        NanoTime(t as RawTime)
    }
}

impl From<NanoTime> for f64 {
    fn from(t: NanoTime) -> Self {
        t.0 as f64
    }
}

impl From<NanoTime> for u64 {
    fn from(t: NanoTime) -> Self {
        t.0
    }
}

impl From<NanoTime> for NaiveDateTime {
    fn from(t: NanoTime) -> Self {
        // Only fails for seconds outside chrono's representable range; NanoTime's
        // u64 nanos can't reach those bounds.
        DateTime::from_timestamp((t.0 / 1_000_000_000) as i64, (t.0 % 1_000_000_000) as u32)
            .expect("NanoTime is always within chrono's representable range")
            .naive_utc()
    }
}

impl From<NanoTime> for Duration {
    fn from(t: NanoTime) -> Self {
        Duration::from_nanos(u64::from(t))
    }
}

impl Add<NanoTime> for NanoTime {
    type Output = Self;
    fn add(self, other: Self) -> Self::Output {
        Self(self.0 + other.0)
    }
}

impl Add<RawTime> for NanoTime {
    type Output = Self;
    fn add(self, other: RawTime) -> Self::Output {
        Self(self.0 + other)
    }
}

impl Add<Duration> for NanoTime {
    type Output = Self;
    fn add(self, other: Duration) -> Self::Output {
        Self(self.0 + other.as_nanos() as RawTime)
    }
}

impl Sub<NanoTime> for NanoTime {
    type Output = Self;
    fn sub(self, other: Self) -> Self::Output {
        Self(self.0 - other.0)
    }
}

impl Mul<u32> for NanoTime {
    type Output = Self;
    fn mul(self, other: u32) -> Self::Output {
        Self(self.0 * other as RawTime)
    }
}

impl Mul<i32> for NanoTime {
    type Output = Self;
    fn mul(self, other: i32) -> Self::Output {
        Self(self.0 * other as RawTime)
    }
}

impl Mul<u64> for NanoTime {
    type Output = Self;
    fn mul(self, other: u64) -> Self::Output {
        Self(self.0 * other as RawTime)
    }
}

impl Mul<i64> for NanoTime {
    type Output = Self;
    fn mul(self, other: i64) -> Self::Output {
        Self(self.0 * other as RawTime)
    }
}

#[cfg(test)]
mod tests {
    use super::NanoTime;
    use chrono::Datelike;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn now_is_anchored_to_the_unix_epoch() {
        // `now()` must return nanoseconds since the unix epoch (its documented
        // contract), not quanta's boot-relative monotonic reading. Compare
        // against the wall clock; a few seconds of slack covers scheduling.
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        let now = u64::from(NanoTime::now());
        let diff = now.abs_diff(wall);
        assert!(
            diff < 5_000_000_000,
            "now() ({now}) is not within 5s of the wall clock ({wall}); diff {diff}ns"
        );
    }

    #[test]
    fn now_is_monotonic_non_decreasing() {
        let a = NanoTime::now();
        let b = NanoTime::now();
        assert!(b >= a, "now() went backwards: {a} then {b}");
    }

    #[test]
    fn kdb_timestamp_roundtrip() {
        let t = NanoTime::new(1_600_000_000_000_000_000);
        assert_eq!(t, NanoTime::from_kdb_timestamp(t.to_kdb_timestamp()));
    }

    #[test]
    fn kdb_max_converts_to_i64_max() {
        assert_eq!(NanoTime::MAX.to_kdb_timestamp(), i64::MAX);
    }

    #[test]
    fn kdb_epoch_offset_lands_on_year_2000() {
        // KDB timestamp 0 = 2000-01-01 00:00:00 UTC
        let t = NanoTime::from_kdb_timestamp(0);
        let dt: chrono::naive::NaiveDateTime = t.into();
        assert_eq!(dt.year(), 2000);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 1);
    }

    #[test]
    fn duration_roundtrip() {
        let d = Duration::from_nanos(123_456_789);
        let t = NanoTime::from(d);
        let d2 = Duration::from(t);
        assert_eq!(d, d2);
    }

    #[test]
    fn arithmetic_add_sub_mul() {
        let a = NanoTime::new(300);
        let b = NanoTime::new(100);
        assert_eq!(a + b, NanoTime::new(400));
        assert_eq!(a - b, NanoTime::new(200));
        assert_eq!(b * 3u32, NanoTime::new(300));
        assert_eq!(b * 4u64, NanoTime::new(400));
    }

    #[test]
    fn ordering() {
        assert!(NanoTime::new(100) > NanoTime::new(50));
        assert!(NanoTime::ZERO < NanoTime::MAX);
        assert_eq!(NanoTime::new(42), NanoTime::new(42));
    }

    #[test]
    fn from_u64_roundtrip() {
        let raw: u64 = 999_888_777;
        assert_eq!(u64::from(NanoTime::from(raw)), raw);
    }

    #[test]
    fn from_u128_truncates_to_u64() {
        let raw: u128 = 1_234_567_890;
        let t = NanoTime::from(raw);
        assert_eq!(u64::from(t), raw as u64);
    }

    #[test]
    fn from_f64_truncates() {
        let f: f64 = 1_000_000_000.0;
        let t = NanoTime::from(f);
        assert_eq!(u64::from(t), 1_000_000_000u64);
    }

    #[test]
    fn from_i64_converts() {
        let i: i64 = 500_000_000;
        let t = NanoTime::from(i);
        assert_eq!(u64::from(t), 500_000_000u64);
    }

    #[test]
    fn from_naive_datetime_roundtrips() {
        use chrono::naive::NaiveDateTime;
        let t = NanoTime::new(1_600_000_000_000_000_000);
        let dt: NaiveDateTime = t.into();
        let t2 = NanoTime::from(dt);
        // NaiveDateTime has nanosecond precision so roundtrip is exact
        assert_eq!(t, t2);
    }

    #[test]
    fn into_f64_converts() {
        let t = NanoTime::new(42);
        let f: f64 = t.into();
        assert_eq!(f, 42.0_f64);
    }

    #[test]
    fn mul_i32_and_i64() {
        let t = NanoTime::new(100);
        assert_eq!(t * 3i32, NanoTime::new(300));
        assert_eq!(t * 5i64, NanoTime::new(500));
    }

    #[test]
    fn pretty_returns_formatted_string() {
        let t = NanoTime::new(1_500_000_000);
        let s = t.pretty();
        // 1_500_000_000 ns * 1e-9 = 1.5 seconds
        assert!(s.contains("1.500"), "expected 1.500 in '{s}'");
    }
}
