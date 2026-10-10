//! The venue's order-entry rate limits, and the token bucket that meters
//! them on both sides of the swap point.
//!
//! Venue-neutral: a venue's own limits are a constant its integration
//! names, built with the `const` [`OrderRate::new`] so an invalid one fails
//! to compile. Three places need
//! the one number: the OMS spends a budget under it, a simulated venue
//! refuses what goes past it, and a live session checks at startup that the
//! venue states exactly it. A limit only the adapter knew would leave the OMS
//! sending what the venue then refuses — and a refusal of order entry costs
//! the session.
//!
//! # What is stated, and what is spent
//!
//! [`OrderRate`] is the venue's limits **as stated** — `trading` for place,
//! amend and cancel, and `cancel_all` on its own — and a `headroom` the OMS
//! spends under them. The OMS's budget is `floor(rate × headroom)` a second
//! and `floor(burst × headroom)` at once ([`OrderRate::budget`]); the sim
//! enforces the stated terms whole. So a breach at the sim is something that
//! bypassed the OMS, never the OMS running at its budget.
//!
//! # Engine time
//!
//! [`Bucket`] refills from the instant each call is handed, never a clock,
//! like every other time in the engine (`Ctx::time`): the same
//! requests at the same instants spend the same tokens, which is what keeps a
//! replay's bursts the live run's.

use crate::NanoTime;
use std::fmt;

/// One limit: requests a second, sustained, and the most at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terms {
    /// Requests a second, sustained.
    pub rate: u32,
    /// The most requests at once.
    pub burst: u32,
}

impl Terms {
    /// A limit the venue does not state: as many a second and at once as the
    /// bucket can count. For a venue with no published order-entry throttle —
    /// a request-for-quote platform, a broker API — where any number would be
    /// one nobody stated. The bucket meters it without overflowing, so an
    /// [`OrderRate`] built on it refuses nothing.
    pub const UNMETERED: Terms = Terms {
        rate: u32::MAX,
        burst: u32::MAX,
    };
}

impl std::fmt::Display for Terms {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/s burst {}", self.rate, self.burst)
    }
}

/// An [`OrderRate`] that cannot be spent under.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Invalid {
    /// A rate or burst of zero: a bucket that never pays.
    Zero {
        /// `trading` or `cancel-all`.
        which: &'static str,
        /// What was stated.
        terms: Terms,
    },
    /// A headroom outside `(0, 1]`.
    Headroom(f64),
    /// Terms the headroom leaves nothing of: `floor(n × headroom)` is zero.
    Spent {
        /// `trading` or `cancel-all`.
        which: &'static str,
        /// What was stated.
        terms: Terms,
        /// The headroom.
        headroom: f64,
    },
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero { which, terms } => {
                write!(f, "the {which} limit {terms} has a zero rate or burst")
            }
            Self::Headroom(h) => write!(f, "headroom {h} is not in (0, 1]"),
            Self::Spent {
                which,
                terms,
                headroom,
            } => write!(
                f,
                "the {which} limit {terms} at headroom {headroom} leaves a zero budget"
            ),
        }
    }
}

impl std::error::Error for Invalid {}

/// A venue's order-entry limits as it states them, and the headroom the OMS
/// spends under them.
///
/// No [`Default`]: a rate limit is the venue's, and a made-up one is a
/// number nobody stated. Construct one with [`OrderRate::new`], which
/// validates and is `const`, so a venue's constant is checked at compile
/// time. The fields are private so an unvalidated one cannot be written.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrderRate {
    trading: Terms,
    cancel_all: Terms,
    headroom: f64,
}

impl OrderRate {
    /// A venue that states no order-entry limit: both terms
    /// [`Terms::UNMETERED`], spent whole. The OMS then never waits on a
    /// token, and the message-to-trade ratio, where the venue has one, is the
    /// only meter left.
    pub const UNMETERED: OrderRate = OrderRate {
        trading: Terms::UNMETERED,
        cancel_all: Terms::UNMETERED,
        headroom: 1.0,
    };

    /// Limits as stated, spent at `headroom`.
    ///
    /// # Errors
    ///
    /// A rate or burst of zero, a headroom outside `(0, 1]`, or one that
    /// leaves either budget at zero.
    pub const fn new(trading: Terms, cancel_all: Terms, headroom: f64) -> Result<Self, Invalid> {
        let rate = OrderRate {
            trading,
            cancel_all,
            headroom,
        };
        match rate.validate() {
            Ok(()) => Ok(rate),
            Err(invalid) => Err(invalid),
        }
    }

    /// The same limits, spent at another headroom.
    ///
    /// # Errors
    ///
    /// As [`new`](Self::new).
    pub fn with_headroom(self, headroom: f64) -> Result<Self, Invalid> {
        Self::new(self.trading, self.cancel_all, headroom)
    }

    /// Whether this can be spent under. [`new`](Self::new) asks it.
    ///
    /// # Errors
    ///
    /// As [`new`](Self::new).
    pub const fn validate(&self) -> Result<(), Invalid> {
        // `!(0 < h <= 1)` rather than the De Morgan form, so a NaN refuses.
        if !(self.headroom > 0.0 && self.headroom <= 1.0) {
            return Err(Invalid::Headroom(self.headroom));
        }
        match check("trading", self.trading, self.headroom) {
            Ok(()) => check("cancel-all", self.cancel_all, self.headroom),
            Err(invalid) => Err(invalid),
        }
    }

    /// Place, amend and cancel, as the venue states it.
    pub const fn trading(&self) -> Terms {
        self.trading
    }

    /// Cancel-all, as the venue states it.
    pub const fn cancel_all(&self) -> Terms {
        self.cancel_all
    }

    /// The fraction of each the OMS spends.
    pub const fn headroom(&self) -> f64 {
        self.headroom
    }

    /// What the OMS spends: `(trading, cancel_all)`, each
    /// `floor(n × headroom)`.
    pub fn budget(&self) -> (Terms, Terms) {
        (
            spend(self.trading, self.headroom),
            spend(self.cancel_all, self.headroom),
        )
    }
}

/// One limit, validated at a headroom already known to be in `(0, 1]`.
const fn check(which: &'static str, terms: Terms, headroom: f64) -> Result<(), Invalid> {
    if terms.rate == 0 || terms.burst == 0 {
        return Err(Invalid::Zero { which, terms });
    }
    let spent = spend(terms, headroom);
    if spent.rate == 0 || spent.burst == 0 {
        return Err(Invalid::Spent {
            which,
            terms,
            headroom,
        });
    }
    Ok(())
}

/// `floor(n × headroom)` for both terms. The nudge is so a product that is
/// a whole number in decimal (`5 × 0.8`) is not floored below it by the
/// binary representation of the factor.
const fn spend(terms: Terms, headroom: f64) -> Terms {
    Terms {
        rate: floor_at(terms.rate, headroom),
        burst: floor_at(terms.burst, headroom),
    }
}

/// `floor(n × headroom)`. The product is non-negative, so the truncating
/// cast is the floor — spelled that way because `f64::floor` is not `const`
/// at this crate's `rust-version`.
const fn floor_at(n: u32, headroom: f64) -> u32 {
    (n as f64 * headroom + 1e-9) as u32
}

/// Nanoseconds in a second, as the bucket's unit of credit per token.
const NANOS: u128 = NanoTime::NANOS_PER_SECOND as u128;

/// A token bucket over engine time: [`Terms::burst`] held, refilled at
/// [`Terms::rate`] a second, one token a request.
///
/// Credit is kept in token-nanoseconds so a refill never rounds. A bucket is
/// full the first time it is asked. An instant earlier than the last one
/// asked restores nothing and does not move the clock back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bucket {
    terms: Terms,
    held: u128,
    at: Option<NanoTime>,
}

impl Bucket {
    /// A bucket on `terms`, full.
    pub const fn new(terms: Terms) -> Self {
        Self {
            terms,
            held: 0,
            at: None,
        }
    }

    /// The terms it meters.
    pub const fn terms(&self) -> Terms {
        self.terms
    }

    fn refill(&mut self, now: NanoTime) {
        let cap = u128::from(self.terms.burst) * NANOS;
        match self.at {
            None => self.held = cap,
            Some(at) if now > at => {
                let elapsed = u128::from(u64::from(now) - u64::from(at));
                self.held = (self.held + elapsed * u128::from(self.terms.rate)).min(cap);
            }
            Some(_) => return,
        }
        self.at = Some(now);
    }

    /// Take one token at `now`, if there is one.
    pub fn try_take(&mut self, now: NanoTime) -> bool {
        self.refill(now);
        if self.held >= NANOS {
            self.held -= NANOS;
            true
        } else {
            false
        }
    }

    /// Take `n` tokens at `now`, all or none. A cost above the burst can
    /// never be held whole, so it is charged the whole burst instead — a
    /// bucket that refused it for ever would stop what it meters.
    pub fn try_take_n(&mut self, now: NanoTime, n: u32) -> bool {
        self.refill(now);
        let cost = u128::from(n.min(self.terms.burst)) * NANOS;
        if self.held >= cost {
            self.held -= cost;
            true
        } else {
            false
        }
    }

    /// Whole tokens held at `now`.
    pub fn available(&mut self, now: NanoTime) -> u32 {
        self.refill(now);
        u32::try_from(self.held / NANOS).unwrap_or(u32::MAX)
    }
}

/// A message-to-trade ratio: the second meter a venue may impose beside its
/// per-second limits.
///
/// Many exchanges bound how many order messages a session may send per
/// execution it gets — a quoter that re-prices without ever trading is load
/// the venue does not want. `free` messages are allowed before the ratio
/// applies; after that the session may have sent at most `free +
/// messages_per_fill × fills`.
///
/// **It holds back what adds risk, never what takes it off.** A place or an
/// amend waits for the ratio; a cancel and a cancel-all are counted but
/// never held — a kill switch that could not pull its quotes because the
/// book had not traded enough is the wrong way round.
///
/// Counted over the life of the [`Oms`](crate::adapters::execution::oms::Oms) that spends it —
/// a session, which is what venues measure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ratio {
    /// Messages allowed per fill.
    pub messages_per_fill: u32,
    /// Messages allowed before any fill.
    pub free: u32,
}

/// A [`Ratio`] being spent: messages sent and fills heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RatioMeter {
    ratio: Ratio,
    messages: u64,
    fills: u64,
}

impl RatioMeter {
    /// Nothing sent, nothing filled.
    pub const fn new(ratio: Ratio) -> Self {
        Self {
            ratio,
            messages: 0,
            fills: 0,
        }
    }

    /// Whether one more message that adds risk may go.
    pub const fn allows(&self) -> bool {
        self.messages < self.ratio.free as u64 + self.ratio.messages_per_fill as u64 * self.fills
    }

    /// Count one message sent, of any kind.
    pub const fn sent(&mut self) {
        self.messages += 1;
    }

    /// Count one execution.
    pub const fn filled(&mut self) {
        self.fills += 1;
    }

    /// Messages sent and fills heard.
    pub const fn counts(&self) -> (u64, u64) {
        (self.messages, self.fills)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `try_take_n` takes all or none, and a cost above the burst is
    /// charged the whole burst rather than refused for ever.
    #[test]
    fn try_take_n_is_all_or_none_and_caps_at_the_burst() {
        let mut bucket = Bucket::new(Terms { rate: 1, burst: 3 });
        assert!(bucket.try_take_n(at(1), 2));
        assert!(!bucket.try_take_n(at(1), 2), "one left: none taken");
        assert_eq!(bucket.available(at(1)), 1);
        assert!(bucket.try_take_n(at(1), 1));
        assert!(!bucket.try_take_n(at(1), 1));
        // A second later one token is back; a cost of five on a burst of
        // three waits for all three, then takes them.
        assert!(!bucket.try_take_n(at(1 + SEC), 5));
        assert!(bucket.try_take_n(at(1 + 3 * SEC), 5));
        assert_eq!(bucket.available(at(1 + 3 * SEC)), 0);
    }

    const SEC: u64 = 1_000_000_000;

    fn at(nanos: u64) -> NanoTime {
        NanoTime::new(nanos)
    }

    #[test]
    fn a_budget_is_the_floor_of_the_headroom_and_exact_where_whole() {
        const RATE: OrderRate = match OrderRate::new(
            Terms { rate: 5, burst: 20 },
            Terms { rate: 3, burst: 7 },
            0.8,
        ) {
            Ok(rate) => rate,
            Err(_) => panic!("valid"),
        };
        let (trading, cancel_all) = RATE.budget();
        assert_eq!(trading, Terms { rate: 4, burst: 16 });
        assert_eq!(cancel_all, Terms { rate: 2, burst: 5 });
        assert_eq!(
            RATE.with_headroom(1.0).unwrap().budget().0,
            Terms { rate: 5, burst: 20 }
        );
    }

    #[test]
    fn construction_refuses_what_cannot_be_spent() {
        let ok = Terms { rate: 5, burst: 20 };
        let zero = Terms { rate: 0, burst: 20 };
        assert!(matches!(
            OrderRate::new(zero, ok, 0.8),
            Err(Invalid::Zero { .. })
        ));
        assert!(matches!(
            OrderRate::new(ok, Terms { rate: 5, burst: 0 }, 0.8),
            Err(Invalid::Zero { .. })
        ));
        for headroom in [0.0, -0.1, 1.01, f64::NAN] {
            assert!(
                matches!(OrderRate::new(ok, ok, headroom), Err(Invalid::Headroom(_))),
                "{headroom}"
            );
        }
        assert!(matches!(
            OrderRate::new(Terms { rate: 1, burst: 20 }, ok, 0.8),
            Err(Invalid::Spent { .. })
        ));
        assert!(OrderRate::new(ok, ok, 1.0).is_ok());
    }

    /// The burst goes at once, the next waits for exactly the refill that
    /// pays for it, and the refill stops at the burst.
    #[test]
    fn the_bucket_refills_at_its_rate_up_to_its_burst() {
        let mut bucket = Bucket::new(Terms { rate: 4, burst: 16 });
        for _ in 0..16 {
            assert!(bucket.try_take(at(SEC)));
        }
        assert!(!bucket.try_take(at(SEC)));
        // A quarter second pays for one, and a nanosecond less does not.
        assert!(!bucket.try_take(at(SEC + SEC / 4 - 1)));
        assert!(bucket.try_take(at(SEC + SEC / 4)));
        assert_eq!(bucket.available(at(SEC + SEC / 4)), 0);
        // An hour later it is full, not more.
        assert_eq!(bucket.available(at(3_600 * SEC)), 16);
        // An earlier instant restores nothing.
        assert!(bucket.try_take(at(3_600 * SEC)));
        assert_eq!(bucket.available(at(SEC)), 15);
    }

    /// An unmetered rate never refuses: a burst of requests far past any
    /// stated venue's limit, at one instant, all pay.
    #[test]
    fn an_unmetered_rate_refuses_nothing() {
        assert!(OrderRate::UNMETERED.validate().is_ok());
        let (trading, cancel_all) = OrderRate::UNMETERED.budget();
        assert_eq!((trading, cancel_all), (Terms::UNMETERED, Terms::UNMETERED));
        let mut bucket = Bucket::new(trading);
        for _ in 0..100_000 {
            assert!(bucket.try_take(at(SEC)));
        }
        assert_eq!(bucket.available(at(2 * SEC)), u32::MAX);
    }

    /// The ratio holds after `free`, and each fill buys `messages_per_fill`
    /// more.
    #[test]
    fn a_ratio_allows_free_messages_then_so_many_per_fill() {
        let mut meter = RatioMeter::new(Ratio {
            messages_per_fill: 3,
            free: 2,
        });
        meter.sent();
        assert!(meter.allows());
        meter.sent();
        assert!(!meter.allows(), "two free, and nothing traded");
        meter.filled();
        for _ in 0..3 {
            assert!(meter.allows());
            meter.sent();
        }
        assert!(!meter.allows());
        assert_eq!(meter.counts(), (5, 1));
    }
}
