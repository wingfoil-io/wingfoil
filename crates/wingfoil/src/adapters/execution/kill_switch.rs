//! The kill switch: a latch over a set of limits, and the day a loss is
//! measured over.
//!
//! Generic over the limits. Which limits a book has and what each compares
//! is the caller's — a set of caps on whatever it is exposed to — and this
//! module holds none of them. What it holds is what every such set needs and
//! gets wrong alone: the *latch*.
//!
//! # Why a latch
//!
//! A limit that clears itself the moment the reading comes back inside is
//! not a limit, it is a lag. A book that gaps through a cap and back in the
//! same second has still been through the move the cap existed to survive,
//! and the reason it is inside again is usually that the market moved, not
//! that anybody reduced. So a breach **latches**: once [`Switch::record`]
//! has seen one, [`Stance::Halted`] stands until a human clears it with
//! [`Switch::clear`], and `clear` refuses while a breach is still standing —
//! clearing a latch on a book that is still over its limit would re-arm it
//! on the next assessment anyway, and refusing says so instead of
//! flickering.
//!
//! What to *do* while halted is not decided here; the whole output is "stop
//! opening, and here is why".
//!
//! # A refusal is a breach
//!
//! The caller's rule, stated here because every set of limits needs it: an
//! input that could not be evaluated is never a pass. A cap nobody evaluated
//! has not been respected. Read every cap through [`within`], so a value that
//! is not a number is outside it, and record an unknown input as a breach of
//! the limit that needed it.
//!
//! # A restart must not clear a latch
//!
//! A kill switch that a process restart releases is one a crash releases.
//! [`Snapshot`] is what a restart carries — the latch, what stood, and the
//! day's opening equity — and [`Switch::restore`] takes it back.
//!
//! # The day
//!
//! "Max daily loss" needs a day. [`Switch::daily_loss`] anchors one at the
//! first equity it sees after [`day_start`](Switch::new) into the UTC day —
//! a settlement print, a session open — and measures against that opening,
//! not a high-water mark, which is a different rule and would want saying
//! so.

use std::fmt;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::time::Duration;

use crate::NanoTime;
use crate::adapters::market::Qty;

/// Nanoseconds in a day, for the day the loss is measured over.
const NANOS_PER_DAY: u64 = 86_400 * 1_000_000_000;

/// A set of limits a book is held to: a fieldless enum, usually.
///
/// [`ALL`](Self::ALL) is every limit in a stated order — the order
/// [`Breaches::iter`] walks, so a log line and its replay agree — and a
/// limit's place in it is its bit. At most 64, because [`Breaches`] is a
/// `u64`: a set with more fails to build wherever a limit's bit is taken,
/// rather than shifting the 65th past the word — a panic in debug and, in
/// release, a wrap onto the first limit's bit.
pub trait Limit: Copy + Eq + Debug + 'static {
    /// Every limit, in a stated order.
    const ALL: &'static [Self];

    /// Its place in [`ALL`](Self::ALL).
    fn index(self) -> u32 {
        Self::ALL
            .iter()
            .position(|limit| *limit == self)
            .expect("every limit is in ALL") as u32
    }
}

/// The set of limits broken at one instant.
///
/// A fixed-width `Copy` set rather than a collection, because it rides an
/// edge and the execution vocabulary is `Copy` all the way down — a `Vec`
/// here would put an allocation on the one edge that fires when the book is
/// already in trouble.
pub struct Breaches<L>(u64, PhantomData<L>);

impl<L> Breaches<L> {
    /// Nothing broken.
    pub const NONE: Breaches<L> = Breaches(0, PhantomData);

    /// Whether nothing is broken. The only shape of this type that means
    /// "within limits" — a caller that checks anything else is checking a
    /// subset.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// How many limits are in the set.
    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }

    /// Everything in either set.
    pub const fn union(self, other: Breaches<L>) -> Breaches<L> {
        Breaches(self.0 | other.0, PhantomData)
    }

    /// What is in both sets.
    pub const fn intersection(self, other: Breaches<L>) -> Breaches<L> {
        Breaches(self.0 & other.0, PhantomData)
    }

    /// What is in this set and not in `other`.
    pub const fn difference(self, other: Breaches<L>) -> Breaches<L> {
        Breaches(self.0 & !other.0, PhantomData)
    }
}

impl<L: Limit> Breaches<L> {
    /// Whether `limit` is in the set.
    pub fn contains(self, limit: L) -> bool {
        self.0 & bit(limit) != 0
    }

    /// The limits in the set, in [`Limit::ALL`]'s order.
    pub fn iter(self) -> impl Iterator<Item = L> {
        L::ALL.iter().copied().filter(move |&l| self.contains(l))
    }

    /// Add `limit` to the set.
    pub fn set(&mut self, limit: L) {
        self.0 |= bit(limit);
    }
}

/// `limit`'s bit.
fn bit<L: Limit>(limit: L) -> u64 {
    let () = Fits::<L>::AT_MOST_64;
    1 << limit.index()
}

/// The bound on a [`Limit`] set, checked at compile time.
///
/// Its own type rather than an associated `const` on `Limit`, which an
/// implementor could override: this one is evaluated for every `L` that
/// [`bit`] is built for, and an `ALL` longer than 64 is a build error there.
struct Fits<L>(PhantomData<L>);

impl<L: Limit> Fits<L> {
    const AT_MOST_64: () = assert!(
        L::ALL.len() <= 64,
        "a Limit set holds at most 64 limits: Breaches is a u64"
    );
}

// Written out rather than derived: a derive would ask the same of `L`, and
// the set is a plain value whatever its limits are.
impl<L> Clone for Breaches<L> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<L> Copy for Breaches<L> {}

impl<L> Default for Breaches<L> {
    fn default() -> Self {
        Breaches::NONE
    }
}

impl<L> PartialEq for Breaches<L> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<L> Eq for Breaches<L> {}

impl<L> Hash for Breaches<L> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl<L: Limit> Debug for Breaches<L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl<L: Limit> FromIterator<L> for Breaches<L> {
    fn from_iter<T: IntoIterator<Item = L>>(iter: T) -> Breaches<L> {
        let mut breaches = Breaches::NONE;
        for limit in iter {
            breaches.set(limit);
        }
        breaches
    }
}

/// What the strategy may do.
///
/// Two states, because the latch has two.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Stance {
    /// Open new risk normally.
    Open,
    /// Latched: open nothing new. Reducing — a hedge — is not gated by this.
    ///
    /// This is the [`Default`] deliberately. A defaulted stance is a filler
    /// value some graph produced, and the inert value of a kill switch is
    /// *off*; one that defaulted to [`Open`](Self::Open) would be handing a
    /// strategy permission nobody computed.
    #[default]
    Halted,
}

/// What tripped the kill switch, and when.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Latch<L: Limit> {
    /// Engine time of the assessment that first tripped it. It does not move
    /// as further limits break — the question a latch answers is when the
    /// book stopped being inside its limits.
    pub at: NanoTime,
    /// Every limit that has broken since, unioned. Not only the first: a gap
    /// that takes several limits is one event, and whatever responds to it
    /// wants all of them.
    pub breaches: Breaches<L>,
}

impl<L: Limit> Clone for Latch<L> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<L: Limit> Copy for Latch<L> {}

/// Whether `value` is inside `cap` in absolute terms.
///
/// Read every cap through this so that a value which is not a number is
/// *outside* it. `NaN <= cap` is false, which is the answer this wants and
/// the opposite of what the obvious `abs() > cap` spelling gives — and an
/// exposure that is not a number reading as "no exposure" is exactly the
/// failure a limit exists to catch.
pub fn within(value: f64, cap: f64) -> bool {
    value.abs() <= cap
}

/// The day the loss is measured over, and what it opened at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Day {
    /// Which day, counted from the epoch at the switch's day start.
    pub window: u64,
    /// The first equity seen in it.
    pub opening: Qty,
}

/// What a [`Switch`] remembers that no reading can recompute: the latch,
/// what stood at the last assessment, and the equity the day opened at.
///
/// Plain data, so a binary can write it down and read it back; nothing here
/// does I/O. [`standing`](Self::standing) is restored with the latch so
/// [`Switch::clear`] keeps refusing until an assessment of the restarted
/// book has found it inside, and the day's opening so a new process does
/// not forgive whatever the day had already lost.
#[derive(Debug, PartialEq, Eq)]
pub struct Snapshot<L: Limit> {
    /// The day being measured, if one was anchored.
    pub day: Option<Day>,
    /// The latch, if it was set.
    pub latch: Option<Latch<L>>,
    /// What was out of limit at the last assessment.
    pub standing: Breaches<L>,
}

impl<L: Limit> Clone for Snapshot<L> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<L: Limit> Copy for Snapshot<L> {}

impl<L: Limit> Default for Snapshot<L> {
    fn default() -> Self {
        Snapshot {
            day: None,
            latch: None,
            standing: Breaches::NONE,
        }
    }
}

/// The latch over `L`, and the day.
#[derive(Clone, Debug)]
pub struct Switch<L: Limit> {
    day_start: Duration,
    day: Option<Day>,
    latch: Option<Latch<L>>,
    standing: Breaches<L>,
}

impl<L: Limit> Switch<L> {
    /// Unlatched, with no day anchored yet; days start `day_start` into the
    /// UTC day.
    pub const fn new(day_start: Duration) -> Switch<L> {
        Switch {
            day_start,
            day: None,
            latch: None,
            standing: Breaches::NONE,
        }
    }

    /// Carrying on from `snapshot` — a restart. The latch stands as it stood.
    pub const fn restore(day_start: Duration, snapshot: Snapshot<L>) -> Switch<L> {
        Switch {
            day_start,
            day: snapshot.day,
            latch: snapshot.latch,
            standing: snapshot.standing,
        }
    }

    /// What a restart needs to carry on from here.
    pub const fn snapshot(&self) -> Snapshot<L> {
        Snapshot {
            day: self.day,
            latch: self.latch,
            standing: self.standing,
        }
    }

    /// The latch, if it is set.
    pub const fn latch(&self) -> Option<Latch<L>> {
        self.latch
    }

    /// Everything the latch holds, or nothing where it is not set.
    pub const fn latched(&self) -> Breaches<L> {
        match self.latch {
            Some(latch) => latch.breaches,
            None => Breaches::NONE,
        }
    }

    /// What was out of limit at the last assessment.
    pub const fn standing(&self) -> Breaches<L> {
        self.standing
    }

    /// Halted whenever the latch is set, which outlives the breach that set
    /// it.
    pub const fn stance(&self) -> Stance {
        match self.latch {
            Some(_) => Stance::Halted,
            None => Stance::Open,
        }
    }

    /// The equity the current day opened at, if a day has been anchored.
    pub const fn opening_equity(&self) -> Option<Qty> {
        match self.day {
            Some(day) => Some(day.opening),
            None => None,
        }
    }

    /// Record one assessment at `as_of`: what is out of limit now. Latches
    /// on anything, keeping the first time and unioning every limit since.
    pub fn record(&mut self, as_of: NanoTime, standing: Breaches<L>) {
        self.standing = standing;
        if !standing.is_empty() {
            self.latch = Some(match self.latch {
                Some(latch) => Latch {
                    at: latch.at,
                    breaches: latch.breaches.union(standing),
                },
                None => Latch {
                    at: as_of,
                    breaches: standing,
                },
            });
        }
    }

    /// Clear the latch.
    ///
    /// Refuses while a breach is still standing. A latch cleared on a book
    /// that is still over its limit re-arms on the next assessment, so the
    /// refusal changes nothing about the outcome and everything about what
    /// the operator was told.
    ///
    /// Clearing a latch that is not set is not an error — it is the state
    /// being asked for.
    ///
    /// # Errors
    ///
    /// [`ClearError::StillBreached`] with what stands.
    pub fn clear(&mut self) -> Result<(), ClearError<L>> {
        if !self.standing.is_empty() {
            return Err(ClearError::StillBreached(self.standing));
        }
        self.latch = None;
        Ok(())
    }

    /// The fraction of the day's opening equity given back — positive being
    /// a loss — and the amount it is, anchoring a new day where `now` has
    /// rolled into one.
    ///
    /// `None` for no equity, which anchors nothing: a day anchored on a
    /// balance nobody measured is a reference the rest of the day cannot
    /// recompute. `None` too for a non-positive opening, against which no
    /// fraction is a number. Either is a breach for the caller.
    pub fn daily_loss(&mut self, now: NanoTime, equity: Option<Qty>) -> Option<(f64, Qty)> {
        let equity = equity?;
        let window = self.window(now);
        let day = match self.day {
            Some(day) if day.window == window => day,
            _ => {
                let day = Day {
                    window,
                    opening: equity,
                };
                self.day = Some(day);
                day
            }
        };
        let lost = day.opening - equity;
        let opening = day.opening.to_f64();
        (opening > 0.0 && opening.is_finite())
            .then(|| ((opening - equity.to_f64()) / opening, lost))
    }

    /// Which day `now` falls in, counted from the day start.
    fn window(&self, now: NanoTime) -> u64 {
        let start = u64::try_from(self.day_start.as_nanos()).unwrap_or(u64::MAX);
        u64::from(now).saturating_sub(start) / NANOS_PER_DAY
    }
}

/// Why a latch was not cleared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClearError<L: Limit> {
    /// The book is still outside these limits.
    ///
    /// Not a transient failure to retry: the assessment that produced them
    /// is the current one, and the next one will re-latch on the same
    /// numbers. What clears it is the book changing.
    StillBreached(Breaches<L>),
}

impl<L: Limit> fmt::Display for ClearError<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StillBreached(breaches) => {
                write!(f, "still outside {} limit(s)", breaches.len())
            }
        }
    }
}

impl<L: Limit> std::error::Error for ClearError<L> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Cap {
        Size,
        Loss,
        Unknown,
    }

    impl Limit for Cap {
        const ALL: &'static [Cap] = &[Cap::Size, Cap::Loss, Cap::Unknown];
    }

    const SEC: u64 = 1_000_000_000;
    const START: Duration = Duration::from_secs(8 * 3600);

    /// 08:00 UTC on some day — a day boundary at `START`.
    fn at(secs: u64) -> NanoTime {
        NanoTime::from(20_000 * 86_400 * SEC + 8 * 3600 * SEC + secs * SEC)
    }

    fn qty(s: &str) -> Qty {
        Qty::parse(s).unwrap()
    }

    fn only(limit: Cap) -> Breaches<Cap> {
        [limit].into_iter().collect()
    }

    #[test]
    fn breaches_are_a_set_walked_in_a_stated_order() {
        let set: Breaches<Cap> = [Cap::Unknown, Cap::Size].into_iter().collect();
        assert_eq!(set.iter().collect::<Vec<_>>(), [Cap::Size, Cap::Unknown]);
        assert_eq!(set.len(), 2);
        assert!(set.contains(Cap::Size) && !set.contains(Cap::Loss));
        assert_eq!(set.intersection(only(Cap::Size)), only(Cap::Size));
        assert_eq!(set.difference(only(Cap::Size)), only(Cap::Unknown));
        assert_eq!(only(Cap::Loss).union(set).len(), 3);
        assert!(Breaches::<Cap>::default().is_empty());
    }

    /// A limit set as wide as the word: the 64th limit's bit is the top one,
    /// and a full set holds every limit. One more fails to build (`Fits`).
    #[test]
    fn a_set_of_64_limits_addresses_its_last_bit() {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        struct Nth(u8);

        impl Limit for Nth {
            const ALL: &'static [Nth] = &{
                let mut all = [Nth(0); 64];
                let mut i = 0;
                while i < all.len() {
                    all[i] = Nth(i as u8);
                    i += 1;
                }
                all
            };
        }

        let last = Nth(63);
        assert_eq!(bit(last), 1 << 63);
        let mut set = Breaches::<Nth>::NONE;
        set.set(last);
        assert!(set.contains(last) && !set.contains(Nth(0)));
        assert_eq!(set.iter().collect::<Vec<_>>(), [last]);

        let full: Breaches<Nth> = Nth::ALL.iter().copied().collect();
        assert_eq!(full.len(), 64);
        assert_eq!(full.iter().last(), Some(last));
    }

    /// The latch outlives the breach, keeps its first time, and unions every
    /// limit since; `clear` refuses while one stands and releases after.
    #[test]
    fn a_breach_latches_and_only_an_inside_book_clears_it() {
        let mut switch = Switch::<Cap>::new(START);
        switch.record(at(0), Breaches::NONE);
        assert_eq!(switch.stance(), Stance::Open);

        switch.record(at(1), only(Cap::Size));
        switch.record(at(2), only(Cap::Loss));
        assert_eq!(switch.stance(), Stance::Halted);
        let latch = switch.latch().unwrap();
        assert_eq!(latch.at, at(1), "when the book first left its limits");
        assert_eq!(latch.breaches.len(), 2);
        assert_eq!(
            switch.clear(),
            Err(ClearError::StillBreached(only(Cap::Loss)))
        );

        switch.record(at(3), Breaches::NONE);
        assert_eq!(switch.stance(), Stance::Halted, "inside is not cleared");
        switch.clear().unwrap();
        assert_eq!(switch.stance(), Stance::Open);
        switch.clear().unwrap();
    }

    /// A restart carries the latch, what stood and the day's opening.
    #[test]
    fn a_restart_does_not_clear_the_latch_or_forgive_the_day() {
        let mut before = Switch::<Cap>::new(START);
        before.daily_loss(at(0), Some(qty("1")));
        before.record(at(1), only(Cap::Size));
        let after = Switch::restore(START, before.snapshot());
        assert_eq!(after.latch(), before.latch());
        assert_eq!(after.standing(), only(Cap::Size));
        assert_eq!(after.opening_equity(), Some(qty("1")));
        let mut after = after;
        assert!(after.clear().is_err());
    }

    /// The day opens at the first equity after its start and rolls at the
    /// next; no equity anchors nothing.
    #[test]
    fn the_day_is_measured_from_its_opening_and_rolls() {
        let mut switch = Switch::<Cap>::new(START);
        assert_eq!(switch.daily_loss(at(0), None), None);
        assert_eq!(switch.opening_equity(), None);

        let (fraction, lost) = switch.daily_loss(at(10), Some(qty("2"))).unwrap();
        assert_eq!((fraction, lost), (0.0, Qty::ZERO));
        let (fraction, lost) = switch.daily_loss(at(20), Some(qty("1.8"))).unwrap();
        assert!((fraction - 0.1).abs() < 1e-12);
        assert_eq!(lost, qty("0.2"));

        let (fraction, _) = switch.daily_loss(at(86_400), Some(qty("1.8"))).unwrap();
        assert_eq!(fraction, 0.0, "a new day opens at what it first sees");
        assert_eq!(switch.opening_equity(), Some(qty("1.8")));

        let mut broke = Switch::<Cap>::new(START);
        assert_eq!(broke.daily_loss(at(0), Some(Qty::ZERO)), None);
    }

    #[test]
    fn a_cap_is_not_respected_by_a_number_that_is_not_one() {
        assert!(within(-0.5, 0.5));
        assert!(!within(0.6, 0.5));
        assert!(!within(f64::NAN, 0.5));
        assert!(!within(f64::INFINITY, 0.5));
    }

    #[test]
    fn a_defaulted_stance_gives_no_permission() {
        assert_eq!(Stance::default(), Stance::Halted);
    }
}
