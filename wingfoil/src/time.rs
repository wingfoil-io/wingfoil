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
#[derive(new, Display, Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct NanoTime(RawTime);

impl NanoTime {
    pub const ZERO: Self = Self(0);
    pub const MAX: Self = Self(RawTime::MAX);
    pub const NANOS_PER_SECOND: RawTime = 1_000_000_000;
    pub const SECONDS_PER_NANO: f64 = 1e-9;

    pub fn now() -> Self {
        Self(CLOCK.now().as_u64())
    }
    pub fn pretty(&self) -> String {
        (self.0 as f64 * Self::SECONDS_PER_NANO).formato("#,###.000_000")
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
        let t = date_time.and_utc().timestamp_nanos_opt().unwrap();
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
        DateTime::from_timestamp((t.0 / 1_000_000_000) as i64, (t.0 % 1_000_000_000) as u32)
            .unwrap()
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
