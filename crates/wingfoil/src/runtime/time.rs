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
    /// The Unix epoch, and the conventional start time for a deterministic
    /// historical run (`RunMode::HistoricalFrom(NanoTime::ZERO)`).
    pub const ZERO: Self = Self(0);
    /// The largest representable instant. Used as "never" when comparing
    /// scheduled times, so an empty queue loses every `min`.
    pub const MAX: Self = Self(RawTime::MAX);
    /// Nanoseconds in a second — the unit conversion for the raw count.
    pub const NANOS_PER_SECOND: RawTime = 1_000_000_000;
    /// Seconds in a nanosecond, i.e. the multiplier that turns the raw count
    /// into fractional seconds for display.
    pub const SECONDS_PER_NANO: f64 = 1e-9;

    /// KDB epoch is 2000-01-01, Unix epoch is 1970-01-01
    /// Difference: 946684800 seconds = 946684800000000000 nanoseconds
    const KDB_EPOCH_OFFSET_NANOS: i64 = 946_684_800_000_000_000;

    /// The current wall-clock instant, as nanoseconds since the Unix epoch.
    ///
    /// Reads a coarse-corrected TSC clock, so it costs ~24 ns rather than a
    /// syscall — which is why the kernel still caches one snap per cycle
    /// instead of calling this per node. **Business logic should not call it:**
    /// use [`Ctx::time`](crate::op::Ctx::time), which is source-driven and
    /// replays deterministically.
    pub fn now() -> Self {
        Self(CLOCK.now().as_u64() + *EPOCH_OFFSET_NANOS)
    }

    /// This instant as fractional **seconds** since the epoch, grouped for
    /// readability. For logs and `Display`; lossy, so do not round-trip
    /// through it.
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

    /// A nanosecond count held in an `i64`, **clamping a negative input to
    /// [`ZERO`](Self::ZERO)**.
    ///
    /// Narrow a `u128` nanosecond count, **saturating at
    /// [`MAX`](Self::MAX)** rather than wrapping modulo 2^64.
    ///
    /// Private, and deliberately so: its only caller is
    /// [`From<Duration>`](#impl-From<Duration>-for-NanoTime), where the `u128`
    /// is forced on us by [`Duration::as_nanos`] — a `Duration` is 64 bits of
    /// seconds plus 32 of nanos, which is wider than this type's `u64`. That
    /// makes the narrowing an implementation detail of that one impl rather
    /// than public vocabulary, so it is not exposed.
    ///
    /// There are deliberately **no public lossy constructors**. The `i64`,
    /// `f64` and `u128` `From` impls this type used to carry were bare `as`
    /// casts the compiler could insert at any `.into()` — `NanoTime::from(-1i64)`
    /// silently became [`MAX`](Self::MAX), an instant 584 years in the *future*
    /// — and named `_lossy` replacements for them were removed unused: every
    /// one invented a policy (negatives to the epoch, `NaN` to the epoch) for a
    /// caller that did not exist. A caller holding a signed or floating
    /// nanosecond count should decide for itself what an out-of-range value
    /// means and go through [`new`](Self::new) with a checked conversion, which
    /// is total and says so.
    fn from_nanos_u128_saturating(nanos: u128) -> Self {
        Self(RawTime::try_from(nanos).unwrap_or(RawTime::MAX))
    }
}

impl From<u64> for NanoTime {
    fn from(t: u64) -> Self {
        NanoTime(t)
    }
}

/// Exact for every [`Duration`] this type can represent — a nanosecond count
/// either way, and `NanoTime`'s whole `u64` range is reachable.
///
/// It stays a `From` for that reason: unlike the `i64`/`f64`/`u128`
/// conversions this type used to carry, nothing is reinterpreted or rounded. The single edge is a `Duration` longer than
/// `u64::MAX` nanoseconds — about 584 years, which `Duration` can hold and
/// `NanoTime` cannot — and that **saturates at [`NanoTime::MAX`]**. It used to
/// wrap: the old body was `as_secs() as u64 * 1_000_000_000 + subsec`, which
/// overflowed for `as_secs() > ~1.8e10` and, in release, silently produced a
/// small instant from an enormous duration.
impl From<Duration> for NanoTime {
    fn from(dur: Duration) -> Self {
        NanoTime::from_nanos_u128_saturating(dur.as_nanos())
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

/// **Lossy above 2^53 nanoseconds** (about 104 days past the epoch, so for
/// every real wall-clock instant): `f64` has a 53-bit mantissa, and present-day
/// timestamps are ~2^60 ns, where consecutive representable values are ~256 ns
/// apart. Kept a `From` because it is an *outbound display* conversion with
/// well-defined round-to-nearest behaviour — `f64::from(t) * SECONDS_PER_NANO`
/// for a log line or a plot axis — not an `as`-cast reinterpretation. Do not
/// round-trip a timestamp through it, and do not use it to compare instants;
/// [`NanoTime`] is `Ord`.
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

// ---------------------------------------------------------------------------
// Arithmetic: plain `u64` arithmetic, deliberately unchecked.
//
// Every operator below is the raw `u64` operation on the nanosecond count, so
// it inherits Rust's integer semantics exactly: **panic on overflow/underflow
// in a debug build, wrap in a release build**. None of them saturate, and
// none of them should start to — see `Sub`.
// ---------------------------------------------------------------------------

/// Overflows past [`NanoTime::MAX`] (~year 2554). Panics in debug, wraps in
/// release, as `u64 + u64` does.
impl Add<NanoTime> for NanoTime {
    type Output = Self;
    fn add(self, other: Self) -> Self::Output {
        Self(self.0 + other.0)
    }
}

/// Adds a raw nanosecond count. Overflow behaves as [`Add<NanoTime>`].
impl Add<RawTime> for NanoTime {
    type Output = Self;
    fn add(self, other: RawTime) -> Self::Output {
        Self(self.0 + other)
    }
}

/// Advances by a [`Duration`]. Overflow behaves as [`Add<NanoTime>`]; a
/// `Duration` past `u64::MAX` nanoseconds saturates first, as
/// [`From<Duration>`](NanoTime#impl-From<Duration>-for-NanoTime) does.
impl Add<Duration> for NanoTime {
    type Output = Self;
    fn add(self, other: Duration) -> Self::Output {
        Self(self.0 + u64::from(NanoTime::from(other)))
    }
}

/// The elapsed nanoseconds from `other` to `self` — and **`self` must be the
/// later instant**.
///
/// `NanoTime` is an unsigned count, so there is no negative difference to
/// return. `a - b` where `b > a` is a `u64` underflow: it **panics in a debug
/// build and wraps to a near-[`MAX`](NanoTime::MAX) value in a release build**.
/// Order the operands, or use [`u64::abs_diff`] on the raw counts
/// (`u64::from(a).abs_diff(u64::from(b))`) when either may be the later one.
///
/// **This is deliberately not saturating.** Every subtraction the engine itself
/// performs is safe by construction — engine time advances strictly
/// monotonically, so `ctx.time() - earlier` cannot go backwards, and the sites
/// that difference *externally supplied* stamps guard the comparison before
/// subtracting. Given that, a `saturating_sub` would buy nothing at those sites
/// while turning a caller's genuine ordering bug into a silent `0` that reads
/// as "no time passed at all" — a plausible-looking latency measurement that
/// nothing downstream can tell from a real one. The loud debug panic is the
/// better failure, so the sharp edge is documented rather than filed off.
///
/// ```
/// # use wingfoil::NanoTime;
/// let (a, b) = (NanoTime::new(300), NanoTime::new(100));
/// assert_eq!(NanoTime::new(200), a - b);
/// // `b - a` would panic in debug / wrap in release; difference the raw counts:
/// assert_eq!(200, u64::from(b).abs_diff(u64::from(a)));
/// ```
impl Sub<NanoTime> for NanoTime {
    type Output = Self;
    fn sub(self, other: Self) -> Self::Output {
        Self(self.0 - other.0)
    }
}

/// Scales the count. Overflow behaves as [`Add<NanoTime>`]; a negative
/// multiplier on the signed impls reinterprets as a huge unsigned one, so do
/// not pass one.
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

    // `from_nanos_u128_saturating` is private and has exactly one caller
    // (`From<Duration>`), but its edge is the one that used to be a silent
    // ordering inversion — the old `impl From<u128>` truncated the high bits,
    // so one nanosecond past `u64::MAX` became `0`. Pinned here rather than
    // left to the `From<Duration>` test, because the saturation is the whole
    // reason the helper exists.
    //
    // There is deliberately nothing to test for `i64` or `f64`: those
    // conversions are gone entirely rather than renamed, so the only way to
    // build a `NanoTime` from a signed or floating count is a checked
    // conversion the caller writes and owns.

    #[test]
    fn from_nanos_u128_saturating_is_exact_in_range_and_saturates_above_it() {
        let raw: u128 = 1_234_567_890;
        assert_eq!(
            raw as u64,
            u64::from(NanoTime::from_nanos_u128_saturating(raw))
        );
        // Exactly representable, and one past it.
        let max = u64::MAX as u128;
        assert_eq!(
            u64::MAX,
            u64::from(NanoTime::from_nanos_u128_saturating(max))
        );
        assert_eq!(
            NanoTime::MAX,
            NanoTime::from_nanos_u128_saturating(max + 1),
            "saturates rather than wrapping to 0 (what the old `as` cast did)"
        );
        assert_eq!(
            NanoTime::MAX,
            NanoTime::from_nanos_u128_saturating(u128::MAX)
        );
    }

    #[test]
    fn from_duration_is_exact_in_range_and_saturates_above_it() {
        // Exact across the whole representable range, sub-second parts included.
        for d in [
            Duration::ZERO,
            Duration::from_nanos(1),
            Duration::new(1, 999_999_999),
            Duration::from_nanos(u64::MAX),
        ] {
            assert_eq!(d.as_nanos(), u64::from(NanoTime::from(d)) as u128);
        }
        // A duration longer than ~584 years saturates. The old body computed
        // `as_secs() as u64 * 1_000_000_000 + subsec`, which wrapped here.
        assert_eq!(NanoTime::MAX, NanoTime::from(Duration::from_secs(u64::MAX)));
    }

    #[test]
    fn sub_is_exact_for_ordered_operands() {
        // Documented contract: the left operand must be the later instant.
        // Underflow is a debug panic / release wrap, deliberately not saturating.
        assert_eq!(
            NanoTime::ZERO,
            NanoTime::new(7) - NanoTime::new(7),
            "an equal pair differences to zero, not to a wrapped value"
        );
        assert_eq!(NanoTime::new(200), NanoTime::new(300) - NanoTime::new(100));
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
