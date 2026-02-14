use chrono::DateTime;
use chrono::naive::NaiveDateTime;
use derive_more::Display;
use derive_new::new;
use formato::Formato;
use once_cell::sync::Lazy;
use quanta::Clock;
use serde::{Deserialize, Serialize};
use std::convert::From;
use std::ops::{Add, Mul, Sub};
use std::time::Duration;

type RawTime = u64;

static CLOCK: Lazy<Clock> = Lazy::new(Clock::new);

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
        Self(CLOCK.now().as_u64())
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

impl TryFrom<NaiveDateTime> for NanoTime {
    type Error = anyhow::Error;
    fn try_from(date_time: NaiveDateTime) -> anyhow::Result<Self> {
        let t = date_time.and_utc().timestamp_nanos_opt().ok_or_else(|| {
            anyhow::anyhow!("timestamp out of range for nanosecond representation")
        })?;
        Ok(NanoTime(t as RawTime))
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

impl TryFrom<NanoTime> for NaiveDateTime {
    type Error = anyhow::Error;
    fn try_from(t: NanoTime) -> anyhow::Result<Self> {
        DateTime::from_timestamp((t.0 / 1_000_000_000) as i64, (t.0 % 1_000_000_000) as u32)
            .map(|dt| dt.naive_utc())
            .ok_or_else(|| anyhow::anyhow!("NanoTime out of range for DateTime"))
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
