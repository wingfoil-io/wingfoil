//! Parent orders: a quantity worked into the OMS over time, as TWAP clips,
//! an iceberg or one capped sweep.
//!
//! A parent is a
//! **layer above the OMS**: it emits [`Desired`]s and reads the fills back,
//! which is all a hedger crossing past its band or a quoter refilling its
//! clip already does. So it adds no request kind, no slot state and nothing
//! for a venue to render, and the OMS does not know it exists.
//!
//! Like the OMS it is a pure fold plus a node. [`Working`] is one parent
//! being worked — [`apply`](Working::apply) the reports,
//! [`decide`](Working::decide) at an engine time — and [`Algos`] is the keyed
//! fold over them; [`AlgoOps`] wires it into a graph (see [`node`]). Nothing
//! here reads a clock but the engine time handed in.
//!
//! # The three styles
//!
//! - **[`Twap`](Style::Twap)**: `total / clips` every `(by − start) / clips`,
//!   where `start` is the engine time the parent arrived. Each clip is a
//!   [`Desired`] with [`Intent::Cross`] capped at `cap`. A clip is sized to
//!   the schedule, not to itself: clip `k` crosses `total × k / clips` less
//!   everything already filled, so a clip the cap refused — rejected, or
//!   killed unfilled — is carried into the next. What is unfilled at `by`
//!   is reported ([`Status::Lapsed`]), never chased.
//! - **[`Iceberg`](Style::Iceberg)**: shows `show` (or what is left, if
//!   less) at `cap` with [`Intent::Rest`], restated on every decide — so a
//!   fill is shown again at once, and the OMS's amend of a part-filled
//!   order back up to what is wanted *is* the refill — until `total` is
//!   filled or `by` passes.
//! - **[`Sweep`](Style::Sweep)**: `total` at once, crossing to `cap`. It is
//!   a TWAP of one clip, and is built as one.
//!
//! # The rules
//!
//! **A parent is keyed on its instrument, and a second one replaces the
//! first.** The OMS's [`Desired`] is per instrument — one decision about
//! both sides — so a parent owns its instrument's desired while it is live:
//! what it emits leaves the other side showing nothing, and a caller must
//! not route another decision onto the same instrument meanwhile. A new
//! parent on the same instrument, either side, ends the old one as
//! [`Status::Replaced`] and its first decision becomes what the OMS diffs
//! against, which amends or cancels whatever the old one left — the same
//! rule that makes a change of side a cancel-then-place in the OMS.
//!
//! **Every fill on the parent's instrument and side while it is live is the
//! parent's.** No tag on `Order` or `Fill`: the key is the attribution. A
//! fill that lands in the same instant as a replacing parent is the old
//! one's, because reports are applied before parents (the [`node`]'s
//! order).
//!
//! **A cross is restated at its own `as_of`, never a newer one.** The OMS
//! crosses a decision once: a newer `as_of` is a new decision and would be
//! crossed again. So when a fill lands while a clip is live the parent
//! re-sizes the clip — `target − filled` — under the clip's original
//! `as_of`, which the OMS takes as the same decision corrected. That is what
//! keeps a clip decided while the previous IOC was still in flight from
//! crossing what that IOC then filled: the OMS remembers the clip while the
//! slot is busy, and the fill re-sizes it before the slot frees. A new
//! clip is a new decision at the engine time it is due, and a clip that
//! falls due with nothing left to cross withdraws at that time, so nothing
//! the OMS still holds from an earlier clip — or from a parent this one
//! replaced — is crossed later at a stale size.
//!
//! That "same decision" holds only while the OMS still keeps the
//! instrument's entry. Once the IOC has ended and `retake` has passed, the
//! OMS forgets an entry that wants nothing, and with it which `as_of` was
//! crossed; a fill arriving after that — a late fill passed on after a
//! session drop reported the IOC cancelled — re-sizes the clip into an
//! entry that has spent nothing, and what the schedule still lacks is
//! crossed a second time. It is bounded by the schedule, never past it.
//!
//! **An iceberg is a decision renewed by the clock, not by every cycle.**
//! A resting desired the OMS has not heard again within its
//! `max_desired_age` is withdrawn, which is the OMS's rule for a stalled
//! strategy and is right for a stalled parent too; so the iceberg takes a
//! fresh `as_of` on each clock beat, and the clock has to beat inside that
//! age. A fill restates it — the size left to show — at the `as_of` it
//! already has: the OMS amends a part-filled order back up whatever the
//! `as_of`, and a place the venue refused, or an ack it rounded, stays
//! answered until the next beat, which is the OMS's *the strategy
//! re-decides*. Renewing on every report instead would re-send a refused
//! place once per round trip.
//!
//! **Everything is engine time.** The schedule is laid out from the instant
//! the parent arrived, every decision's `as_of` is the engine time it was
//! taken at, and a clip falls due on the first decide at or after its
//! instant: several falling due in one decide are one clip sized to the
//! latest. Same inputs at the same instants produce the same desireds.
//!
//! **A parent that cannot be worked is refused, not guessed at.** No
//! quantity, no clips, nothing to show, or a deadline already past:
//! [`Status::Refused`], and a live parent on the instrument is left alone.
//!
//! **Sizes are exact and the lot is the caller's.** A clip is `total × k /
//! clips` in the quantity's own fixed point, rounded down, and the last
//! clip is whatever is left. A venue that trades in lots rounds a clip that
//! is not one, and the OMS believes the size it acks; a caller that wants
//! round clips picks a `total` that divides.
//!
//! # What a caller spaces
//!
//! Two crosses on one instrument are the OMS's `Config::retake` apart. A
//! TWAP interval shorter than that is paced by the OMS, not the schedule,
//! and a clip due inside the retake waits for it.

use std::collections::BTreeMap;
use std::fmt;

use crate::Burst;
use crate::NanoTime;
use crate::adapters::market::{Level, Px, Qty, Side};

use crate::adapters::execution::edge::Report;
use crate::adapters::execution::oms::{Desired, Intent, Ladder};
use crate::adapters::execution::order::Instrument;
use crate::adapters::market::mul_div;

pub mod node;

pub use node::{AlgoOp, AlgoOps};

/// How a parent is worked.
///
/// Defaults to [`Sweep`](Self::Sweep) only because a `Burst` needs a
/// default; a defaulted [`Parent`] has no quantity and is refused whatever
/// its style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Style {
    /// `clips` crosses, evenly spaced up to the deadline.
    Twap {
        /// How many.
        clips: u32,
    },
    /// Rest `show` at the parent's price, shown again on every fill.
    Iceberg {
        /// The most shown at once.
        show: Qty,
    },
    /// Everything at once, crossing to the cap.
    #[default]
    Sweep,
}

/// A quantity to work on one side of one instrument by a deadline.
///
/// Plain data and `Copy`, like [`Desired`]; build one with
/// [`twap`](Self::twap), [`iceberg`](Self::iceberg) or
/// [`sweep`](Self::sweep), which refuse what cannot be worked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parent<I> {
    /// Which instrument.
    pub instrument: I,
    /// Which side: a bid buys.
    pub side: Side,
    /// How much, in total. Positive.
    pub total: Qty,
    /// The worst price a cross will take, and the price an iceberg rests
    /// at.
    pub cap: Px,
    /// The engine time by which it is done. What is unfilled then is
    /// reported, not chased.
    pub by: NanoTime,
    /// How it is worked.
    pub style: Style,
    /// Whether every order it sends may only reduce what is held — carried
    /// onto each [`Desired`]. A close sets it.
    pub reduce_only: bool,
}

/// Inert: no quantity, so [`validate`](Parent::validate) refuses it. Here
/// only because a `Burst` needs a default; the side is a bid because one has
/// to be named.
impl<I: Default> Default for Parent<I> {
    fn default() -> Self {
        Parent {
            instrument: I::default(),
            side: Side::Bid,
            total: Qty::ZERO,
            cap: Px::ZERO,
            by: NanoTime::ZERO,
            style: Style::Sweep,
            reduce_only: false,
        }
    }
}

/// Why a parent cannot be worked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentError {
    /// Nothing to work: the total is zero or negative.
    NoQuantity,
    /// A TWAP of no clips never sends anything.
    NoClips,
    /// An iceberg that shows nothing never fills.
    NoShow,
}

impl fmt::Display for ParentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoQuantity => f.write_str("a parent needs a positive total"),
            Self::NoClips => f.write_str("a TWAP needs at least one clip"),
            Self::NoShow => f.write_str("an iceberg needs a positive size to show"),
        }
    }
}

impl std::error::Error for ParentError {}

impl<I: Copy> Parent<I> {
    /// `total` in `clips` crosses capped at `cap`, spread evenly up to `by`.
    pub fn twap(
        instrument: I,
        side: Side,
        total: Qty,
        cap: Px,
        by: NanoTime,
        clips: u32,
    ) -> Result<Parent<I>, ParentError> {
        Parent::with(instrument, side, total, cap, by, Style::Twap { clips })
    }

    /// `total`, shown `show` at a time at `price`, until `by`.
    pub fn iceberg(
        instrument: I,
        side: Side,
        total: Qty,
        price: Px,
        by: NanoTime,
        show: Qty,
    ) -> Result<Parent<I>, ParentError> {
        Parent::with(instrument, side, total, price, by, Style::Iceberg { show })
    }

    /// `total` at once, crossing to `cap`; what is left at `by` is reported.
    pub fn sweep(
        instrument: I,
        side: Side,
        total: Qty,
        cap: Px,
        by: NanoTime,
    ) -> Result<Parent<I>, ParentError> {
        Parent::with(instrument, side, total, cap, by, Style::Sweep)
    }

    /// The same parent, sending only orders that reduce what is held.
    #[must_use]
    pub const fn reduce_only(self) -> Parent<I> {
        Parent {
            reduce_only: true,
            ..self
        }
    }

    fn with(
        instrument: I,
        side: Side,
        total: Qty,
        cap: Px,
        by: NanoTime,
        style: Style,
    ) -> Result<Parent<I>, ParentError> {
        let parent = Parent {
            instrument,
            side,
            total,
            cap,
            by,
            style,
            reduce_only: false,
        };
        parent.validate()?;
        Ok(parent)
    }

    /// Whether it can be worked — the constructors' check, for one
    /// assembled another way.
    pub fn validate(&self) -> Result<(), ParentError> {
        if self.total <= Qty::ZERO {
            return Err(ParentError::NoQuantity);
        }
        match self.style {
            Style::Twap { clips: 0 } => Err(ParentError::NoClips),
            Style::Iceberg { show } if show <= Qty::ZERO => Err(ParentError::NoShow),
            _ => Ok(()),
        }
    }

    /// How many crosses it is worked in: one for a sweep, none for an
    /// iceberg, which rests.
    pub const fn clips(&self) -> u32 {
        match self.style {
            Style::Twap { clips } => clips,
            Style::Sweep => 1,
            Style::Iceberg { .. } => 0,
        }
    }
}

/// Where a parent is.
///
/// Defaults to [`Working`](Self::Working) only because a `Burst` needs a
/// default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Status {
    /// Live: its fills are its own and it is still deciding.
    #[default]
    Working,
    /// Filled in full.
    Filled,
    /// Its deadline passed with [`Progress::remaining`] unfilled, which was
    /// not chased.
    Lapsed,
    /// A newer parent on its instrument took over.
    Replaced,
    /// It could not be worked, and nothing was sent for it.
    Refused,
}

impl Status {
    /// Whether it has ended.
    pub const fn done(self) -> bool {
        !matches!(self, Status::Working)
    }
}

/// What a parent has done so far: the line a caller logs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress<I> {
    /// The parent it is about.
    pub parent: Parent<I>,
    /// The engine time it started — with `parent`, which one this is.
    pub start: NanoTime,
    /// Filled so far. May exceed the total by what a clip in flight filled
    /// past it. Not final at [`Status::Lapsed`] or [`Status::Replaced`]: an
    /// IOC still in flight can fill after, and after a replacement a fill on
    /// the same side is the new parent's.
    pub filled: Qty,
    /// What is left of the total, never below zero.
    pub remaining: Qty,
    /// Crosses decided so far — a clip that fell due with nothing left to
    /// cross is not one.
    pub clips: u32,
    /// Where it is.
    pub status: Status,
    /// The engine time this was true at.
    pub at: NanoTime,
}

/// One parent being worked: a pure fold over the reports, and a decision at
/// an engine time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Working<I> {
    parent: Parent<I>,
    /// The engine time it arrived, which the schedule is laid out from.
    start: NanoTime,
    filled: Qty,
    /// Clips fallen due so far, whether or not they had anything to cross.
    due: u32,
    /// Crosses actually decided.
    clips: u32,
    /// The `as_of` of the live clip, which a re-size is stated under.
    clip: Option<NanoTime>,
    /// A fill has landed since the last decide.
    moved: bool,
    /// The `as_of` an iceberg rests under: taken on the first decide and on
    /// each clock beat, kept through a report.
    rest: Option<NanoTime>,
    status: Status,
}

impl<I: Copy + PartialEq> Working<I> {
    /// `parent`, arrived at `now`. A parent that cannot be worked, or whose
    /// deadline is not after `now`, is [`Status::Refused`] from the start.
    pub fn new(parent: Parent<I>, now: NanoTime) -> Working<I> {
        let status = if parent.validate().is_err() || parent.by <= now {
            Status::Refused
        } else {
            Status::Working
        };
        Working {
            parent,
            start: now,
            filled: Qty::ZERO,
            due: 0,
            clips: 0,
            clip: None,
            moved: false,
            rest: None,
            status,
        }
    }

    /// The parent.
    pub const fn parent(&self) -> Parent<I> {
        self.parent
    }

    /// Where it is.
    pub const fn status(&self) -> Status {
        self.status
    }

    /// What it has done, as of `at`.
    pub fn progress(&self, at: NanoTime) -> Progress<I> {
        Progress {
            parent: self.parent,
            start: self.start,
            filled: self.filled,
            remaining: self.remaining(),
            clips: self.clips,
            status: self.status,
            at,
        }
    }

    /// Fold a burst of reports, in order: every fill on the parent's
    /// instrument and side is its own while it is live. Whether anything
    /// was.
    pub fn apply(&mut self, reports: &[Report<I>]) -> bool {
        if self.status.done() {
            return false;
        }
        let mut any = false;
        for report in reports {
            if let Report::Fill(fill) = report
                && fill.instrument == self.parent.instrument
                && fill.side == self.parent.side
            {
                self.filled = self.filled + fill.qty;
                any = true;
            }
        }
        self.moved |= any;
        any
    }

    /// End it as replaced by a newer parent.
    pub fn replace(&mut self) {
        if !self.status.done() {
            self.status = Status::Replaced;
        }
    }

    /// What it wants of the OMS at `now`, if that has changed: a new clip, a
    /// clip re-sized by a fill, an iceberg restated, or — on the instant it
    /// ends, or when a clip falls due with nothing to cross — nothing
    /// showing. `beat` says the clock ticked, which is what renews an
    /// iceberg's `as_of`. `None` once it has ended, and for a parent that was
    /// refused.
    pub fn decide(&mut self, now: NanoTime, beat: bool) -> Option<Desired<I>> {
        if self.status.done() {
            return None;
        }
        let moved = std::mem::take(&mut self.moved);
        if self.remaining() == Qty::ZERO {
            self.status = Status::Filled;
            return Some(Desired::nothing(self.parent.instrument, now));
        }
        if now >= self.parent.by {
            self.status = Status::Lapsed;
            return Some(Desired::nothing(self.parent.instrument, now));
        }
        match self.parent.style {
            Style::Iceberg { show } => {
                let renew = beat || self.rest.is_none();
                if !renew && !moved {
                    return None;
                }
                let as_of = match self.rest {
                    Some(kept) if !renew => kept,
                    _ => now,
                };
                self.rest = Some(as_of);
                let qty = if self.remaining() < show {
                    self.remaining()
                } else {
                    show
                };
                Some(self.showing(qty, Intent::Rest, as_of))
            }
            Style::Twap { .. } | Style::Sweep => {
                let due = self.due_at(now);
                if due > self.due {
                    self.due = due;
                    self.clip = None;
                    let qty = self.short_of(due);
                    if qty <= Qty::ZERO {
                        return Some(Desired::nothing(self.parent.instrument, now));
                    }
                    self.clips += 1;
                    self.clip = Some(now);
                    return Some(self.showing(qty, Intent::Cross, now));
                }
                // A fill on the live clip: the same decision, re-sized.
                let as_of = self.clip.filter(|_| moved)?;
                let qty = self.short_of(self.due);
                if qty <= Qty::ZERO {
                    return Some(Desired::nothing(self.parent.instrument, as_of));
                }
                Some(self.showing(qty, Intent::Cross, as_of))
            }
        }
    }

    /// What is left of the total, never below zero.
    fn remaining(&self) -> Qty {
        let left = self.parent.total - self.filled;
        if left > Qty::ZERO { left } else { Qty::ZERO }
    }

    /// How many clips have fallen due by `now`: clip `k` (from zero) at
    /// `start + k × (by − start) / clips`.
    fn due_at(&self, now: NanoTime) -> u32 {
        let clips = self.parent.clips();
        let span = u128::from(u64::from(self.parent.by) - u64::from(self.start));
        let elapsed = u128::from(u64::from(now).saturating_sub(u64::from(self.start)));
        // `elapsed × clips / span`, which is below `clips` while `now < by`;
        // the multiply first, so an interval that does not divide is not
        // rounded away clip by clip.
        let passed = elapsed * u128::from(clips) / span;
        u32::try_from(passed + 1).map_or(clips, |due| due.min(clips))
    }

    /// The schedule's total through clip `due`, less what has filled.
    fn short_of(&self, due: u32) -> Qty {
        let clips = self.parent.clips();
        let total = self.parent.total.raw();
        let target = if due >= clips {
            total
        } else {
            mul_div(total, i128::from(due), i128::from(clips))
                .expect("invariant: total × due fits, since due < clips and total × 1 does")
        };
        Qty::from_raw(target) - self.filled
    }

    /// The parent's side at `cap`, for `qty`, as a desired.
    ///
    /// The one place a [`Desired`] is filled in, so that a change to its
    /// shape is a change here.
    fn showing(&self, qty: Qty, intent: Intent, as_of: NanoTime) -> Desired<I> {
        let mut desired = Desired::nothing(self.parent.instrument, as_of);
        let level = Ladder::one(Level::new(self.parent.cap, qty));
        match self.parent.side {
            Side::Bid => desired.bids = level,
            Side::Ask => desired.asks = level,
        }
        desired.intent = intent;
        desired.reduce_only = self.parent.reduce_only;
        desired
    }
}

/// Every live parent, one per instrument, walked in the instrument's order.
///
/// An ordered map for the OMS's reason: the burst of desireds it emits is
/// the same on a replay as on the run it replays.
#[derive(Clone, Debug)]
pub struct Algos<I> {
    live: BTreeMap<I, Working<I>>,
}

impl<I> Default for Algos<I> {
    fn default() -> Self {
        Algos {
            live: BTreeMap::new(),
        }
    }
}

impl<I: Instrument> Algos<I> {
    /// None live.
    pub fn new() -> Algos<I> {
        Algos::default()
    }

    /// The parent live on `instrument`, if one is.
    pub fn working(&self, instrument: &I) -> Option<&Working<I>> {
        self.live.get(instrument)
    }

    /// How many are live.
    pub fn len(&self) -> usize {
        self.live.len()
    }

    /// Whether none is.
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// Fold reports into every live parent, pushing the progress of each
    /// one a fill moved.
    pub fn apply(&mut self, reports: &[Report<I>], now: NanoTime, out: &mut Burst<Progress<I>>) {
        for working in self.live.values_mut() {
            if working.apply(reports) {
                out.push(working.progress(now));
            }
        }
    }

    /// Start `parent` at `now`, replacing whatever is live on its
    /// instrument; one that cannot be worked is refused and replaces
    /// nothing. Pushes the progress of each parent this ends or starts.
    pub fn start(&mut self, parent: Parent<I>, now: NanoTime, out: &mut Burst<Progress<I>>) {
        let working = Working::new(parent, now);
        if working.status() == Status::Refused {
            log::warn!("algo: refused {parent:?}");
            out.push(working.progress(now));
            return;
        }
        if let Some(mut old) = self.live.insert(parent.instrument, working) {
            old.replace();
            out.push(old.progress(now));
        }
        out.push(working.progress(now));
    }

    /// Every live parent's decision at `now`, in the instrument's order.
    /// What ends here is dropped once its progress is pushed; a parent whose
    /// clip count moved pushes its progress too.
    pub fn decide(
        &mut self,
        now: NanoTime,
        beat: bool,
        out: &mut Burst<Progress<I>>,
    ) -> Burst<Desired<I>> {
        let mut desired = Burst::new();
        for working in self.live.values_mut() {
            let clips = working.clips;
            if let Some(want) = working.decide(now, beat) {
                desired.push(want);
            }
            if working.status().done() || working.clips != clips {
                out.push(working.progress(now));
            }
        }
        self.live.retain(|_, working| !working.status().done());
        desired
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::execution::exec_id::ExecId;
    use crate::adapters::execution::order::{ClientOrderId, Fill, Liquidity};

    /// A venue listing tickers by number.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
    struct Ticker(u8);

    const A: Ticker = Ticker(1);

    fn t(nanos: u64) -> NanoTime {
        NanoTime::new(nanos)
    }

    fn q(text: &str) -> Qty {
        Qty::parse(text).unwrap()
    }

    fn px(text: &str) -> Px {
        Px::parse(text).unwrap()
    }

    fn fill(side: Side, qty: &str, at: u64) -> Report<Ticker> {
        Report::Fill(Fill {
            order: ClientOrderId::default(),
            exec_id: ExecId::new("e").unwrap(),
            instrument: A,
            side,
            qty: q(qty),
            price: px("100"),
            liquidity: Liquidity::Taker,
            recv_time: t(at),
            ..Default::default()
        })
    }

    fn cross(side: Side, qty: &str, as_of: u64) -> Desired<Ticker> {
        let mut want = Desired::nothing(A, t(as_of));
        let level = Ladder::one(Level::new(px("101"), q(qty)));
        match side {
            Side::Bid => want.bids = level,
            Side::Ask => want.asks = level,
        }
        want.intent = Intent::Cross;
        want
    }

    /// Ten in four clips over four seconds: 2.5 due at 0, 1, 2 and 3 s.
    fn twap() -> Working<Ticker> {
        let parent = Parent::twap(A, Side::Bid, q("10"), px("101"), t(4_000), 4).unwrap();
        Working::new(parent, t(0))
    }

    #[test]
    fn the_constructors_refuse_what_cannot_be_worked() {
        let by = t(1);
        assert_eq!(
            Parent::sweep(A, Side::Bid, Qty::ZERO, px("1"), by),
            Err(ParentError::NoQuantity)
        );
        assert_eq!(
            Parent::twap(A, Side::Bid, q("1"), px("1"), by, 0),
            Err(ParentError::NoClips)
        );
        assert_eq!(
            Parent::iceberg(A, Side::Bid, q("1"), px("1"), by, Qty::ZERO),
            Err(ParentError::NoShow)
        );
        assert_eq!(
            Working::new(Parent::<Ticker>::default(), t(0)).status(),
            Status::Refused
        );
        let late = Parent::sweep(A, Side::Bid, q("1"), px("1"), by).unwrap();
        assert_eq!(Working::new(late, t(1)).status(), Status::Refused);
    }

    /// A clip falls due on the first decide at or after its instant, and
    /// nothing is decided between.
    #[test]
    fn a_twap_crosses_a_clip_at_each_instant() {
        let mut w = twap();
        assert_eq!(w.decide(t(0), true), Some(cross(Side::Bid, "2.5", 0)));
        assert_eq!(w.decide(t(999), true), None);
        w.apply(&[fill(Side::Bid, "2.5", 500)]);
        // The fill re-sizes the live clip to nothing, at its own `as_of`.
        assert_eq!(w.decide(t(999), true), Some(Desired::nothing(A, t(0))));
        assert_eq!(
            w.decide(t(1_000), true),
            Some(cross(Side::Bid, "2.5", 1_000))
        );
        assert_eq!(w.progress(t(1_000)).clips, 2);
    }

    /// A clip the cap refused carries into the next; the rest at the
    /// deadline is reported, not chased.
    #[test]
    fn an_unfilled_clip_carries_and_the_deadline_lapses() {
        let mut w = twap();
        assert_eq!(w.decide(t(0), true), Some(cross(Side::Bid, "2.5", 0)));
        assert_eq!(w.decide(t(1_000), true), Some(cross(Side::Bid, "5", 1_000)));
        w.apply(&[fill(Side::Bid, "4", 1_500)]);
        assert_eq!(w.decide(t(1_500), true), Some(cross(Side::Bid, "1", 1_000)));
        // Two clips due at once are one, sized to the later.
        assert_eq!(w.decide(t(3_000), true), Some(cross(Side::Bid, "6", 3_000)));
        assert_eq!(
            w.decide(t(4_000), true),
            Some(Desired::nothing(A, t(4_000)))
        );
        let done = w.progress(t(4_000));
        assert_eq!(done.status, Status::Lapsed);
        assert_eq!(
            (done.filled, done.remaining, done.clips),
            (q("4"), q("6"), 3)
        );
        assert_eq!(w.decide(t(4_001), true), None);
    }

    /// A fill on the other side, or another instrument, is not the parent's.
    #[test]
    fn only_the_keys_fills_count() {
        let mut w = twap();
        let mut other = fill(Side::Bid, "1", 1);
        if let Report::Fill(fill) = &mut other {
            fill.instrument = Ticker(2);
        }
        assert!(!w.apply(&[fill(Side::Ask, "1", 1), other]));
        assert!(w.apply(&[fill(Side::Bid, "10", 1)]));
        assert_eq!(w.decide(t(2), true), Some(Desired::nothing(A, t(2))));
        assert_eq!(w.status(), Status::Filled);
    }

    /// A clip that falls due with nothing to cross withdraws at its own
    /// time, and is not counted as a clip.
    #[test]
    fn a_clip_with_nothing_to_cross_withdraws() {
        let mut w = twap();
        assert_eq!(w.decide(t(0), true), Some(cross(Side::Bid, "2.5", 0)));
        w.apply(&[fill(Side::Bid, "5", 10)]);
        assert_eq!(w.decide(t(10), false), Some(Desired::nothing(A, t(0))));
        assert_eq!(
            w.decide(t(1_000), true),
            Some(Desired::nothing(A, t(1_000)))
        );
        assert_eq!(w.progress(t(1_000)).clips, 1);
        assert_eq!(
            w.decide(t(2_000), true),
            Some(cross(Side::Bid, "2.5", 2_000))
        );
    }

    /// The split is exact and the last clip takes what the division left.
    #[test]
    fn clips_that_do_not_divide_are_exact() {
        let parent = Parent::twap(A, Side::Ask, q("1"), px("101"), t(3_000), 3).unwrap();
        let mut w = Working::new(parent, t(0));
        let sizes: Vec<_> = [0, 1_000, 2_000]
            .into_iter()
            .map(|at| {
                let want = w.decide(t(at), true).unwrap();
                let size = want.asks.best().unwrap().qty;
                w.apply(&[fill(Side::Ask, &size.to_string(), at)]);
                size
            })
            .collect();
        assert_eq!(
            sizes,
            [q("0.333333333"), q("0.333333333"), q("0.333333334")]
        );
    }

    /// A sweep is one cross for everything.
    #[test]
    fn a_sweep_crosses_everything_once() {
        let parent = Parent::sweep(A, Side::Bid, q("3"), px("101"), t(1_000)).unwrap();
        let mut w = Working::new(parent, t(0));
        assert_eq!(w.decide(t(0), true), Some(cross(Side::Bid, "3", 0)));
        assert_eq!(w.decide(t(500), true), None);
        assert_eq!(
            w.decide(t(1_000), true),
            Some(Desired::nothing(A, t(1_000)))
        );
        assert_eq!(w.status(), Status::Lapsed);
    }

    /// An iceberg rests `show` and restates at every decide; the last show
    /// is what is left.
    #[test]
    fn an_iceberg_shows_its_clip_until_done() {
        let parent = Parent::iceberg(A, Side::Ask, q("5"), px("101"), t(10_000), q("2"))
            .unwrap()
            .reduce_only();
        let mut w = Working::new(parent, t(0));
        let mut rest = cross(Side::Ask, "2", 0);
        rest.intent = Intent::Rest;
        rest.reduce_only = true;
        assert_eq!(w.decide(t(0), true), Some(rest));
        w.apply(&[fill(Side::Ask, "2", 10), fill(Side::Ask, "2", 10)]);
        // A fill restates the size left at the `as_of` it rests under; a
        // report that moved nothing restates nothing; a beat renews it.
        rest.asks = Ladder::one(Level::new(px("101"), q("1")));
        assert_eq!(w.decide(t(10), false), Some(rest));
        assert_eq!(w.decide(t(15), false), None);
        rest.as_of = t(16);
        assert_eq!(w.decide(t(16), true), Some(rest));
        w.apply(&[fill(Side::Ask, "1", 20)]);
        assert_eq!(w.decide(t(20), false), Some(Desired::nothing(A, t(20))));
        assert_eq!(w.status(), Status::Filled);
    }

    /// A second parent on the instrument replaces the first; a refused one
    /// replaces nothing.
    #[test]
    fn a_second_parent_replaces_the_first() {
        let mut algos = Algos::new();
        let mut out = Burst::new();
        let first = Parent::sweep(A, Side::Bid, q("3"), px("101"), t(1_000)).unwrap();
        algos.start(first, t(0), &mut out);
        let refused = Parent {
            instrument: A,
            ..Parent::default()
        };
        algos.start(refused, t(1), &mut out);
        assert_eq!(algos.working(&A).map(Working::parent), Some(first));
        let second = Parent::sweep(A, Side::Ask, q("1"), px("99"), t(1_000)).unwrap();
        algos.start(second, t(2), &mut out);
        let statuses: Vec<_> = out.iter().map(|p| (p.parent.side, p.status)).collect();
        assert_eq!(
            statuses,
            [
                (Side::Bid, Status::Working),
                (Side::Bid, Status::Refused),
                (Side::Bid, Status::Replaced),
                (Side::Ask, Status::Working),
            ]
        );
        assert_eq!(algos.len(), 1);
    }
}
