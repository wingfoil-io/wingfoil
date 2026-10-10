//! The OMS: the diff and the state machine, generic over the instrument.
//!
//! It takes what the strategy *wants* showing, what it knows is *working*,
//! and the reports the venue has sent back, and emits the [`Request`]s that
//! close the gap. It is pure: no venue, no I/O, and no clock
//! but the engine time that arrives as an argument.
//!
//! The instrument is an `I` the OMS keys on and walks in order
//! ([`Instrument`]); nothing here reads it.
//!
//! Order reconciliation against venue truth is not here. It belongs beside
//! the venue whose truth it is, and it reaches this fold as the reports it
//! already reads: a place lost with its session arrives as a `Cancelled`, so
//! nothing here knows reconciliation exists. A backtest cannot exercise it at
//! all — against a simulator there is no venue truth to reconcile *against*.
//!
//! # The two invariants
//!
//! **One request in flight per slot.** A slot with a request outstanding emits
//! nothing; the desired level is remembered and the diff re-runs when the
//! report lands. This is what stops the OMS spraying amends when the touch
//! moves on every tick, and it is the single most important thing in the
//! component.
//!
//! **Nothing is emitted that matches what is working.** If the working price
//! and quantity equal the desired ones, the diff is empty — so "re-price as the
//! touch moves" is a property of the strategy emitting a [`Desired`] every
//! tick, with no rule anywhere that says when to re-price.
//!
//! # What a burst of desireds means
//!
//! This has to be answered before anything can be
//! cancelled: does a key *absent* from a burst want nothing, or want what it
//! wanted last time?
//!
//! **It wants what it wanted last time, until that goes stale.** Absence
//! cannot mean withdrawal, because the desireds arrive on more than one edge —
//! a quoter's contracts and a hedger's leg are separate decisions at
//! separate instants, and a burst carrying one of them must not cancel the
//! other. So a desired is *remembered*, and [`Config::max_desired_age`] is
//! what ends it: a key whose last desired is older than that, read at the
//! reader's own engine time, wants nothing and is cancelled.
//!
//! The reason is that ops run only when something ticks, so a strategy that has stopped deciding
//! looks exactly like one that keeps deciding the same thing. It also buys the
//! property that matters more than tidiness: if the graph upstream stalls, the
//! OMS withdraws rather than leaving orders resting on a strategy that is no
//! longer thinking. Withdrawing what a contract *explicitly* wants nothing of
//! does not wait for the age — an empty [`Desired`] is a decision and is acted
//! on at once.
//!
//! # Depth
//!
//! **A side shows a [`Ladder`], and its slots are not ranked.** A
//! [`Desired`] holds up to [`MAX_DEPTH`] levels a side, best first, and an
//! instrument holds as many slots a side. The diff matches the wanted
//! ladder to the slots by price first — so a ladder shifting one tick keeps
//! the orders still at a wanted price, and their queue place — then pairs
//! what is left by rank, never amending an order past one it kept, and
//! cancels the excess and places the shortfall ([`Oms::diff`]'s passes).
//! Both invariants hold per slot, and so does the budget: its priority and
//! superseding apply across every slot as they did across every side.
//!
//! A ladder of one is one level a side, and is what everything below was
//! first written for. What a refused place spends is its **level** (the
//! decision's `as_of` and the price), not the side: a post-only reject at
//! rank 0 says the touch moved and nothing about the ranks behind it. A
//! cross takes rank 0 only.
//!
//! # Resting and crossing
//!
//! **A level is shown post-only and good-till-cancelled unless the desired
//! says [`Intent::Cross`].** A cross — a hedge past its band, a kill
//! switch flattening — arrives here as a kind on the wanted side rather than
//! as a new state, because a crossing order does not rest.
//!
//! A cross is sent as a **limit, immediate-or-cancel**, at the level's price,
//! which is the *cap*: the worst price the strategy will take, not a price to
//! show. Never a market order — a gap is exactly when a market order's worst
//! price is unbounded. It uses the same slot as anything else, because it is
//! still one request in flight on one side, and the slot goes back to idle on
//! whatever ends it: a fill to nothing, or the venue cancelling the rest.
//!
//! Two things differ from a resting level, both because an IOC is one-shot:
//!
//! - **A decision is crossed on once.** Once sent, the level — the
//!   decision's `as_of` and the cap — is spent, exactly as a refused place
//!   is, so a cross that came back part filled is not re-sent until the
//!   strategy has looked again at the delta the fill left. What is spent is
//!   that pair, not the `as_of` alone: a second desired with the *same*
//!   `as_of` and a different cap is a level not yet spent, and only
//!   [`Config::retake`] holds it back. No caller reaches it — every `as_of`
//!   is the deciding cycle's own engine time, and a cycle decides once — so
//!   it is left to the interval rather than given a rule of its own.
//! - **Two crosses on one instrument are [`Config::retake`] apart.** In a
//!   graph where a report decides at once, so an IOC the venue killed unfilled
//!   would wake a fresh decision to cross again, whose IOC would be killed in
//!   turn — an instant-by-instant loop, which is a stopped backtest and a
//!   message storm into a rate-limited venue. The interval is what breaks it.
//!
//! A side that is resting when a cross is wanted is cancelled first and
//! crossed on the next diff, since a post-only order cannot be amended into
//! a crossing one and one request in flight per slot is not relaxed for it.
//!
//! # Triggers
//!
//! **A triggered order is a level the venue holds back until a price
//! reaches it.** A desired that says [`Intent::Trigger`] places its one
//! level as a limit resting untriggered under the trigger, and once the
//! reference reaches it the venue makes it the plain limit it names — the
//! level's price a cap, as a cross's is. A stop is the case it is for: the
//! order that cuts a position, resting where the strategy is not.
//!
//! **It is a decision of its own on the instrument.** The OMS remembers two
//! desireds an instrument: the plain one, shown or crossed, and the
//! triggered one. A burst carrying either replaces only its own, so a stop
//! on a hedge leg and the hedge working that leg do not withdraw each
//! other, and each is aged by [`Config::max_desired_age`] on its own. They
//! share everything else: the id space, the budget, a cancel-all.
//!
//! **One level, on one side.** A stop is a level, not a shape, and one
//! trigger price for a buy and a sell fires whichever way the market goes;
//! both are refused ([`Oms::refused`]). Moving the trigger's price is an
//! amend, compared like the price, past [`Config::min_requote`]; a change
//! of its kind or its reference is a different order — pulled, and placed
//! once the pull is answered, as a change of side is.
//!
//! **A fired stop is answered while it is still the stop asked for.** The
//! venue says it fired ([`Report::Triggered`]), or a fill does, since only a
//! fired order trades. From then on it is a plain limit no venue will put
//! back under a trigger, so it is never amended. It is left working, and
//! nothing re-arms the stop, for as long as the strategy keeps asking for
//! that same stop — same trigger, same level — however many fresh
//! decisions restate it: a strategy decides every tick, and one that had
//! not yet seen the fill must not pull the exit it is in the middle of. A
//! decision for anything else, or nothing, or a stale one, pulls it, and a
//! different stop is placed once the pull is answered.
//!
//! **A replacement never rests beside what it replaces.** A new stop is
//! placed only when its side holds no triggered order, one being pulled
//! included, and the other side holds none either.
//!
//! # The order-rate budget
//!
//! **Every request spends a token, and what cannot be paid for waits.** The
//! venue meters order entry — place, amend and cancel from one bucket,
//! cancel-all from another — and a request past it is refused, which on
//! some venues costs the session. So the OMS holds two [`Bucket`]s over engine
//! time at [`OrderRate::budget`] — the venue's stated terms under
//! [`Config::rate`]'s headroom — and a request goes out only with a token.
//!
//! When the tokens are short, the diff is planned whole first and paid for
//! in priority order: a pending **cancel-all**, then **cancels**, then
//! **amends**, then **places** — what takes risk off before what puts it on,
//! and within a class the walk's order, which is the instrument's. The burst
//! still comes out in walk order; only which of it goes is chosen by priority.
//!
//! **What waits is not a queue.** A request that was not paid for changes no
//! slot and mints no id; the slot stays as it was and the desired stays
//! remembered, so the next diff plans it again from what is wanted *then*.
//! A request whose plan has changed by the time it could be paid for — a
//! newer decision, a fill, a staleness — was **superseded**: it is dropped
//! and counted, and whatever the diff now plans takes its place. Nothing
//! stale is ever sent, because nothing is kept to send.
//!
//! **Re-driven by the same clock as staleness.** The diff runs on every
//! desired, every report and every sweep of the strategy graph's clock, so a
//! request left waiting goes out on the first of them after its token has
//! refilled — no new input needed, and no second clock. Same inputs at the
//! same instants spend the same tokens, so a replay's bursts are the live
//! run's. [`Oms::pacing`] says how many requests waited and how many of those
//! were superseded.
//!
//! **Every diff walks every instrument.** Whatever ticked, [`Oms::diff`]
//! plans each instrument it holds, so a report costs O(N) in the instruments
//! held — the right trade for a book of tens of instruments, not thousands,
//! because the walk is what lets staleness and a waiting request need no
//! bookkeeping of their own. If the book ever is thousands, the fix is a
//! dirty set of the keys touched this tick plus the sweep that finds
//! staleness, not a rewrite of the OMS.
//!
//! **Client order ids are a counter under an epoch.** The counter is
//! replay-deterministic, which a random or wall-clock-derived id is not, and
//! that is what an id the venue echoes back has to be. What a bare counter does not survive
//! is a restart: it begins again at numbers the previous process's orders
//! were labelled with. So the high bits are the process's
//! [`Epoch`], handed to
//! [`Oms::resumed`] by whoever persisted the last one — see
//! [`ClientOrderId`]'s layout. [`Oms::new`] is epoch zero, the bare counter,
//! which is what a backtest and every test here run on.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

pub mod node;

pub use node::{OmsOp, OmsOps};

use crate::Burst;
use crate::NanoTime;
use crate::adapters::execution::edge::TradingState;
use crate::adapters::execution::edge::{Amend, RejectReason, Report, Request};
use crate::adapters::execution::order::{
    ClientOrderId, Epoch, Instrument, Order, TimeInForce, Trigger,
};
use crate::adapters::execution::rate_limit::{Bucket, OrderRate, Ratio, RatioMeter};
use crate::adapters::market::{Level, Px, Qty, Side};

/// How a wanted level is to be worked: rested or crossed.
///
/// Named for what the strategy intends of the level, not "execution": the
/// module is `execution`, and an execution is already a fill
/// ([`ExecId`](crate::adapters::execution::exec_id::ExecId), a FIX `ExecReport` and its
/// `ExecType`), so a field called `execution` on a [`Desired`] would read as
/// the fill.
///
/// Defaults to [`Rest`](Self::Rest), the inert choice for the same
/// reason [`OrderKind`](crate::adapters::execution::order::OrderKind) defaults to post-only: it is
/// the one that cannot cross.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Intent {
    /// Show it: post-only, good till cancelled.
    #[default]
    Rest,
    /// Take it: a limit order, immediate-or-cancel, whose price is the worst
    /// the strategy will pay.
    Cross,
    /// Rest it untriggered at the venue until the trigger fires, then take
    /// it: a limit order whose price is the worst the strategy will pay once
    /// it has fired, like a cross's cap.
    ///
    /// **A triggered desired is a decision of its own on the instrument**
    /// (module docs, Triggers): it is remembered beside the instrument's
    /// other desired rather than over it, so a stop on a leg and the quotes
    /// or hedge working that leg do not replace each other. Withdrawing a
    /// stop is a triggered desired with nothing on either side
    /// ([`Desired::no_trigger`]). One level, on one side: a stop is a
    /// level, not a shape, and a stop both ways at one price would fire
    /// whichever way the market went.
    Trigger(Trigger),
}

impl Intent {
    /// The trigger, for [`Trigger`](Self::Trigger).
    pub const fn trigger(self) -> Option<Trigger> {
        match self {
            Intent::Trigger(trigger) => Some(trigger),
            Intent::Rest | Intent::Cross => None,
        }
    }
}

/// What kind of order a passive level is sent as.
///
/// A venue setting, not a decision: where the venue has a post-only order
/// the OMS uses it, and where it has none a plain
/// limit is the only way to rest. The difference is who guards against
/// crossing. A post-only order is refused by the venue if the touch has
/// moved through it; a limit is filled, as a taker, at whatever it crosses —
/// so under [`Limit`](Self::Limit) the price guard is the **strategy's**,
/// which prices every level off a touch the OMS never sees.
///
/// Defaults to [`PostOnly`](Self::PostOnly), the one that cannot cross.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Passive {
    /// Post-only, good till cancelled: the venue refuses what would cross.
    #[default]
    PostOnly,
    /// A plain limit, good till cancelled, for a venue with no post-only. A
    /// `PostOnlyWouldCross` reject is never expected.
    Limit,
}

/// How long a passive level rests at the venue.
///
/// Defaults to [`GoodTillCancel`](Self::GoodTillCancel), which a continuous
/// venue needs. A session venue retires [`Day`](Self::Day) orders at its
/// close, as expired, and the OMS re-places what is still wanted after the
/// next open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Lifetime {
    /// Until cancelled.
    #[default]
    GoodTillCancel,
    /// Until the venue's close.
    Day,
}

impl Lifetime {
    /// The order's time in force.
    pub const fn tif(self) -> TimeInForce {
        match self {
            Lifetime::GoodTillCancel => TimeInForce::GoodTillCancel,
            Lifetime::Day => TimeInForce::Day,
        }
    }
}

/// The most levels a side can show: the length of a [`Ladder`].
///
/// A crate constant rather than a const generic, which would land on
/// [`Oms`], [`OmsOp`] and every alias of them for the sake of a bound the
/// venue's order-rate budget sets anyway — depth multiplies every re-price
/// by itself.
pub const MAX_DEPTH: usize = 8;

/// A ladder the diff cannot work: refused at construction, the way an
/// order a venue would refuse is refused by
/// [`Order::validate`](crate::adapters::execution::order::Order::validate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LadderError {
    /// More levels than [`MAX_DEPTH`].
    ///
    /// Refused rather than trimmed: which levels to drop is a decision, and
    /// it is the strategy's.
    TooDeep(usize),
    /// A level no further from the touch than the one before it.
    ///
    /// Two levels at one price are refused with the rest: a venue would
    /// accept two orders there, and the diff, which matches working orders
    /// to wanted levels by price, could not tell them apart.
    NotMonotone {
        /// The rank of the level out of order.
        rank: usize,
    },
}

impl fmt::Display for LadderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooDeep(n) => write!(f, "a ladder holds at most {MAX_DEPTH} levels, not {n}"),
            Self::NotMonotone { rank } => {
                write!(f, "level {rank} is not strictly behind the one before it")
            }
        }
    }
}

impl std::error::Error for LadderError {}

/// The levels one side shows, best first: rank 0 is the touch's.
///
/// Fixed-size so that a [`Desired`] stays `Copy`. Its prices are strictly
/// monotone away from the touch, which only the constructors can build, so
/// no two levels share a price. A ladder with only rank 0 filled is one
/// level a side, and [`Ladder::one`] is the whole of what a one-level
/// strategy needs.
///
/// **It does not know its side.** [`from_levels`](Self::from_levels) checks
/// the order against the side it is given and keeps nothing of it, so a
/// ladder built as asks can be put on [`Desired::bids`]. The diff is not
/// fooled — it re-sorts every side's levels nearest the touch first — but
/// [`best`](Self::best) and [`get`](Self::get) read the ranks as built, and
/// on the wrong side `best` is the level furthest from the touch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ladder {
    /// Filled from rank 0, with no gap.
    levels: [Option<Level>; MAX_DEPTH],
}

impl Ladder {
    /// No level: the side shows nothing.
    pub const fn none() -> Ladder {
        Ladder {
            levels: [None; MAX_DEPTH],
        }
    }

    /// One level, at rank 0.
    pub const fn one(level: Level) -> Ladder {
        let mut levels = [None; MAX_DEPTH];
        levels[0] = Some(level);
        Ladder { levels }
    }

    /// One level or none.
    pub const fn maybe(level: Option<Level>) -> Ladder {
        match level {
            Some(level) => Ladder::one(level),
            None => Ladder::none(),
        }
    }

    /// `levels` on `side`, best first: each strictly further from the touch
    /// than the one before — lower for a bid, higher for an ask.
    pub fn from_levels(side: Side, levels: &[Level]) -> Result<Ladder, LadderError> {
        if levels.len() > MAX_DEPTH {
            return Err(LadderError::TooDeep(levels.len()));
        }
        let mut ladder = Ladder::none();
        for (rank, level) in levels.iter().enumerate() {
            if rank > 0 && !ahead(side, levels[rank - 1].price, level.price) {
                return Err(LadderError::NotMonotone { rank });
            }
            ladder.levels[rank] = Some(*level);
        }
        Ok(ladder)
    }

    /// The level at `rank`, if the ladder is that deep.
    pub fn get(&self, rank: usize) -> Option<Level> {
        self.levels.get(rank).copied().flatten()
    }

    /// Rank 0: the level nearest the touch.
    pub const fn best(&self) -> Option<Level> {
        self.levels[0]
    }

    /// How many levels it shows.
    pub fn depth(&self) -> usize {
        self.levels
            .iter()
            .take_while(|level| level.is_some())
            .count()
    }

    /// Whether it shows nothing.
    pub const fn is_empty(&self) -> bool {
        self.levels[0].is_none()
    }

    /// Its levels, best first.
    pub fn iter(&self) -> impl Iterator<Item = Level> + '_ {
        self.levels.iter().map_while(|level| *level)
    }
}

impl From<Level> for Ladder {
    fn from(level: Level) -> Ladder {
        Ladder::one(level)
    }
}

impl From<Option<Level>> for Ladder {
    fn from(level: Option<Level>) -> Ladder {
        Ladder::maybe(level)
    }
}

/// Whether `a` is nearer the touch than `b` on `side`: higher for a bid,
/// lower for an ask.
fn ahead(side: Side, a: Px, b: Px) -> bool {
    match side {
        Side::Bid => a > b,
        Side::Ask => a < b,
    }
}

/// The order of two prices on `side`, nearest the touch first.
fn touch_order(side: Side, a: Px, b: Px) -> Ordering {
    match side {
        Side::Bid => b.cmp(&a),
        Side::Ask => a.cmp(&b),
    }
}

/// What the strategy wants showing on one instrument, as of one instant.
///
/// The venue-neutral shape a strategy's own decisions convert into: the OMS
/// has no business knowing what a smile or a delta band is. A two-way on a
/// contract and a hedger's one side on a leg are the same thing, the second
/// its degenerate case. Each side is a [`Ladder`]; one level a side is a
/// ladder of one.
///
/// Build one with the constructor for its shape — [`quote`](Self::quote),
/// [`two_way`](Self::two_way), [`rest`](Self::rest),
/// [`cross`](Self::cross), [`stop`](Self::stop), [`nothing`](Self::nothing),
/// [`no_trigger`](Self::no_trigger), then [`reduce_only`](Self::reduce_only)
/// if it is — each of which takes the `as_of`, because it is what the OMS
/// keys answers and staleness on and has no default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Desired<I> {
    /// Which instrument.
    pub instrument: I,
    /// The bids to show, best first; empty to show none.
    pub bids: Ladder,
    /// The asks to show, best first; empty to show none.
    pub asks: Ladder,
    /// The engine time the decision was made at — never the wall clock, so a
    /// replay decides at the instant the live run did. It is also what
    /// [`Config::max_desired_age`] is measured from.
    pub as_of: NanoTime,
    /// Whether the levels are shown or taken. Taken, a level's price is a
    /// cap rather than a place to rest, and a side takes rank 0 only: a
    /// cross is a cap, not a shape, and a crossing ladder deeper than one is
    /// refused ([`Oms::refused`]).
    pub intent: Intent,
    /// Whether the levels may only reduce what is held: the venue refuses
    /// an order that would open or flip. Closes, and a hedge that crosses to
    /// cut its leg's own position — the one venue-side backstop against a
    /// hedge opening exposure on a fold the venue disagrees with. Carried
    /// onto the
    /// [`Order`] and said again on every amend.
    pub reduce_only: bool,
}

impl<I: Copy> Desired<I> {
    /// Nothing showing on either side.
    pub const fn nothing(instrument: I, as_of: NanoTime) -> Desired<I> {
        Desired {
            instrument,
            bids: Ladder::none(),
            asks: Ladder::none(),
            as_of,
            intent: Intent::Rest,
            reduce_only: false,
        }
    }

    /// No triggered order on either side: what withdraws a stop, since a
    /// triggered desired is remembered apart from the rest
    /// ([`Intent::Trigger`]). The trigger it names is never sent.
    pub const fn no_trigger(instrument: I, as_of: NanoTime) -> Desired<I> {
        Desired {
            intent: Intent::Trigger(Trigger::stop(
                crate::adapters::execution::order::Reference::Mark,
                Px::ZERO,
            )),
            ..Desired::nothing(instrument, as_of)
        }
    }

    /// `bids` and `asks` shown, both rested: post-only (or as
    /// [`Config::passive`] says) and good till cancelled, not reduce-only.
    /// A quoter's decision, a ladder a side. Either side may be
    /// [`Ladder::none`].
    pub const fn quote(instrument: I, as_of: NanoTime, bids: Ladder, asks: Ladder) -> Desired<I> {
        Desired {
            bids,
            asks,
            ..Desired::nothing(instrument, as_of)
        }
    }

    /// One level rested on each side: [`quote`](Self::quote) with a ladder
    /// of one a side.
    pub const fn two_way(instrument: I, as_of: NanoTime, bid: Level, ask: Level) -> Desired<I> {
        Desired::quote(instrument, as_of, Ladder::one(bid), Ladder::one(ask))
    }

    /// `ladder` rested on `side` and nothing on the other: a hedger's one
    /// side on a leg, the degenerate case of a [`quote`](Self::quote). A
    /// single level is `Ladder::one(level)`.
    pub const fn rest(instrument: I, as_of: NanoTime, side: Side, ladder: Ladder) -> Desired<I> {
        match side {
            Side::Bid => Desired::quote(instrument, as_of, ladder, Ladder::none()),
            Side::Ask => Desired::quote(instrument, as_of, Ladder::none(), ladder),
        }
    }

    /// `level` taken on `side` ([`Intent::Cross`]): a limit,
    /// immediate-or-cancel, whose price is the worst the strategy will pay,
    /// and nothing on the other side. One level, because a cross is a cap,
    /// not a shape — a crossing ladder deeper than one is refused, so this
    /// cannot build one. Spent once sent (module docs, *Resting and
    /// crossing*): a fresh decision is a fresh `as_of`.
    pub const fn cross(instrument: I, as_of: NanoTime, side: Side, level: Level) -> Desired<I> {
        Desired {
            intent: Intent::Cross,
            ..Desired::rest(instrument, as_of, side, Ladder::one(level))
        }
    }

    /// `level` resting untriggered on `side` under `trigger`
    /// ([`Intent::Trigger`]), and nothing on the other side: a decision
    /// remembered beside the instrument's plain one, not over it. One
    /// level, one side — a stop is a level, not a shape, and this cannot
    /// build the shapes that are refused. A stop that cuts a position is
    /// usually also [`reduce_only`](Self::reduce_only); withdraw it with
    /// [`no_trigger`](Self::no_trigger).
    pub const fn stop(
        instrument: I,
        as_of: NanoTime,
        side: Side,
        trigger: Trigger,
        level: Level,
    ) -> Desired<I> {
        Desired {
            intent: Intent::Trigger(trigger),
            ..Desired::rest(instrument, as_of, side, Ladder::one(level))
        }
    }

    /// The same decision, reduce-only: the venue refuses any of its levels
    /// that would open or flip a position ([`reduce_only`](Self#structfield.reduce_only)).
    #[must_use]
    pub const fn reduce_only(mut self) -> Desired<I> {
        self.reduce_only = true;
        self
    }

    /// One side of it.
    pub const fn side(&self, side: Side) -> Ladder {
        match side {
            Side::Bid => self.bids,
            Side::Ask => self.asks,
        }
    }

    /// Whether this instrument wants nothing showing at all.
    pub const fn is_empty(&self) -> bool {
        self.bids.is_empty() && self.asks.is_empty()
    }
}

/// The OMS's own thresholds.
///
/// Start from [`Config::unmetered`] and state what differs; a live venue
/// states its limits with [`with_rate`](Self::with_rate):
///
/// ```
/// use std::time::Duration;
/// use wingfoil::adapters::execution::oms::Config;
/// use wingfoil::adapters::execution::rate_limit::{OrderRate, Terms};
///
/// const VENUE_RATE: OrderRate =
///     OrderRate::stated(Terms::per_second(50, 100), Terms::per_second(5, 20), 0.8);
///
/// let backtest = Config::unmetered();
/// let live = Config::unmetered()
///     .with_rate(VENUE_RATE)
///     .with_max_desired_age(Duration::from_secs(2));
/// assert_eq!(live.rate, VENUE_RATE);
/// assert_eq!(backtest.retake, live.retake);
/// ```
///
/// The fields stay public, so a struct literal states all seven.
///
/// **No `Default`**, like [`OrderRate`]'s and the risk limits'. The OMS
/// spends a headroom, never the terms, so [`OrderRate::UNMETERED`] would be
/// a harmless default *in the OMS* — but `Config::default()` and
/// `..Default::default()` read as "nothing was chosen", and a live config
/// written that way would leave the venue's limits unstated without a word
/// in the source saying so. `unmetered()` puts the one venue term it assumes
/// in its name: a reader of a live config sees either a
/// [`with_rate`](Self::with_rate) or a deliberate statement that the venue
/// has no limit. A config that leaves a venue's limits out sends past
/// them, and a refusal of order entry costs the session.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    /// How long a [`Desired`] is believed without being refreshed, measured
    /// at the reader's own engine time. Past it the key wants nothing and
    /// whatever is working on it is cancelled.
    ///
    /// It has to be comfortably longer than the strategy's own decision
    /// interval and comfortably shorter than how long we are willing to rest
    /// on a stalled graph — a number to measure, not to guess.
    ///
    /// Off is `Duration::MAX`: a desired is then believed until replaced.
    pub max_desired_age: Duration,
    /// The smallest price move worth an amend.
    ///
    /// An exact diff means a one-tick move causes an amend, and into a venue
    /// that rate-limits that may be more messages than the improvement is
    /// worth. Defaults to zero — amend on any move — deliberately: the amend
    /// rate is a thing to *measure* on recorded data and then set from, not to
    /// guess at now. A quantity change amends whatever this says, because a
    /// size that is wrong is wrong by the whole difference.
    pub min_requote: Px,
    /// The shortest interval between two crossing orders on one instrument,
    /// measured from the engine time the first was sent.
    ///
    /// It is what stops an IOC the venue killed from waking a decision that
    /// crosses again in the next instant — see the module docs. Long enough
    /// that the market has had a chance to show something new, short enough
    /// that a book still outside its outer band is not left there: the
    /// strategy's own sweep is the natural scale.
    ///
    /// Off is `Duration::ZERO`: crosses are not spaced. `Duration::MAX` is
    /// the other end — one cross per instrument for the life of the OMS.
    pub retake: Duration,
    /// The venue's order-entry limits and the headroom spent under them —
    /// see the module docs' budget. The OMS spends
    /// [`OrderRate::budget`], never the stated terms.
    pub rate: OrderRate,
    /// What kind of order a passive level is sent as — [`Passive`].
    pub passive: Passive,
    /// How long a passive level rests — [`Lifetime`].
    pub lifetime: Lifetime,
    /// A message-to-trade ratio the venue imposes beside [`rate`](Self::rate),
    /// or `None` for a venue with none. Places and amends wait for it;
    /// cancels are counted and never held ([`Ratio`]).
    pub ratio: Option<Ratio>,
}

impl Config {
    /// A backtest's, or a venue's that states no order-entry limit: rate
    /// [`OrderRate::UNMETERED`] and no message-to-trade [`Ratio`], passive
    /// levels post-only and good till cancelled, and these thresholds:
    ///
    /// - **[`max_desired_age`](Self::max_desired_age): five seconds.** The
    ///   field exists for a stalled graph, so the default must withdraw —
    ///   `Duration::MAX` would be the one failure it is there to prevent. A
    ///   strategy that decides on market data restates many times a second,
    ///   so five seconds is comfortably longer than its interval, and bounds
    ///   how long a live book rests on a strategy that has stopped. One that
    ///   decides less often (a hedger restating once a minute) states its
    ///   own: under this default its orders are withdrawn between decisions,
    ///   which is visible and safe, never left resting.
    /// - **[`min_requote`](Self::min_requote): zero** — amend on any move,
    ///   as the field's own docs argue: the amend rate is to be measured on
    ///   recorded data and set from, and an unmetered venue charges nothing
    ///   for the amends meanwhile.
    /// - **[`retake`](Self::retake): one second.** Zero is the one value
    ///   that is not safe: a killed IOC's report wakes a decision that
    ///   crosses again the next instant, every instant. A second is long
    ///   enough for any venue's market to have shown something new, and short
    ///   enough that a book outside its band is not left there for long.
    /// - **[`passive`](Self::passive): [`Passive::PostOnly`]**, the one kind
    ///   that cannot cross, and **[`lifetime`](Self::lifetime):
    ///   [`Lifetime::GoodTillCancel`]**, which a continuous venue needs —
    ///   both their types' own defaults.
    ///
    /// Each is changed with its `with_` setter. A venue that states limits
    /// is `Config::unmetered().with_rate(VENUE_RATE)` — the name says what
    /// was assumed until then, which is why there is no `Default`
    /// ([`Config`]).
    pub const fn unmetered() -> Self {
        Self {
            max_desired_age: Duration::from_secs(5),
            min_requote: Px::ZERO,
            retake: Duration::from_secs(1),
            rate: OrderRate::UNMETERED,
            passive: Passive::PostOnly,
            lifetime: Lifetime::GoodTillCancel,
            ratio: None,
        }
    }

    /// The venue's order-entry limits, as stated — [`rate`](Self::rate).
    pub const fn with_rate(self, rate: OrderRate) -> Self {
        Self { rate, ..self }
    }

    /// The venue's message-to-trade ratio — [`ratio`](Self::ratio).
    pub const fn with_ratio(self, ratio: Ratio) -> Self {
        Self {
            ratio: Some(ratio),
            ..self
        }
    }

    /// What a passive level is sent as — [`passive`](Self::passive).
    pub const fn with_passive(self, passive: Passive) -> Self {
        Self { passive, ..self }
    }

    /// How long a passive level rests — [`lifetime`](Self::lifetime).
    pub const fn with_lifetime(self, lifetime: Lifetime) -> Self {
        Self { lifetime, ..self }
    }

    /// How long a desired is believed —
    /// [`max_desired_age`](Self::max_desired_age).
    pub const fn with_max_desired_age(self, max_desired_age: Duration) -> Self {
        Self {
            max_desired_age,
            ..self
        }
    }

    /// The interval between two crosses on one instrument —
    /// [`retake`](Self::retake).
    pub const fn with_retake(self, retake: Duration) -> Self {
        Self { retake, ..self }
    }

    /// The smallest price move worth an amend —
    /// [`min_requote`](Self::min_requote).
    pub const fn with_min_requote(self, min_requote: Px) -> Self {
        Self {
            min_requote,
            ..self
        }
    }
}

/// What the budget has held back, for a health line and a metric.
///
/// Totals since the OMS was made, and what is waiting now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pacing {
    /// Requests the budget could not pay for when first planned — each
    /// counted once however many diffs it then waited through. A pending
    /// cancel-all counts too.
    pub deferred: u64,
    /// Of those, the ones dropped because what was planned changed before a
    /// token did: never sent stale.
    pub superseded: u64,
    /// Requests waiting now, a pending cancel-all included.
    pub waiting: u32,
}

/// One side's request, planned but not yet paid for: what [`Oms::diff`]
/// would send, before an id is minted or a slot moved.
///
/// Compared by value between diffs, which is what says whether a request
/// that waited is still the one wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Plan<I> {
    /// Pull what is resting.
    Cancel(Placed),
    /// Move what is resting.
    Amend(Placed, Amend<I>),
    /// Place a level: shown, or taken when `cross`, or resting untriggered
    /// under `trigger`.
    Place {
        level: Level,
        cross: bool,
        reduce_only: bool,
        trigger: Option<Trigger>,
    },
}

impl<I> Plan<I> {
    /// Who is paid first when the tokens are short: what takes risk off
    /// before what puts it on.
    const fn priority(&self) -> u8 {
        match self {
            Plan::Cancel(_) => 0,
            Plan::Amend(..) => 1,
            Plan::Place { .. } => 2,
        }
    }
}

/// An order the venue has, or is about to have.
///
/// Placed rather than resting: it is what [`Slot::Pending`] holds too,
/// where the venue has not accepted it yet, and what a cross holds, which
/// never rests at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Placed {
    /// Its client id.
    pub id: ClientOrderId,
    /// The price it rests at.
    pub price: Px,
    /// The quantity it was sent for.
    pub qty: Qty,
    /// What is still showing — `qty` less everything filled against it. This
    /// is what the diff compares a desired size against, so a partial fill
    /// re-shows rather than leaving a quote short.
    pub remaining: Qty,
    /// The `as_of` of the [`Desired`] it was last asked from: the one it
    /// was placed from, until an amend is sent from a newer one. It is what
    /// a refused place spends, and what a refused amend marks answered: a
    /// decision that arrived while the request was in flight was never
    /// asked of the venue, and must not be held by an answer to an older
    /// one.
    pub as_of: NanoTime,
    /// The trigger it rests under, until the venue fires it: `None` for an
    /// order that has none, and for a triggered one that has fired — a plain
    /// order from then on.
    pub trigger: Option<Trigger>,
}

/// One side of one instrument: the state machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Slot<I> {
    /// Nothing working and nothing asked for.
    #[default]
    Idle,
    /// A place is in flight.
    Pending(Placed),
    /// The venue acknowledged it: it is resting.
    Working(Placed),
    /// An amend is in flight. `working` is what is still resting until the
    /// venue says otherwise — which is what a rejected amend returns to.
    PendingAmend {
        /// What is resting now.
        working: Placed,
        /// What the amend asks for.
        to: Amend<I>,
    },
    /// A cancel is in flight.
    PendingCancel(Placed),
}

impl<I: Copy> Slot<I> {
    /// The order this slot is about, if there is one.
    pub const fn placed(&self) -> Option<Placed> {
        match self {
            Slot::Idle => None,
            Slot::Pending(resting)
            | Slot::Working(resting)
            | Slot::PendingCancel(resting)
            | Slot::PendingAmend {
                working: resting, ..
            } => Some(*resting),
        }
    }

    /// Whether a request is outstanding on it — the first invariant, as a
    /// predicate. A slot that answers `true` emits nothing.
    pub const fn in_flight(&self) -> bool {
        matches!(
            self,
            Slot::Pending(_) | Slot::PendingAmend { .. } | Slot::PendingCancel(_)
        )
    }

    /// The order id this slot would route a report by.
    const fn id(&self) -> Option<ClientOrderId> {
        match self.placed() {
            Some(resting) => Some(resting.id),
            None => None,
        }
    }
}

/// One side of one instrument: its slots, and what the fold remembers
/// about them.
///
/// **Slots are not ranked.** A slot holds whichever order the diff put
/// there, and the diff matches slots to the wanted ladder by price every
/// time it runs ([`Oms::diff`]), so a slot's index says nothing about where
/// its order sits in the ladder — only which slot a report routes to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Rungs<I> {
    slots: [Slot<I>; MAX_DEPTH],
    /// Per slot: the `as_of` of the decision the venue accepted at something
    /// other than what was asked — a price rounded to its tick, a size
    /// trimmed to its lot — and the price that was asked, which is the
    /// wanted level it answered.
    ///
    /// What it buys is in [`Oms::plan`]: the working order is left as the
    /// venue rests it, neither amended back to the ask (which the venue
    /// would round again, every instant) nor pulled, until the strategy has
    /// decided afresh. Cleared when the slot is placed again.
    answered: [Option<(NanoTime, Px)>; MAX_DEPTH],
    /// The levels already answered — a place the venue refused, or a cross
    /// that was sent — as the `as_of` of the decision and the level's price.
    ///
    /// What it buys is in [`Oms::wanted`]: a place the venue rejected is not
    /// sent again until the strategy has decided afresh, and nor is a cross,
    /// which is one-shot. Per level, because a post-only reject at rank 0
    /// says the touch moved and nothing about rank 3; and per side, because
    /// a post-only bid can cross while the ask rests perfectly well. Kept by
    /// price rather than by slot, because the slot a level is placed in is
    /// whichever is idle: a decision holds at most [`MAX_DEPTH`] levels, so
    /// the array holds every level of one decision, and a new entry evicts
    /// the oldest.
    spent: [Option<(NanoTime, Px)>; MAX_DEPTH],
    /// The requests the budget could not pay for at the last diff — what the
    /// next diff's plans are compared against. A set: a side plans at most
    /// one request a slot, and what waited and is no longer planned is
    /// dropped before anything new is held.
    held: [Option<Plan<I>>; MAX_DEPTH],
    /// The triggered half only: the stop the venue fired on this side, as
    /// the strategy asked for it — its trigger and its level's price. While
    /// the strategy keeps asking for that same stop it is answered, however
    /// many fresh decisions say so: the fired order is left to trade and
    /// nothing is re-armed. Cleared by a decision that asks for anything
    /// else ([`Oms::diff`]).
    fired: Option<(Trigger, Px)>,
}

impl<I: Copy + PartialEq> Rungs<I> {
    /// Whether the level at `price` has been answered for the decision made
    /// at `as_of`.
    fn is_spent(&self, as_of: NanoTime, price: Px) -> bool {
        self.spent.contains(&Some((as_of, price)))
    }

    /// Record the level at `price` answered for the decision at `as_of`,
    /// over the oldest record if every one is taken.
    ///
    /// `current` is the `as_of` of the decision remembered now, the only one
    /// [`Oms::wanted`] ever asks about. A record older than it can never
    /// match again, so it is dropped rather than stored: stored, a late
    /// reject from an older decision could evict a record of the current
    /// one, and that level would be sent again under the decision that had
    /// already had its answer.
    fn spend(&mut self, as_of: NanoTime, price: Px, current: NanoTime) {
        if as_of < current || self.is_spent(as_of, price) {
            return;
        }
        // `None` orders before any `Some`, so an empty record is taken first.
        if let Some(oldest) = self
            .spent
            .iter_mut()
            .min_by_key(|record| record.map(|(as_of, _)| as_of))
        {
            *oldest = Some((as_of, price));
        }
    }

    /// How many requests are held on this side.
    fn holds(&self) -> u32 {
        self.held.iter().filter(|held| held.is_some()).count() as u32
    }

    /// Hold `plan`, if it is not held already: whether it was newly held.
    fn hold(&mut self, plan: Plan<I>) -> bool {
        if self.held.contains(&Some(plan)) {
            return false;
        }
        match self.held.iter_mut().find(|held| held.is_none()) {
            Some(free) => {
                *free = Some(plan);
                true
            }
            None => false,
        }
    }

    /// Release `plan` if it is held: whether it was.
    fn release(&mut self, plan: Plan<I>) -> bool {
        match self.held.iter_mut().find(|held| **held == Some(plan)) {
            Some(held) => {
                *held = None;
                true
            }
            None => false,
        }
    }

    /// Whether every slot is idle.
    fn is_idle(&self) -> bool {
        self.slots.iter().all(|slot| matches!(slot, Slot::Idle))
    }
}

/// Which of an instrument's two remembered decisions an [`Entry`] holds: the
/// plain one — shown or crossed — or the triggered one (module docs,
/// Triggers). Ordered, so a walk of the entries is the instrument's order
/// and, within one, plain before triggered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Half {
    Plain,
    Triggered,
}

impl Half {
    /// Which half a desired is remembered in.
    const fn of(intent: Intent) -> Half {
        match intent {
            Intent::Trigger(_) => Half::Triggered,
            Intent::Rest | Intent::Cross => Half::Plain,
        }
    }
}

/// What the entries are keyed on: an instrument, and which of its decisions.
type Key<I> = (I, Half);

/// Both sides of one instrument, and the last thing decided about it.
///
/// The key is the instrument rather than `(instrument, side)`: the facts that are per-contract — whether
/// it is quotable at all, what was last wanted on it — would otherwise be split
/// across two entries that have to be kept agreeing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Entry<I> {
    bids: Rungs<I>,
    asks: Rungs<I>,
    /// The last [`Desired`] seen for this instrument. Its `as_of` is what
    /// [`Config::max_desired_age`] is measured against.
    desired: Desired<I>,
    /// The engine time the last crossing order on this instrument was sent,
    /// which [`Config::retake`] is measured from.
    crossed: Option<NanoTime>,
}

impl<I: Copy> Entry<I> {
    const fn rungs(&self, side: Side) -> &Rungs<I> {
        match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        }
    }

    fn rungs_mut(&mut self, side: Side) -> &mut Rungs<I> {
        match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }

    /// Whether a cross sent at `crossed` still holds the next one back.
    ///
    /// Judged on the time elapsed since, never as `crossed + retake`: a
    /// `NanoTime` plus a `Duration` is an unchecked `u64` add, and
    /// `Duration::MAX` — "never retake" — would overflow it.
    fn retaking(&self, now: NanoTime, retake: Duration) -> bool {
        self.crossed
            .is_some_and(|crossed| now < crossed || Duration::from(now - crossed) < retake)
    }
}

impl<I: Copy + PartialEq> Entry<I> {
    /// Nothing working on either side and no cross to space the next one
    /// from, so the entry holds no state worth keeping.
    fn is_quiet(&self, now: NanoTime, retake: Duration) -> bool {
        self.bids.is_idle() && self.asks.is_idle() && !self.retaking(now, retake)
    }

    /// Whether a level of the decision remembered now has been answered.
    /// Dropped, the entry would forget it, and the same decision restated
    /// would be sent again.
    fn has_spent(&self) -> bool {
        let as_of = self.desired.as_of;
        [&self.bids, &self.asks]
            .iter()
            .any(|rungs| rungs.spent.iter().flatten().any(|&(at, _)| at == as_of))
    }
}

/// Where a slot's order is, or is about to be, for matching it to a wanted
/// level: the price it rests at, the price an amend in flight moves it to,
/// or `None` for a slot that holds nothing a level can be matched to — idle,
/// or being pulled.
const fn target<I: Copy>(slot: Slot<I>) -> Option<Px> {
    match slot {
        Slot::Pending(placed) | Slot::Working(placed) => Some(placed.price),
        Slot::PendingAmend { to, .. } => Some(to.price),
        Slot::Idle | Slot::PendingCancel(_) => None,
    }
}

/// Levels wanted on one side, best first, and how many.
type Wanted = ([Option<Level>; MAX_DEPTH], usize);

/// Desireds and reports in, requests out, for a ladder of
/// resting orders per instrument and side — up to [`MAX_DEPTH`] — with no
/// parent/child.
///
/// One fold over one map, keyed on the instrument, holding a quoter's
/// contracts and a hedger's leg together — it is one venue and one id space,
/// and a slot pool would need a capacity choice that a book of tens of
/// instruments, not thousands, does not have to make.
#[derive(Clone, Debug)]
pub struct Oms<I> {
    config: Config,
    epoch: Epoch,
    next_id: u32,
    /// Ordered, so a walk of it is the stated order a burst is emitted in
    /// with no sort: see [`Oms::diff`].
    entries: BTreeMap<Key<I>, Entry<I>>,
    refused: u64,
    rejected: u64,
    adjusted: u64,
    unrouted: u64,
    cancel_all_refused: u64,
    /// Place, amend and cancel, at the budget.
    trading: Bucket,
    /// Cancel-all, at the budget.
    cancel_alls: Bucket,
    /// A cancel-all the budget could not pay for, sent at the head of the
    /// next diff that can.
    cancel_all_pending: bool,
    /// A cancel-all has been sent and nothing has yet answered for it. It is
    /// what lets an unattributed reject be read as that cancel-all's
    /// refusal; with none in flight, one is a refusal of something else and
    /// is only counted. A venue acknowledges a cancel-all with nothing on
    /// this edge but the cancels it caused, so the flag is spent by the first
    /// unattributed reject after the request, or by the last slot it pulled
    /// answering — after which an unattributed reject has nothing of the
    /// cancel-all's left to return. Never set by a cancel-all that pulled
    /// nothing: its refusal would have nothing to return either.
    cancel_all_in_flight: bool,
    /// When the last cancel-all was asked for. A desired decided before it is
    /// the decision the cancel-all pulled, and is not taken up again — not
    /// even after the entry it was remembered on has been dropped.
    cancel_all_at: NanoTime,
    /// The message-to-trade ratio, where the venue has one.
    ratio: Option<RatioMeter>,
    /// The venue's trading state: open until it says otherwise.
    state: TradingState,
    deferred: u64,
    superseded: u64,
    /// Plans held back by the budget across every entry — every side's
    /// `held` that are set — kept as a count where a hold is set or
    /// released, so [`Oms::pacing`], which a graph asks on every tick, does
    /// not walk every entry to answer.
    holds: u32,
    /// The diff's scratch, kept so a diff does not allocate for it: the
    /// keys it walks, what it plans (and for which slot), the plans in the
    /// order they are paid for, and which were. Emptied at the top of every
    /// diff and meaningless between them.
    keys: Vec<Key<I>>,
    plans: Vec<(Key<I>, Side, usize, Plan<I>)>,
    by_priority: Vec<usize>,
    paid: Vec<bool>,
    /// Per key in `keys`: whether either side wanted anything at the last
    /// diff, as the plan found it.
    wanting: Vec<bool>,
}

impl<I: Instrument> Oms<I> {
    /// An OMS running on `config`, with nothing working, minting ids in
    /// epoch zero — the bare counter.
    pub fn new(config: Config) -> Oms<I> {
        Oms::resumed(config, Epoch::ZERO)
    }

    /// An OMS running on `config`, with nothing working, minting ids in
    /// `epoch`.
    ///
    /// Nothing else is resumed, and that is the design rather than a gap:
    /// what was working when the last process stopped is the venue's to say,
    /// and it has said it by the time an order goes out — the session's
    /// connect frames pull everything and reconciling the venue's working
    /// orders on connect — a design of its own, not this module's — settles
    /// what was in flight. The epoch is the one thing the venue cannot tell
    /// us: which numbers the last process used.
    pub fn resumed(config: Config, epoch: Epoch) -> Oms<I> {
        let (trading, cancel_alls) = config.rate.budget();
        Oms {
            config,
            epoch,
            next_id: 1,
            entries: BTreeMap::new(),
            refused: 0,
            rejected: 0,
            adjusted: 0,
            unrouted: 0,
            cancel_all_refused: 0,
            trading: Bucket::new(trading),
            cancel_alls: Bucket::new(cancel_alls),
            cancel_all_pending: false,
            cancel_all_in_flight: false,
            cancel_all_at: NanoTime::ZERO,
            ratio: config.ratio.map(RatioMeter::new),
            state: TradingState::Open,
            deferred: 0,
            superseded: 0,
            holds: 0,
            keys: Vec::new(),
            plans: Vec::new(),
            by_priority: Vec::new(),
            paid: Vec::new(),
            wanting: Vec::new(),
        }
    }

    /// The config it runs on.
    pub const fn config(&self) -> &Config {
        &self.config
    }

    /// The epoch its ids are minted in.
    pub const fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// What is working, or in flight, nearest the touch on one side of one
    /// instrument: the slot whose order is best priced, or
    /// [`Slot::Idle`] where the side holds none. One level a side, it is
    /// the side's only slot; [`slots`](Self::slots) has them all.
    pub fn slot(&self, instrument: &I, side: Side) -> Slot<I> {
        self.slots(instrument, side)
            .into_iter()
            .filter_map(|slot| slot.placed().map(|placed| (placed.price, slot)))
            .reduce(|best, next| {
                if ahead(side, next.0, best.0) {
                    next
                } else {
                    best
                }
            })
            .map_or(Slot::Idle, |(_, slot)| slot)
    }

    /// Every slot on one side of one instrument, in no order of price:
    /// slots are not ranked (see [`Oms::diff`]).
    pub fn slots(&self, instrument: &I, side: Side) -> [Slot<I>; MAX_DEPTH] {
        self.slots_of((*instrument, Half::Plain), side)
    }

    /// Every slot of the triggered orders on one side of one instrument
    /// ([`Intent::Trigger`]), in no order: at most one holds an order the
    /// strategy wants, and another may hold one being pulled.
    pub fn triggered(&self, instrument: &I, side: Side) -> [Slot<I>; MAX_DEPTH] {
        self.slots_of((*instrument, Half::Triggered), side)
    }

    fn slots_of(&self, key: Key<I>, side: Side) -> [Slot<I>; MAX_DEPTH] {
        self.entries
            .get(&key)
            .map_or([Slot::Idle; MAX_DEPTH], |entry| entry.rungs(side).slots)
    }

    /// Desired levels the diff refused to send, because a venue would have
    /// refused them — a level with a quantity that is not positive — or
    /// because the OMS cannot work them — a crossing ladder deeper than one,
    /// which is refused whole. A metric, not an error: the level is
    /// withdrawn, which is always safe, and the count is what says the
    /// strategy upstream is producing them. Counted once per tick it is
    /// asked for, so it is a rate rather than a tally of distinct levels.
    pub const fn refused(&self) -> u64 {
        self.refused
    }

    /// Places and amends the venue rejected.
    ///
    /// Ordinary on a post-only book: the touch moved between the decision
    /// and the arrival, and the slot returns to idle — or, for an amend,
    /// stays where it rested — for the strategy to decide about again. It
    /// is the *rate* that means something — a storm
    /// of them is a quoter chasing a book it cannot reach, and latching on
    /// one is risk's.
    pub const fn rejected(&self) -> u64 {
        self.rejected
    }

    /// Acks that accepted something other than what was asked — a price
    /// rounded to the venue's tick, a size trimmed to its lot. The slot
    /// works what the venue says, and the decision counts as answered, so
    /// the strategy re-decides rather than the diff re-asking every instant.
    /// A rate: a strategy pricing off the venue's tick never produces one,
    /// and a steady count is a strategy that is not.
    pub const fn adjusted(&self) -> u64 {
        self.adjusted
    }

    /// Cancel-alls the venue refused.
    ///
    /// Each one returned what it had pending to working, to be pulled one
    /// by one. The count is what the venue is doing to the one request the
    /// gap playbook opens with, and a venue that keeps refusing it is a
    /// venue whose book cannot be flattened in one message.
    pub const fn cancel_all_refused(&self) -> u64 {
        self.cancel_all_refused
    }

    /// What the order-rate budget has held back — see the module docs.
    ///
    /// A read of counters, not a walk of the entries: [`OmsOp`] asks it on
    /// every cycle.
    pub const fn pacing(&self) -> Pacing {
        Pacing {
            deferred: self.deferred,
            superseded: self.superseded,
            waiting: self.holds + self.cancel_all_pending as u32,
        }
    }

    /// Reports that named an order no slot holds.
    ///
    /// Ordinary in small numbers — a cancel that crossed with a fill, a
    /// report for an order already retired — and what reconciliation of the
    /// venue's working orders exists to resolve in large ones, since a venue that keeps talking about an order
    /// we have forgotten is the definition of a book that needs
    /// reconciling.
    pub const fn unrouted(&self) -> u64 {
        self.unrouted
    }

    /// Apply a burst of reports, in order.
    ///
    /// In order because they are one stream for exactly that reason: an ack
    /// and the fill it precedes cannot be reordered, so neither can their
    /// effects on a slot.
    ///
    /// A report naming an id in the [reserved](ClientOrderId::RESERVED) half
    /// — a fill against a quote another minter sent — is passed over: it
    /// rides this stream so the position fold sees it, and names no order of
    /// the OMS's, neither a slot nor its message-to-trade ratio.
    pub fn apply(&mut self, reports: &[Report<I>]) {
        for report in reports {
            if report.order().is_some_and(ClientOrderId::is_reserved) {
                continue;
            }
            if let (Some(meter), Report::Fill(_)) = (self.ratio.as_mut(), report) {
                meter.filled();
            }
            self.apply_one(report);
            // The last order the cancel-all moved to pending has its answer:
            // a later unattributed reject is a refusal of something else.
            if self.cancel_all_in_flight && !self.any_pending_cancel() {
                self.cancel_all_in_flight = false;
            }
        }
    }

    /// The venue's trading state changed. While it is anything but open,
    /// the diff sends cancels and nothing else: places and amends are held,
    /// not spent — what is wanted is remembered, and goes out on the first
    /// diff after the venue opens, if it is still wanted and still fresh.
    pub fn trading(&mut self, state: TradingState) {
        self.state = state;
    }

    /// The venue's trading state, as last stated.
    pub const fn trading_state(&self) -> TradingState {
        self.state
    }

    /// Messages sent and fills heard, where the venue has a
    /// message-to-trade ratio.
    pub fn ratio(&self) -> Option<(u64, u64)> {
        self.ratio.map(|meter| meter.counts())
    }

    fn apply_one(&mut self, report: &Report<I>) {
        let Some(id) = report.order() else {
            // A refusal the venue could not attribute, while a cancel-all is
            // in flight, is that cancel-all's: it is the one request that
            // names no order. Every slot `cancel_all` moved to
            // `PendingCancel` is still resting, and left pending it would
            // never quote or hedge again — nothing else answers for it.
            // Returned to `Working`, the next diff pulls each of them singly
            // (nothing is wanted on them), and a `Cancelled` that does
            // arrive for one, or an `UnknownOrder` on the single cancel,
            // retires it as it would have anyway.
            //
            // With no cancel-all in flight it is a refusal of something else
            // — a session-level reject, a venue error nobody could pin on an
            // order — and it changes nothing: the slots whose single cancels
            // are in flight are still waiting on those cancels' own answers.
            if matches!(report, Report::Reject(_)) && self.cancel_all_in_flight {
                self.cancel_all_in_flight = false;
                self.on_cancel_all_refused();
            } else {
                self.unrouted += 1;
            }
            return;
        };
        let Some((key, side, index)) = self.side_of(id) else {
            self.unrouted += 1;
            return;
        };
        let Some(entry) = self.entries.get_mut(&key) else {
            self.unrouted += 1;
            return;
        };
        let desired_as_of = entry.desired.as_of;
        let asked = entry
            .desired
            .intent
            .trigger()
            .zip(entry.desired.side(side).best().map(|level| level.price));
        let rungs = entry.rungs_mut(side);
        let slot = rungs.slots[index];
        let (next, routed) = Self::transition(slot, report);
        rungs.slots[index] = next;
        if !routed {
            self.unrouted += 1;
            return;
        }
        // A place the venue refused is not sent again until the strategy has
        // decided afresh. What is spent is the decision the order was placed
        // from, not whatever is remembered now: a decision that arrived
        // while the place was in flight was never asked, and the next diff
        // sends it. And it is that level, not the side: the rest of the
        // ladder was not refused. Recorded here rather than in `transition`,
        // which is a function of a state and an event and must not reach the
        // entry.
        if let (Slot::Pending(resting), Report::Reject(_)) = (slot, report) {
            rungs.spend(resting.as_of, resting.price, desired_as_of);
            self.rejected += 1;
        }
        // An amend the venue refused, for any reason but the order being
        // gone, leaves the order resting where it was — and asking the same
        // amend again would be the same question of the same book, asked
        // on the instant the answer arrived wherever the reject wakes the
        // fold. So the level it asked for counts as answered by the order
        // that rests, exactly as an accept at the venue's own tick does:
        // the next diff leaves the order alone, and the strategy's next
        // decision asks afresh. Not spent: a spent level is one that is
        // not wanted, and the order resting there would be pulled for it.
        if let (Slot::PendingAmend { working, to }, Report::Reject(reject)) = (slot, report)
            && reject.reason != RejectReason::UnknownOrder
        {
            rungs.answered[index] = Some((working.as_of, to.price));
            self.rejected += 1;
        }
        // The venue accepted something other than what was asked — a price
        // rounded to its tick, a size trimmed to its lot. The slot now says
        // what rests, so the next diff would see a level that differs from
        // the decision and ask for the decision again, and the venue would
        // round it again: an instant-by-instant loop, like re-sending a
        // refused post-only. So the decision is marked answered, and the
        // working order is left as the venue rests it until the strategy's
        // next decision — which ought to be on the venue's tick — asks again.
        if let (Some((asked_price, asked_remaining)), Report::Ack(ack)) =
            (Self::asked(slot), report)
            && (ack.price.is_some_and(|price| price != asked_price)
                || ack
                    .remaining
                    .is_some_and(|remaining| remaining != asked_remaining))
        {
            let as_of = slot.placed().map_or(desired_as_of, |placed| placed.as_of);
            rungs.answered[index] = Some((as_of, asked_price));
            self.adjusted += 1;
        }
        // A triggered order fired — said by the venue, or by a fill, since
        // only an order that has fired can trade. The stop asked for now is
        // answered: the fired order is left to trade, and while the
        // strategy keeps asking for that stop nothing re-arms it — not when
        // the order fills away, and not on the next decision that only
        // restates it.
        let fired = matches!(report, Report::Triggered(_))
            || matches!(report, Report::Fill(_))
                && slot.placed().is_some_and(|placed| placed.trigger.is_some());
        if fired && key.1 == Half::Triggered {
            rungs.fired = asked;
        }
    }

    /// The price and showing size a slot's request in flight asked for, for
    /// a place or an amend — what an ack is compared against.
    const fn asked(slot: Slot<I>) -> Option<(Px, Qty)> {
        match slot {
            Slot::Pending(resting) => Some((resting.price, resting.remaining)),
            Slot::PendingAmend { to, .. } => Some((to.price, to.qty)),
            Slot::Idle | Slot::Working(_) | Slot::PendingCancel(_) => None,
        }
    }

    /// The state machine itself: one slot and one report in, the slot's next
    /// state and whether the report meant anything to it out.
    ///
    /// An associated function rather than a method, so that it is exactly what
    /// a state machine is — a function of a state and an event — and cannot
    /// reach the rest of the OMS to do anything else.
    fn transition(slot: Slot<I>, report: &Report<I>) -> (Slot<I>, bool) {
        let next = match (slot, report) {
            // The venue accepted what was asked for.
            // What rests is what the venue says rests, where it says: a
            // rounded price or a trimmed size is the venue's, not the ask.
            (Slot::Pending(resting), Report::Ack(ack)) => Slot::Working(Placed {
                price: ack.price.unwrap_or(resting.price),
                remaining: ack.remaining.unwrap_or(resting.remaining),
                ..resting
            }),
            (Slot::PendingAmend { working, to }, Report::Ack(ack)) => Slot::Working(Placed {
                price: ack.price.unwrap_or(to.price),
                trigger: to.trigger,
                qty: to.qty,
                // An amend re-shows the whole amended size: the venue is
                // resting `to.qty`, not what was left of the old order.
                remaining: ack.remaining.unwrap_or(to.qty),
                ..working
            }),

            // A place the venue refused. Ordinary when it is post-only and
            // the touch moved: the slot goes idle and the next tick
            // re-decides.
            (Slot::Pending(_), Report::Reject(_)) => Slot::Idle,
            // An amend or a cancel the venue refused. `UnknownOrder` is the
            // one reason that says something about what is *working* — the
            // order is gone — and every other reason leaves it resting where
            // it was.
            (
                Slot::PendingAmend { working, .. } | Slot::PendingCancel(working),
                Report::Reject(reject),
            ) => {
                if reject.reason == RejectReason::UnknownOrder {
                    Slot::Idle
                } else {
                    Slot::Working(working)
                }
            }

            // An execution, in whatever state the slot is in: a fill can
            // arrive while an amend is in flight, and the amend's own reject
            // follows it. The slot retires when nothing is left showing, and
            // a partial does not retire it.
            (_, Report::Fill(fill)) if fill.remaining <= Qty::ZERO => Slot::Idle,
            (Slot::Pending(resting), Report::Fill(fill)) => {
                Slot::Pending(resting.filled_to(fill.remaining))
            }
            (Slot::Working(resting), Report::Fill(fill)) => {
                Slot::Working(resting.filled_to(fill.remaining))
            }
            (Slot::PendingCancel(resting), Report::Fill(fill)) => {
                Slot::PendingCancel(resting.filled_to(fill.remaining))
            }
            (Slot::PendingAmend { working, to }, Report::Fill(fill)) => Slot::PendingAmend {
                working: working.filled_to(fill.remaining),
                to,
            },

            // A triggered order fired: what it is changed, not whether it is
            // ours or what it is waiting on. An amend in flight under the old
            // trigger is now an amend of a plain order, whatever the venue
            // makes of it.
            (Slot::Pending(resting), Report::Triggered(_)) => Slot::Pending(resting.fired()),
            (Slot::Working(resting), Report::Triggered(_)) => Slot::Working(resting.fired()),
            (Slot::PendingCancel(resting), Report::Triggered(_)) => {
                Slot::PendingCancel(resting.fired())
            }
            (Slot::PendingAmend { working, to }, Report::Triggered(_)) => Slot::PendingAmend {
                working: working.fired(),
                to: Amend {
                    trigger: None,
                    ..to
                },
            },

            // The order is off the book, however it got there — our cancel,
            // the venue's cancel-on-disconnect or MMP, a lifetime that ran
            // out, a contract that expired under it.
            (_, Report::Cancelled(_) | Report::Expired(_)) => Slot::Idle,

            // Anything else is a report for a state that cannot produce it —
            // an ack for an order already working, a reject for one nobody
            // asked about. Counted, not acted on.
            (state, _) => return (state, false),
        };
        (next, true)
    }

    /// `CancelAll` refused: what it moved to `PendingCancel` is still
    /// resting, and says so again.
    fn on_cancel_all_refused(&mut self) {
        let mut returned = 0;
        for entry in self.entries.values_mut() {
            for slot in entry.bids.slots.iter_mut().chain(&mut entry.asks.slots) {
                if let Slot::PendingCancel(resting) = *slot {
                    *slot = Slot::Working(resting);
                    returned += 1;
                }
            }
        }
        self.cancel_all_refused += 1;
        log::warn!(
            "oms: cancel-all refused with {returned} order(s) pending on it; \
             pulling them one by one"
        );
    }

    /// Whether any slot has a cancel in flight.
    fn any_pending_cancel(&self) -> bool {
        self.entries.values().any(|entry| {
            entry
                .bids
                .slots
                .iter()
                .chain(&entry.asks.slots)
                .any(|slot| matches!(slot, Slot::PendingCancel(_)))
        })
    }

    /// Which slot of which side of which instrument holds `id`, if any.
    ///
    /// A scan, not an index. The book is tens of instruments, not thousands —
    /// the same argument that makes this one fold instead of a slot pool — so
    /// a linear walk of its slots, [`MAX_DEPTH`] a side, is cheaper than the
    /// `ClientOrderId → (instrument, side, slot)` map it would otherwise
    /// take, and, unlike that map, it cannot fall out of agreement with the
    /// thing it indexes. If the book ever is thousands, this is the line to
    /// revisit, beside the diff's walk (module docs).
    fn side_of(&self, id: ClientOrderId) -> Option<(Key<I>, Side, usize)> {
        self.entries.iter().find_map(|(key, entry)| {
            [Side::Bid, Side::Ask].into_iter().find_map(|side| {
                entry
                    .rungs(side)
                    .slots
                    .iter()
                    .position(|slot| slot.id() == Some(id))
                    .map(|index| (*key, side, index))
            })
        })
    }

    /// Take in what the strategy wants, and emit what closes the gap.
    ///
    /// `now` is the reader's own engine time (`Ctx::time`), which is what
    /// [`Config::max_desired_age`] is judged at and what the budget refills
    /// to.
    ///
    /// Desireds are merged into the remembered state *first* and the requests
    /// emitted afterwards, at most one per slot — which is why two desireds
    /// for one instrument in one burst cannot become two requests. Then the
    /// whole diff is planned, paid for in priority order, and what was paid
    /// for is sent in walk order (module docs, the budget): instrument by
    /// instrument, bids before asks, and within a side cancels (worst price
    /// first), then amends, then places (each best level first).
    ///
    /// Each side's wanted ladder is matched to its slots in three passes:
    ///
    /// 1. **Keep what already matches.** A slot whose price equals a wanted
    ///    level's stays, amended only if its showing quantity differs.
    /// 2. **Amend the rest by rank**, best slot to best level — but never
    ///    past a level pass 1 kept: an amend moves an order within the gap
    ///    between kept levels it sits in, not through them.
    /// 3. **Cancel the excess, place the shortfall.** A working slot pass 2
    ///    left unpaired is cancelled; a wanted level left unpaired is placed
    ///    in an idle slot.
    ///
    /// Matching by price is what keeps a ladder shifting one tick from
    /// re-pricing every order: `[100, 99, 98]` becoming `[99, 98, 97]` keeps
    /// 99 and 98 and their queue place, and — since 100 cannot be moved to
    /// 97 through them — pulls 100 and places 97, the cancel paid for first
    /// when the budget is short.
    ///
    /// A slot with a request in flight is matched like any other, by the
    /// price it will rest at, but emits nothing: the level it answers is
    /// spoken for, which is what stops a place in flight being placed again
    /// beside itself, and the diff re-runs when its report lands. A slot
    /// being pulled answers for nothing.
    pub fn diff(&mut self, now: NanoTime, desired: &[Desired<I>]) -> Burst<Request<I>> {
        for want in desired {
            // Decided before the last cancel-all: that decision was pulled,
            // and only a fresh one re-places.
            if want.as_of < self.cancel_all_at {
                continue;
            }
            let key = (want.instrument, Half::of(want.intent));
            let entry = self.entries.entry(key).or_default();
            // The later decision wins, whatever order the burst arrived in.
            if entry.desired.as_of <= want.as_of {
                entry.desired = *want;
                // A fired stop stays answered only while the same stop is
                // asked for.
                for side in [Side::Bid, Side::Ask] {
                    let asked = want
                        .intent
                        .trigger()
                        .zip(want.side(side).best().map(|level| level.price));
                    let rungs = entry.rungs_mut(side);
                    if rungs.fired.is_some() && rungs.fired != asked {
                        rungs.fired = None;
                    }
                }
            }
        }

        let mut requests = Burst::new();
        // A cancel-all that waited goes first, from its own bucket, before
        // anything is planned: what it pulls is then in flight and plans
        // nothing.
        if self.cancel_all_pending && self.cancel_alls.try_take(now) {
            self.cancel_all_pending = false;
            requests.push(self.pull_all());
        }

        // In key order, because the map is ordered: this burst has to be the
        // same one on a replay as on the run it replays, and the order is
        // the map's rather than a sort's. Collected, not walked in place,
        // because planning and committing mutate the entries; the scratch is
        // the OMS's, so this does not allocate once it has grown.
        let mut keys = std::mem::take(&mut self.keys);
        let mut plans = std::mem::take(&mut self.plans);
        let mut by_priority = std::mem::take(&mut self.by_priority);
        let mut paid = std::mem::take(&mut self.paid);
        let mut wanting = std::mem::take(&mut self.wanting);
        keys.clear();
        plans.clear();
        by_priority.clear();
        paid.clear();
        wanting.clear();
        keys.extend(self.entries.keys().copied());

        for &key in &keys {
            // Whether either side wants anything, as the plan found it: the
            // pruning below reads this rather than asking again.
            let mut wants = false;
            for side in [Side::Bid, Side::Ask] {
                let from = plans.len();
                wants |= self.plan(now, key, side, &mut plans) > 0;
                // A request that waited and is no longer what is planned was
                // superseded: dropped, never sent stale.
                let planned = &plans[from..];
                let entry = self.entries.get_mut(&key).expect("a listed key");
                for held in &mut entry.rungs_mut(side).held {
                    if held.is_some_and(|held| !planned.iter().any(|&(.., plan)| plan == held)) {
                        self.superseded += 1;
                        self.holds -= 1;
                        *held = None;
                    }
                }
            }
            wanting.push(wants);
        }

        // Paid for by priority, walk order within one: the index breaks the
        // tie, so an unstable sort gives the stable order without the
        // buffer a stable one allocates.
        by_priority.extend(0..plans.len());
        by_priority.sort_unstable_by_key(|&i| (plans[i].3.priority(), i));
        paid.resize(plans.len(), false);
        for &i in &by_priority {
            // The ratio holds back what adds risk; a cancel is counted and
            // never held.
            let adds_risk = !matches!(plans[i].3, Plan::Cancel(_));
            if adds_risk && self.ratio.is_some_and(|meter| !meter.allows()) {
                continue;
            }
            if !self.trading.try_take(now) {
                break;
            }
            if let Some(meter) = self.ratio.as_mut() {
                meter.sent();
            }
            paid[i] = true;
        }

        for (&(key, side, index, plan), &paid) in plans.iter().zip(&paid) {
            let rungs = self
                .entries
                .get_mut(&key)
                .expect("a listed key")
                .rungs_mut(side);
            if paid {
                if rungs.release(plan) {
                    self.holds -= 1;
                }
                requests.push(self.commit(now, key, side, index, plan));
            } else if rungs.hold(plan) {
                // Counted once, however many diffs it then waits through.
                self.deferred += 1;
                self.holds += 1;
            }
        }

        for (&key, &wants) in keys.iter().zip(&wanting) {
            // An instrument with nothing working and nothing wanted holds no
            // state, and options expire: without this the map grows for the
            // life of the process.
            let entry = &self.entries[&key];
            if !wants
                && entry.is_quiet(now, self.config.retake)
                && !(entry.has_spent() && self.fresh(now, entry.desired.as_of))
            {
                // Nothing quiet and unwanted plans anything, so nothing is
                // held on it; released all the same, so the count cannot
                // drift from the entries it counts.
                if let Some(entry) = self.entries.remove(&key) {
                    self.holds -= entry.bids.holds() + entry.asks.holds();
                }
            }
        }

        self.keys = keys;
        self.plans = plans;
        self.by_priority = by_priority;
        self.paid = paid;
        self.wanting = wanting;
        requests
    }

    /// What one side of one instrument should be asked to do, if anything —
    /// planned, not yet sent: nothing here moves a slot or mints an id. The
    /// plans are pushed onto `out` in the side's stated order, each with the
    /// slot it is for. Returns how many levels the side wants, which
    /// [`diff`](Self::diff) reads to prune an entry that wants nothing.
    ///
    /// The passes are [`diff`](Self::diff)'s.
    fn plan(
        &mut self,
        now: NanoTime,
        key: Key<I>,
        side: Side,
        out: &mut Vec<(Key<I>, Side, usize, Plan<I>)>,
    ) -> usize {
        // Counted once the entry is no longer borrowed, so the plan reads
        // the side in place rather than copying it out.
        let (refused, depth) = match key.1 {
            Half::Plain => self.plan_side(now, key, side, out),
            Half::Triggered => self.plan_triggered(now, key, side, out),
        };
        self.refused += refused;
        depth
    }

    /// [`plan`](Self::plan)'s body, borrowing the entry: what it planned
    /// went onto `out`, and it returns how many levels were refused and how
    /// many are wanted.
    fn plan_side(
        &self,
        now: NanoTime,
        key: Key<I>,
        side: Side,
        out: &mut Vec<(Key<I>, Side, usize, Plan<I>)>,
    ) -> (u64, usize) {
        let Some(entry) = self.entries.get(&key) else {
            return (0, 0);
        };
        let instrument = key.0;
        let ((wanted, depth), refused) = self.wanted(now, entry, side);
        let rungs = entry.rungs(side);
        let desired = &entry.desired;
        let retaking = entry.retaking(now, self.config.retake);

        // A book that is not open takes a cancel and nothing else. Held, not
        // spent: the decision stands for the open.
        let open = self.state == TradingState::Open;
        let reduce_only = desired.reduce_only;

        if desired.intent == Intent::Cross {
            // A resting order cannot be amended into a crossing one, and one
            // request in flight per slot is not relaxed for it: pull every
            // one on the side, and the diff that finds the side idle crosses.
            // Nothing wanted pulls them whether the book is open or not.
            // The cross itself, acked but not yet filled or killed — a
            // venue that reports in FIX's order says it is working for an
            // instant — is not among them: its level was spent when it was
            // sent, and an immediate-or-cancel order cannot rest, so there
            // is nothing to pull and the venue would only say so.
            for (index, slot) in rungs.slots.iter().enumerate() {
                if let Slot::Working(resting) = *slot
                    && (depth == 0 || open)
                    && !rungs.is_spent(resting.as_of, resting.price)
                {
                    out.push((key, side, index, Plan::Cancel(resting)));
                }
            }
            // Held, not spent, while retaking: the decision still stands,
            // and the next diff past the interval sends it if nothing newer
            // has replaced it.
            if let Some(level) = wanted[0]
                && open
                && !retaking
                && rungs.is_idle()
            {
                let plan = Plan::Place {
                    level,
                    cross: true,
                    reduce_only,
                    trigger: None,
                };
                out.push((key, side, 0, plan));
            }
            return (refused, depth);
        }

        // Which wanted level each slot answers, and which slots each level
        // is answered by.
        let mut answers: [Option<usize>; MAX_DEPTH] = [None; MAX_DEPTH];
        let mut answered_by: [Option<usize>; MAX_DEPTH] = [None; MAX_DEPTH];
        // The prices pass 1 kept, which pass 2 does not amend through.
        let mut kept: [Option<Px>; MAX_DEPTH] = [None; MAX_DEPTH];

        // Pass 1: keep what matches by price — or, for an order the venue
        // rested at its own tick, what its ask was.
        for (index, slot) in rungs.slots.iter().enumerate() {
            let Some(price) = target(*slot) else {
                continue;
            };
            let asked = rungs.answered[index]
                .filter(|&(as_of, _)| as_of == desired.as_of)
                .map(|(_, asked)| asked);
            let open_level = |rank: usize, wanted_at: Px| {
                answered_by[rank].is_none() && wanted[rank].is_some_and(|l| l.price == wanted_at)
            };
            // The ask first: a venue that rounded it onto another wanted
            // level's price must not take that level from the order that
            // rests there.
            let rank = asked
                .and_then(|asked| (0..depth).find(|&rank| open_level(rank, asked)))
                .or_else(|| (0..depth).find(|&rank| open_level(rank, price)));
            if let Some(rank) = rank {
                answers[index] = Some(rank);
                answered_by[rank] = Some(index);
                kept[rank] = wanted[rank].map(|level| level.price);
            }
        }

        // How many kept prices are nearer the touch than `price`: the gap
        // between kept levels it sits in.
        let gap = |price: Px| {
            kept.iter()
                .flatten()
                .filter(|&&kept| ahead(side, kept, price))
                .count()
        };

        // Pass 2: the slots pass 1 left, best first (the index breaks a tie,
        // which only an ack at the venue's own tick can make), each to the
        // best level left in its gap.
        let mut spare: [Option<(Px, usize)>; MAX_DEPTH] = [None; MAX_DEPTH];
        let mut spares = 0;
        for (index, slot) in rungs.slots.iter().enumerate() {
            if let Some(price) = target(*slot)
                && answers[index].is_none()
            {
                spare[spares] = Some((price, index));
                spares += 1;
            }
        }
        spare[..spares].sort_unstable_by(|a, b| match (a, b) {
            (Some((a_price, a_index)), Some((b_price, b_index))) => {
                touch_order(side, *a_price, *b_price).then(a_index.cmp(b_index))
            }
            _ => Ordering::Equal,
        });
        for &(price, index) in spare[..spares].iter().flatten() {
            let within = gap(price);
            let rank = (0..depth).find(|&rank| {
                answered_by[rank].is_none()
                    && wanted[rank].is_some_and(|level| gap(level.price) == within)
            });
            if let Some(rank) = rank {
                answers[index] = Some(rank);
                answered_by[rank] = Some(index);
            }
        }

        // Pass 3's cancels: what is working and answers nothing, worst
        // first. Unwanted, it is pulled whether the book is open or not.
        for &(_, index) in spare[..spares].iter().rev().flatten() {
            if let (None, Slot::Working(resting)) = (answers[index], rungs.slots[index]) {
                out.push((key, side, index, Plan::Cancel(resting)));
            }
        }

        // The amends, best level first: a working slot whose level has moved
        // or resized. The second invariant — nothing is emitted that matches
        // what is working — and the comparison is against what is *still
        // showing*, so a partial fill re-shows rather than leaving the quote
        // short.
        for rank in 0..depth {
            let (Some(index), Some(level)) = (answered_by[rank], wanted[rank]) else {
                continue;
            };
            let Slot::Working(resting) = rungs.slots[index] else {
                // In flight: the first invariant, and it emits nothing.
                continue;
            };
            // The venue has already answered this decision, at its own tick
            // or lot: what rests is its answer, and asking again would be
            // asking the same question of the same book.
            let answered = rungs.answered[index].is_some_and(|(as_of, _)| as_of == desired.as_of);
            if !open || answered {
                continue;
            }
            let moved = self.worth_requote(level.price, resting.price);
            let resized = level.qty != resting.remaining;
            if !moved && !resized {
                continue;
            }
            let amend = Amend {
                order: resting.id,
                instrument,
                price: level.price,
                qty: level.qty,
                trigger: None,
            };
            out.push((key, side, index, Plan::Amend(resting, amend)));
        }

        // Pass 3's places: what is wanted and answered by nothing, best
        // first, each into the first idle slot.
        if !open {
            return (refused, depth);
        }
        let mut idle = rungs
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| matches!(slot, Slot::Idle))
            .map(|(index, _)| index);
        for rank in 0..depth {
            let (None, Some(level)) = (answered_by[rank], wanted[rank]) else {
                continue;
            };
            let Some(index) = idle.next() else {
                // Every slot is taken, some by an order being pulled: the
                // level waits for one to come free.
                break;
            };
            let plan = Plan::Place {
                level,
                cross: false,
                reduce_only,
                trigger: None,
            };
            out.push((key, side, index, plan));
        }
        (refused, depth)
    }

    /// The triggered half's plan for one side: at most one order a side,
    /// so none of [`diff`](Self::diff)'s ladder passes, and one rule
    /// [`plan_side`](Self::plan_side) has no use for — an order that fired
    /// (module docs, Triggers).
    ///
    /// Walked in slot order, and what it plans is cancels and an amend in
    /// that walk, then the place.
    fn plan_triggered(
        &self,
        now: NanoTime,
        key: Key<I>,
        side: Side,
        out: &mut Vec<(Key<I>, Side, usize, Plan<I>)>,
    ) -> (u64, usize) {
        let Some(entry) = self.entries.get(&key) else {
            return (0, 0);
        };
        let ((wanted, depth), refused) = self.wanted(now, entry, side);
        let rungs = entry.rungs(side);
        let desired = &entry.desired;
        let open = self.state == TradingState::Open;
        let want = wanted[0].zip(desired.intent.trigger());
        // The stop asked for is the one the venue already fired: answered,
        // and the order the firing made is what works it.
        let done = want.is_some_and(|(level, trigger)| rungs.fired == Some((trigger, level.price)));
        // An accept at the venue's own tick answers this decision.
        let answered =
            |index: usize| rungs.answered[index].is_some_and(|(as_of, _)| as_of == desired.as_of);

        // Whether a slot already answers what is wanted.
        let mut claimed = false;
        for (index, slot) in rungs.slots.iter().enumerate() {
            match *slot {
                // Fired: the plain order the trigger made. Kept while the
                // strategy asks for the stop that fired — however many
                // fresh decisions restate it — and pulled once it asks for
                // anything else, or nothing, or goes stale. Never amended:
                // a venue will not put a fired order back under a trigger.
                Slot::Working(resting) if resting.trigger.is_none() => {
                    if !done {
                        out.push((key, side, index, Plan::Cancel(resting)));
                    }
                }
                Slot::Working(resting) => {
                    let Some((level, trigger)) = want.filter(|_| !claimed && !done) else {
                        // Unwanted, it is pulled whether the book is open or not.
                        out.push((key, side, index, Plan::Cancel(resting)));
                        continue;
                    };
                    claimed = true;
                    // A stop that has become a take, or watches another
                    // price, is a different order: pulled, and placed once
                    // the pull is answered — no venue need edit more of a
                    // trigger than its price.
                    if resting.trigger.is_some_and(|was| {
                        was.kind != trigger.kind || was.reference != trigger.reference
                    }) {
                        out.push((key, side, index, Plan::Cancel(resting)));
                        continue;
                    }
                    if !open || answered(index) {
                        continue;
                    }
                    let moved = self.worth_requote(level.price, resting.price);
                    let resized = level.qty != resting.remaining;
                    let retriggered = resting
                        .trigger
                        .is_some_and(|was| self.worth_requote(was.price, trigger.price));
                    if moved || resized || retriggered {
                        let amend = Amend {
                            order: resting.id,
                            instrument: key.0,
                            price: level.price,
                            qty: level.qty,
                            trigger: Some(trigger),
                        };
                        out.push((key, side, index, Plan::Amend(resting, amend)));
                    }
                }
                // In flight: the first invariant. It emits nothing, and a
                // place waits for it.
                Slot::Pending(_) | Slot::PendingAmend { .. } => claimed = true,
                Slot::Idle | Slot::PendingCancel(_) => {}
            }
        }
        // A new stop goes out only onto a clear book: nothing triggered on
        // this side — an order being pulled included, so a replacement
        // never rests beside what it replaces — and nothing on the other,
        // which a one-sided stop always wants pulled first.
        let clear = rungs.is_idle() && entry.rungs(side.opposite()).is_idle();
        if let Some((level, trigger)) = want
            && !done
            && clear
            && open
        {
            let plan = Plan::Place {
                level,
                cross: false,
                reduce_only: desired.reduce_only,
                trigger: Some(trigger),
            };
            out.push((key, side, 0, plan));
        }
        (refused, depth)
    }

    /// Whether a move from `a` to `b` is worth a message: any move at all,
    /// and at least [`Config::min_requote`].
    fn worth_requote(&self, a: Px, b: Px) -> bool {
        let by = (a.raw() - b.raw()).abs();
        by > 0 && by >= self.config.min_requote.raw()
    }

    /// Send a plan the budget has paid for: mint its id, move its slot.
    fn commit(
        &mut self,
        now: NanoTime,
        key: Key<I>,
        side: Side,
        index: usize,
        plan: Plan<I>,
    ) -> Request<I> {
        let instrument = key.0;
        match plan {
            Plan::Place {
                level,
                cross,
                reduce_only,
                trigger,
            } => {
                let id = self.take_id();
                let order = if trigger.is_some() {
                    // A limit, whatever the passive kind: once it fires it
                    // is there to trade, and its price is the cap.
                    Order {
                        created: now,
                        tif: self.config.lifetime.tif(),
                        reduce_only,
                        trigger,
                        ..Order::limit(id, instrument, side, level.qty, level.price)
                    }
                } else if cross {
                    Order {
                        created: now,
                        tif: TimeInForce::ImmediateOrCancel,
                        reduce_only,
                        ..Order::limit(id, instrument, side, level.qty, level.price)
                    }
                } else {
                    let resting = match self.config.passive {
                        Passive::PostOnly => Order::post_only,
                        Passive::Limit => Order::limit,
                    };
                    Order {
                        created: now,
                        reduce_only,
                        tif: self.config.lifetime.tif(),
                        ..resting(id, instrument, side, level.qty, level.price)
                    }
                };
                let entry = self.entries.get_mut(&key).expect("a planned key");
                let as_of = entry.desired.as_of;
                let placed = Placed {
                    id,
                    price: level.price,
                    qty: level.qty,
                    remaining: level.qty,
                    as_of,
                    trigger,
                };
                if cross {
                    // One-shot: whatever the venue does with it, this
                    // decision has had its answer.
                    entry.crossed = Some(now);
                }
                let rungs = entry.rungs_mut(side);
                rungs.slots[index] = Slot::Pending(placed);
                rungs.answered[index] = None;
                if cross {
                    rungs.spend(as_of, level.price, as_of);
                }
                Request::Place(order)
            }
            Plan::Cancel(resting) => {
                self.entries
                    .get_mut(&key)
                    .expect("a planned key")
                    .rungs_mut(side)
                    .slots[index] = Slot::PendingCancel(resting);
                Request::Cancel(resting.id)
            }
            Plan::Amend(working, to) => {
                let entry = self.entries.get_mut(&key).expect("a planned key");
                // The decision this amend asks: what a reject of it marks
                // answered, and what an accept at the venue's own tick
                // answers.
                let working = Placed {
                    as_of: entry.desired.as_of,
                    ..working
                };
                entry.rungs_mut(side).slots[index] = Slot::PendingAmend { working, to };
                Request::Amend(to)
            }
        }
    }

    /// What is wanted on one side, best first, once staleness and
    /// sendability are applied, and how many levels were asked for that
    /// could not be sent.
    ///
    /// An empty ladder covers all three ways a side ends up showing nothing:
    /// the strategy said so, the strategy stopped saying anything, or it
    /// asked for what no venue would take. The count separates the third
    /// from the other two, because it is the only one that says something is
    /// wrong upstream.
    fn wanted(&self, now: NanoTime, entry: &Entry<I>, side: Side) -> (Wanted, u64) {
        let mut wanted = [None; MAX_DEPTH];
        let as_of = entry.desired.as_of;
        if !self.fresh(now, as_of) {
            return ((wanted, 0), 0);
        }
        let ladder = entry.desired.side(side);
        // A cross is a cap, not a shape: which rank of a crossing ladder
        // would the cap be? Refused whole rather than guessed at. A trigger
        // likewise, and a trigger on both sides is refused on each: one
        // trigger price for a buy and a sell fires whichever way the
        // market goes.
        match entry.desired.intent {
            Intent::Cross if ladder.depth() > 1 => return ((wanted, 0), 1),
            Intent::Trigger(_)
                if ladder.depth() > 1
                    || !ladder.is_empty() && !entry.desired.side(side.opposite()).is_empty() =>
            {
                return ((wanted, 0), 1);
            }
            _ => {}
        }
        let (mut depth, mut refused) = (0, 0);
        for level in ladder.iter() {
            // The venue has already refused this level of this decision.
            // Sending it again would be asking the same question of the same
            // book and getting the same answer — and, wired into a graph
            // where the rejection wakes this fold, it would be asked again on
            // the instant the answer arrived, for as long as the decision
            // stayed fresh. "The next tick re-decides" means
            // the *strategy* re-decides; this is the line that makes that
            // true.
            if entry.rungs(side).is_spent(as_of, level.price) {
                continue;
            }
            if level.qty <= Qty::ZERO {
                refused += 1;
                continue;
            }
            wanted[depth] = Some(level);
            depth += 1;
        }
        // Best first, whichever way the ladder was built: the passes pair
        // slots and levels in that order.
        wanted[..depth].sort_unstable_by(|a, b| match (a, b) {
            (Some(a), Some(b)) => touch_order(side, a.price, b.price),
            _ => Ordering::Equal,
        });
        ((wanted, depth), refused)
    }

    /// Whether a decision made at `as_of` is still believed at `now`
    /// ([`Config::max_desired_age`]).
    fn fresh(&self, now: NanoTime, as_of: NanoTime) -> bool {
        // The age elapsed, not `as_of + max_desired_age`: that add is an
        // unchecked `u64` one, and `Duration::MAX` would overflow it.
        !(now > as_of && Duration::from(now - as_of) > self.config.max_desired_age)
    }

    /// Pull everything, on every instrument — the first move of a kill
    /// switch.
    ///
    /// Every remembered desired is dropped at once, so nothing is wanted
    /// afterwards and nothing is re-placed until the strategy decides again:
    /// a desired decided before `now` is not taken up after it.
    /// The request itself spends a token from the cancel-all budget: `None`
    /// when there is none at `now`, and it then goes at the head of the first
    /// [`diff`](Self::diff) that can pay for it — meanwhile that diff pulls
    /// what is working one cancel at a time, since nothing is wanted on it.
    ///
    /// Slots with a request already in flight are left alone, because the
    /// invariant is one in flight per slot and this is not an exception to
    /// it: a place whose ack lands after this becomes a working order that
    /// nothing wants, and the next diff cancels it.
    pub fn cancel_all(&mut self, now: NanoTime) -> Option<Request<I>> {
        self.cancel_all_at = self.cancel_all_at.max(now);
        for entry in self.entries.values_mut() {
            entry.desired = Desired::default();
        }
        if self.cancel_alls.try_take(now) {
            self.cancel_all_pending = false;
            return Some(self.pull_all());
        }
        if !self.cancel_all_pending {
            self.deferred += 1;
            self.cancel_all_pending = true;
        }
        None
    }

    /// Move everything working to pending cancel, for a cancel-all that is
    /// being sent.
    fn pull_all(&mut self) -> Request<I> {
        if let Some(meter) = self.ratio.as_mut() {
            meter.sent();
        }
        let mut pulled = false;
        for entry in self.entries.values_mut() {
            for slot in entry.bids.slots.iter_mut().chain(&mut entry.asks.slots) {
                if let Slot::Working(resting) = *slot {
                    *slot = Slot::PendingCancel(resting);
                    pulled = true;
                }
            }
        }
        self.cancel_all_in_flight = pulled;
        Request::CancelAll
    }

    /// The next client order id.
    ///
    /// A counter under the process's epoch: replay-deterministic, which is
    /// what matters here, and distinct per order, which is what defuses
    /// `TimeQueue` dedup on the feedback edge. The counter wraps within its
    /// epoch below [`ClientOrderId::RESERVED`] — two billion requests on,
    /// skipping zero, which is the id of no order (a settlement's) — because
    /// the sequence's top half is another minter's. An order that rested through two billion
    /// requests of its own process is the one case that could meet its own
    /// number, and nothing here rests that long.
    fn take_id(&mut self) -> ClientOrderId {
        let id = ClientOrderId::new(self.epoch, self.next_id);
        self.next_id = if self.next_id >= ClientOrderId::RESERVED - 1 {
            1
        } else {
            self.next_id + 1
        };
        id
    }
}

impl Placed {
    /// The same order with `remaining` left showing. Fired, if it was
    /// triggered: only an order that has fired can trade.
    const fn filled_to(self, remaining: Qty) -> Placed {
        Placed {
            remaining,
            trigger: None,
            ..self
        }
    }

    /// The same order, fired: no longer waiting on a trigger.
    const fn fired(self) -> Placed {
        Placed {
            trigger: None,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::execution::edge::{Ack, Reject, Retired};
    use crate::adapters::execution::exec_id::{ExecId, VenueId};
    use crate::adapters::execution::order::Liquidity;

    /// A test instrument: calls by strike, and a perpetual. Ordered as an
    /// options spec is — every call before the perp — so a stated walk is
    /// the one this module's callers see.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
    enum Inst {
        Call(Px),
        #[default]
        Perp,
    }

    type Oms = super::Oms<Inst>;
    type Desired = super::Desired<Inst>;
    type Slot = super::Slot<Inst>;
    type Request = crate::adapters::execution::edge::Request<Inst>;
    type Report = crate::adapters::execution::edge::Report<Inst>;
    type Amend = crate::adapters::execution::edge::Amend<Inst>;
    type Fill = crate::adapters::execution::order::Fill<Inst>;

    /// A venue stating 5 a second, burst 20, for both limits, spent at four
    /// fifths.
    const TIER: OrderRate = OrderRate::stated(
        crate::adapters::execution::rate_limit::Terms::per_second(5, 20),
        crate::adapters::execution::rate_limit::Terms::per_second(5, 20),
        0.8,
    );

    fn px(s: &str) -> Px {
        Px::parse(s).unwrap()
    }

    fn qty(s: &str) -> Qty {
        Qty::parse(s).unwrap()
    }

    fn at(s: &str, q: &str) -> Level {
        Level::new(px(s), qty(q))
    }

    /// The thresholds a test that is about something else runs on: an hour
    /// of staleness and no requote threshold, neither of which suppresses
    /// anything; a second between crosses, the strategy's sweep; and a
    /// named venue rate, since the module has no default to offer.
    fn config() -> Config {
        Config::unmetered()
            .with_rate(TIER)
            .with_max_desired_age(Duration::from_secs(3600))
    }

    /// `unmetered` is what its docs say, field by field, and each setter
    /// sets its one field and leaves the rest.
    #[test]
    fn config_unmetered_is_its_documented_defaults_and_setters_set_one_field() {
        let base = Config::unmetered();
        assert_eq!(
            base,
            Config {
                max_desired_age: Duration::from_secs(5),
                min_requote: Px::ZERO,
                retake: Duration::from_secs(1),
                rate: OrderRate::UNMETERED,
                passive: Passive::PostOnly,
                lifetime: Lifetime::GoodTillCancel,
                ratio: None,
            }
        );
        assert_eq!(base.passive, Passive::default());
        assert_eq!(base.lifetime, Lifetime::default());

        let ratio = Ratio {
            messages_per_fill: 3,
            free: 10,
        };
        assert_eq!(base.with_rate(TIER), Config { rate: TIER, ..base });
        assert_eq!(
            base.with_ratio(ratio),
            Config {
                ratio: Some(ratio),
                ..base
            }
        );
        assert_eq!(
            base.with_passive(Passive::Limit),
            Config {
                passive: Passive::Limit,
                ..base
            }
        );
        assert_eq!(
            base.with_lifetime(Lifetime::Day),
            Config {
                lifetime: Lifetime::Day,
                ..base
            }
        );
        assert_eq!(
            base.with_max_desired_age(Duration::MAX),
            Config {
                max_desired_age: Duration::MAX,
                ..base
            }
        );
        assert_eq!(
            base.with_retake(Duration::ZERO),
            Config {
                retake: Duration::ZERO,
                ..base
            }
        );
        assert_eq!(
            base.with_min_requote(px("0.5")),
            Config {
                min_requote: px("0.5"),
                ..base
            }
        );

        // The whole chain is `const`, so a venue's config can be a constant.
        const LIVE: Config = Config::unmetered()
            .with_rate(TIER)
            .with_passive(Passive::Limit)
            .with_lifetime(Lifetime::Day);
        assert_eq!((LIVE.rate, LIVE.passive), (TIER, Passive::Limit));
    }

    fn oms() -> Oms {
        Oms::new(config())
    }

    fn now(nanos: u64) -> NanoTime {
        NanoTime::from(nanos)
    }

    /// A contract to quote, and a second one so that "every instrument" means
    /// more than one.
    fn call(strike: &str) -> Inst {
        Inst::Call(px(strike))
    }

    /// The hedge leg: an instrument with at most one side live, which is the
    /// degenerate case the design says needs no special handling.
    fn perp() -> Inst {
        Inst::Perp
    }

    fn want(instrument: Inst, bid: Option<Level>, ask: Option<Level>) -> Desired {
        Desired::quote(instrument, now(1_000), bid.into(), ask.into())
    }

    /// The hedge leg wanting `level` taken on `side`, as of `as_of`.
    fn cross(side: Side, level: Level, as_of: u64) -> Desired {
        Desired::cross(perp(), now(as_of), side, level)
    }

    /// Each constructor builds the literal its docs describe: the levels on
    /// the sides it names, the intent it names, never reduce-only unless
    /// asked, and the `as_of` it was handed.
    #[test]
    fn each_desired_constructor_is_the_literal_its_docs_state() {
        let (bid, ask) = (at("99", "1"), at("101", "2"));
        let deep = ladder(Side::Bid, &[("99", "1"), ("98", "3")]);
        let as_of = now(7);
        let literal = |bids: Ladder, asks: Ladder, intent: Intent| Desired {
            instrument: perp(),
            bids,
            asks,
            as_of,
            intent,
            reduce_only: false,
        };
        let stop = Trigger::stop(crate::adapters::execution::order::Reference::Mark, px("95"));

        assert_eq!(
            Desired::quote(perp(), as_of, deep, Ladder::one(ask)),
            literal(deep, Ladder::one(ask), Intent::Rest)
        );
        assert_eq!(
            Desired::two_way(perp(), as_of, bid, ask),
            literal(Ladder::one(bid), Ladder::one(ask), Intent::Rest)
        );
        assert_eq!(
            Desired::rest(perp(), as_of, Side::Bid, deep),
            literal(deep, Ladder::none(), Intent::Rest)
        );
        assert_eq!(
            Desired::rest(perp(), as_of, Side::Ask, Ladder::one(ask)),
            literal(Ladder::none(), Ladder::one(ask), Intent::Rest)
        );
        assert_eq!(
            Desired::cross(perp(), as_of, Side::Bid, bid),
            literal(Ladder::one(bid), Ladder::none(), Intent::Cross)
        );
        assert_eq!(
            Desired::cross(perp(), as_of, Side::Ask, ask),
            literal(Ladder::none(), Ladder::one(ask), Intent::Cross)
        );
        assert_eq!(
            Desired::stop(perp(), as_of, Side::Ask, stop, bid),
            literal(Ladder::none(), Ladder::one(bid), Intent::Trigger(stop))
        );
        assert_eq!(
            Desired::stop(perp(), as_of, Side::Bid, stop, ask),
            literal(Ladder::one(ask), Ladder::none(), Intent::Trigger(stop))
        );
        assert_eq!(
            Desired::nothing(perp(), as_of),
            literal(Ladder::none(), Ladder::none(), Intent::Rest)
        );
        assert_eq!(
            Desired::cross(perp(), as_of, Side::Ask, ask).reduce_only(),
            Desired {
                reduce_only: true,
                ..literal(Ladder::none(), Ladder::one(ask), Intent::Cross)
            }
        );
        // `reduce_only` changes that field and nothing else, and is idempotent.
        let quoted = Desired::two_way(perp(), as_of, bid, ask);
        assert_eq!(
            quoted.reduce_only().reduce_only(),
            Desired {
                reduce_only: true,
                ..quoted
            }
        );
    }

    /// The constructors are `const`: a fixed decision can be a constant.
    #[test]
    fn a_desired_constructor_is_usable_in_a_const() {
        const PULL: Desired =
            Desired::rest(Inst::Perp, NanoTime::ZERO, Side::Bid, Ladder::none()).reduce_only();
        assert_eq!(
            PULL,
            Desired {
                reduce_only: true,
                ..Desired::nothing(perp(), NanoTime::ZERO)
            }
        );
    }

    fn ack(id: ClientOrderId) -> Report {
        Report::Ack(Ack {
            order: id,
            venue_id: VenueId::new("v1").unwrap(),
            price: None,
            remaining: None,
            venue_time: None,
            recv_time: now(1_001),
        })
    }

    /// The `Ack` inside a report built by [`ack`], for struct update.
    trait IntoAck {
        fn into_ack(self) -> Ack;
    }

    impl IntoAck for Report {
        fn into_ack(self) -> Ack {
            match self {
                Report::Ack(ack) => ack,
                other => panic!("{other:?}"),
            }
        }
    }

    fn reject(id: ClientOrderId, reason: RejectReason) -> Report {
        Report::Reject(Reject {
            order: Some(id),
            reason,
            venue_time: None,
            recv_time: now(1_001),
        })
    }

    fn fill(id: ClientOrderId, filled: &str, remaining: &str) -> Report {
        Report::Fill(Fill {
            order: id,
            exec_id: ExecId::new("e1").unwrap(),
            qty: qty(filled),
            remaining: qty(remaining),
            price: px("0.05"),
            liquidity: Liquidity::Maker,
            recv_time: now(1_002),
            ..Default::default()
        })
    }

    fn cancelled(id: ClientOrderId) -> Report {
        Report::Cancelled(Retired {
            order: id,
            remaining: Qty::ZERO,
            venue_time: None,
            recv_time: now(1_002),
        })
    }

    /// An OMS with one working bid on `call("60000")` at 0.05 × 10, and the
    /// id it is working under — the starting point most cases below share.
    fn with_a_working_bid() -> (Oms, ClientOrderId) {
        let mut oms = oms();
        let requests = oms.diff(
            now(1_000),
            &[want(call("60000"), Some(at("0.05", "10")), None)],
        );
        let id = match requests.as_slice() {
            [Request::Place(order)] => order.id,
            other => panic!("expected one place, got {other:?}"),
        };
        oms.apply(&[ack(id)]);
        assert!(matches!(
            oms.slot(&call("60000"), Side::Bid),
            Slot::Working(_)
        ));
        (oms, id)
    }

    /// A desired level with nothing working against it is a place, post-only
    /// and at the level asked for, and the slot is then in flight.
    #[test]
    fn a_wanted_level_with_nothing_working_is_placed() {
        let mut oms = oms();
        let requests = oms.diff(
            now(1_000),
            &[want(
                call("60000"),
                Some(at("0.05", "10")),
                Some(at("0.06", "10")),
            )],
        );
        assert_eq!(requests.len(), 2, "a two-way is two orders");
        for (request, side) in requests.iter().zip([Side::Bid, Side::Ask]) {
            let Request::Place(order) = request else {
                panic!("expected a place, got {request:?}")
            };
            order
                .validate()
                .expect("the OMS does not send what a venue refuses");
            assert_eq!(order.side, side, "bid before ask, deterministically");
            assert_eq!(
                order.kind,
                crate::adapters::execution::order::OrderKind::PostOnly
            );
            assert_eq!(order.created, now(1_000), "engine time, not the wall clock");
        }
        assert!(oms.slot(&call("60000"), Side::Bid).in_flight());
    }

    /// The second invariant. Re-deciding the same level every tick is what
    /// the strategy does; emitting it again is what the OMS must not do.
    #[test]
    fn a_desired_that_matches_what_is_working_emits_nothing() {
        let (mut oms, _) = with_a_working_bid();
        let again = oms.diff(
            now(1_100),
            &[want(call("60000"), Some(at("0.05", "10")), None)],
        );
        assert!(again.is_empty(), "{again:?}");
    }

    /// The first invariant, and the half of it that matters: the OMS does not
    /// spray amends while one is outstanding, and it emits the right thing
    /// once the report lands.
    #[test]
    fn a_slot_in_flight_emits_nothing_until_its_report_lands() {
        let (mut oms, id) = with_a_working_bid();

        let amended = oms.diff(
            now(1_100),
            &[want(call("60000"), Some(at("0.04", "10")), None)],
        );
        assert_eq!(
            amended.as_slice(),
            [Request::Amend(Amend {
                order: id,
                instrument: call("60000"),
                price: px("0.04"),
                qty: qty("10"),
                trigger: None,
            })]
        );

        // The touch keeps moving, three ticks running. The OMS says nothing.
        for (tick, price) in [(1_200, "0.03"), (1_300, "0.02"), (1_400, "0.01")] {
            let quiet = oms.diff(
                now(tick),
                &[want(call("60000"), Some(at(price, "10")), None)],
            );
            assert!(quiet.is_empty(), "amended again at {price}: {quiet:?}");
        }

        // The ack lands, and the next tick asks for the level the strategy
        // wants *now* — not the one it wanted when the amend went out.
        oms.apply(&[ack(id)]);
        let Slot::Working(resting) = oms.slot(&call("60000"), Side::Bid) else {
            panic!("the ack makes it working again")
        };
        assert_eq!((resting.price, resting.remaining), (px("0.04"), qty("10")));
        let caught_up = oms.diff(
            now(1_500),
            &[want(call("60000"), Some(at("0.01", "10")), None)],
        );
        assert_eq!(
            caught_up.as_slice(),
            [Request::Amend(Amend {
                order: id,
                instrument: call("60000"),
                price: px("0.01"),
                qty: qty("10"),
                trigger: None,
            })]
        );
    }

    /// Ordinary, not an error: the touch moved between the decision and the
    /// arrival, and the venue refused to let a post-only order cross.
    ///
    /// The decision that follows carries its own `as_of`, which is what
    /// makes it a decision rather than the same one sent twice — see
    /// `a_refused_place_waits_for_a_fresh_decision` below.
    #[test]
    fn a_post_only_reject_returns_the_slot_to_idle_and_the_next_tick_re_decides() {
        let mut oms = oms();
        let requests = oms.diff(
            now(1_000),
            &[want(call("60000"), Some(at("0.05", "10")), None)],
        );
        let Some(Request::Place(order)) = requests.first() else {
            panic!("expected a place")
        };

        oms.apply(&[reject(order.id, RejectReason::PostOnlyWouldCross)]);
        assert_eq!(oms.slot(&call("60000"), Side::Bid), Slot::Idle);

        let mut decided = want(call("60000"), Some(at("0.04", "10")), None);
        decided.as_of = now(1_100);
        let again = oms.diff(now(1_100), &[decided]);
        let [Request::Place(retry)] = again.as_slice() else {
            panic!("the next tick re-decides: {again:?}")
        };
        assert_eq!(retry.price, Some(px("0.04")));
        assert_ne!(retry.id, order.id, "a new order is a new id");
    }

    /// A rejected *amend* is not a rejected order: what was resting is still
    /// resting, and pretending otherwise would have the OMS place a second
    /// order against the first.
    #[test]
    fn a_rejected_amend_leaves_the_order_where_it_was() {
        let (mut oms, id) = with_a_working_bid();
        oms.diff(
            now(1_100),
            &[want(call("60000"), Some(at("0.04", "10")), None)],
        );

        oms.apply(&[reject(id, RejectReason::PostOnlyWouldCross)]);
        let Slot::Working(resting) = oms.slot(&call("60000"), Side::Bid) else {
            panic!("still resting at the old level")
        };
        assert_eq!(resting.price, px("0.05"));
        assert_eq!(oms.rejected(), 1);

        // The same decision is not asked again: the venue has answered it,
        // and re-sending the amend on every diff would be the loop a
        // refused place is kept out of. The order is left where it rests.
        for tick in [1_101, 1_200, 2_000] {
            let again = oms.diff(now(tick), &[]);
            assert!(again.is_empty(), "at {tick}: {again:?}");
        }
        let again = oms.diff(
            now(1_300),
            &[want(call("60000"), Some(at("0.04", "10")), None)],
        );
        assert!(again.is_empty(), "the decision restated: {again:?}");

        // A fresh decision asks afresh.
        let mut fresh = want(call("60000"), Some(at("0.04", "10")), None);
        fresh.as_of = now(1_400);
        let amended = oms.diff(now(1_400), &[fresh]);
        assert!(
            matches!(
                amended.as_slice(),
                [Request::Amend(amend)] if amend.order == id && amend.price == px("0.04")
            ),
            "{amended:?}"
        );
    }

    /// A cancel the venue refuses for its rate leaves the order working, and
    /// the next diff amends it — after a cancel. That is the sequence a
    /// per-order ceiling judging from a table of placed ids aborted on, and
    /// the reason an amend carries its own contract.
    #[test]
    fn a_rate_limited_cancel_leaves_the_order_working_and_the_next_diff_amends_it() {
        let (mut oms, id) = with_a_working_bid();
        let pulled = oms.diff(now(1_100), &[want(call("60000"), None, None)]);
        assert_eq!(pulled.as_slice(), [Request::Cancel(id)]);
        oms.apply(&[reject(id, RejectReason::RateLimited)]);
        assert!(matches!(
            oms.slot(&call("60000"), Side::Bid),
            Slot::Working(_)
        ));
        let mut again = want(call("60000"), Some(at("0.04", "10")), None);
        again.as_of = now(1_200);
        let amended = oms.diff(now(1_200), &[again]);
        assert!(
            matches!(
                amended.as_slice(),
                [Request::Amend(amend)] if amend.order == id && amend.instrument == call("60000")
            ),
            "{amended:?}"
        );
    }

    /// The one reject reason the state machine reads, because it is the one
    /// that says the *order* is gone rather than that the request was.
    #[test]
    fn an_unknown_order_reject_retires_the_slot() {
        let (mut oms, id) = with_a_working_bid();
        oms.diff(
            now(1_100),
            &[want(call("60000"), Some(at("0.04", "10")), None)],
        );
        oms.apply(&[reject(id, RejectReason::UnknownOrder)]);
        assert_eq!(oms.slot(&call("60000"), Side::Bid), Slot::Idle);
    }

    /// A partial leaves the order working and short; a fill that takes what is
    /// left to zero retires the slot.
    #[test]
    fn a_partial_fill_does_not_retire_the_slot_and_a_full_one_does() {
        let (mut oms, id) = with_a_working_bid();

        oms.apply(&[fill(id, "3", "7")]);
        let Slot::Working(resting) = oms.slot(&call("60000"), Side::Bid) else {
            panic!("a partial leaves it working")
        };
        assert_eq!((resting.qty, resting.remaining), (qty("10"), qty("7")));

        oms.apply(&[fill(id, "7", "0")]);
        assert_eq!(oms.slot(&call("60000"), Side::Bid), Slot::Idle);
    }

    /// The comparison is against what is still *showing*, so a quote that has
    /// been partially filled is re-shown in full rather than left short.
    #[test]
    fn a_partially_filled_quote_is_amended_back_up() {
        let (mut oms, id) = with_a_working_bid();
        oms.apply(&[fill(id, "3", "7")]);
        let requests = oms.diff(
            now(1_100),
            &[want(call("60000"), Some(at("0.05", "10")), None)],
        );
        assert_eq!(
            requests.as_slice(),
            [Request::Amend(Amend {
                order: id,
                instrument: call("60000"),
                price: px("0.05"),
                qty: qty("10"),
                trigger: None,
            })],
            "the price is unchanged; the size is not"
        );
    }

    /// A contract the strategy has stopped wanting anything on is withdrawn
    /// at once — an empty desired is a decision, not an absence.
    #[test]
    fn an_empty_desired_cancels_what_is_working() {
        let (mut oms, id) = with_a_working_bid();
        let requests = oms.diff(now(1_100), &[Desired::nothing(call("60000"), now(1_100))]);
        assert_eq!(requests.as_slice(), [Request::Cancel(id)]);
        assert!(oms.slot(&call("60000"), Side::Bid).in_flight());

        oms.apply(&[cancelled(id)]);
        assert_eq!(oms.slot(&call("60000"), Side::Bid), Slot::Idle);
    }

    /// Absence cannot mean withdrawal — the quoter's contracts and the
    /// hedger's leg are separate decisions on separate edges, and a burst
    /// carrying one must not cancel the other.
    #[test]
    fn a_burst_that_does_not_mention_an_instrument_leaves_it_alone() {
        let mut oms = oms();
        let placed = oms.diff(
            now(1_000),
            &[
                want(call("60000"), Some(at("0.05", "10")), None),
                want(perp(), None, Some(at("61000", "1"))),
            ],
        );
        assert_eq!(placed.len(), 2);
        for request in &placed {
            let Request::Place(order) = request else {
                panic!("expected places")
            };
            oms.apply(&[ack(order.id)]);
        }

        // The hedger decides again; the quoter has not.
        let hedge_only = oms.diff(now(1_100), &[want(perp(), None, Some(at("61000", "1")))]);
        assert!(hedge_only.is_empty(), "{hedge_only:?}");
        assert!(
            matches!(oms.slot(&call("60000"), Side::Bid), Slot::Working(_)),
            "the option quote is untouched by a hedge decision"
        );
    }

    /// What ends a remembered desired instead: it goes stale, judged at the
    /// reader's own engine time. A graph that has stopped deciding must not
    /// leave orders resting on a strategy that is no longer thinking.
    #[test]
    fn a_desired_that_has_gone_stale_is_cancelled() {
        let mut oms = Oms::new(Config {
            max_desired_age: Duration::from_secs(1),
            ..config()
        });
        let placed = oms.diff(
            now(1_000),
            &[want(call("60000"), Some(at("0.05", "10")), None)],
        );
        let Some(Request::Place(order)) = placed.first() else {
            panic!("expected a place")
        };
        oms.apply(&[ack(order.id)]);

        // The desired was decided at 1_000ns and is believed for a second, so
        // it is still good at 1_000_001_000 and not at the nanosecond after.
        let quiet = oms.diff(now(1_000_001_000), &[]);
        assert!(quiet.is_empty(), "{quiet:?}");

        // Past it: withdrawn, with no help from upstream.
        let withdrawn = oms.diff(now(1_000_001_001), &[]);
        assert_eq!(withdrawn.as_slice(), [Request::Cancel(order.id)]);
    }

    /// `Duration::MAX` is "never stale", and means it: the age is judged as
    /// time elapsed, so it does not overflow `as_of + age` into a wrap that
    /// would read every desired as stale and cancel the book.
    #[test]
    fn a_max_desired_age_of_duration_max_is_never_stale() {
        let mut oms = Oms::new(Config {
            max_desired_age: Duration::MAX,
            ..config()
        });
        let placed = oms.diff(
            now(1_000),
            &[want(call("60000"), Some(at("0.05", "10")), None)],
        );
        let Some(Request::Place(order)) = placed.first() else {
            panic!("expected a place")
        };
        oms.apply(&[ack(order.id)]);

        let quiet = oms.diff(NanoTime::MAX, &[]);
        assert!(quiet.is_empty(), "{quiet:?}");
        assert!(matches!(
            oms.slot(&call("60000"), Side::Bid),
            Slot::Working(_)
        ));
    }

    /// Options expire and contracts fall out of the selection, so an entry
    /// with nothing working and nothing wanted is dropped rather than held
    /// for the life of the process.
    #[test]
    fn a_quiet_instrument_is_forgotten() {
        let (mut oms, id) = with_a_working_bid();
        oms.diff(now(1_100), &[Desired::nothing(call("60000"), now(1_100))]);
        oms.apply(&[cancelled(id)]);
        oms.diff(now(1_200), &[Desired::nothing(call("60000"), now(1_200))]);
        assert!(
            oms.entries.is_empty(),
            "an idle slot with nothing wanted holds no state"
        );
    }

    /// A desired that may only reduce goes out as a reduce-only order,
    /// shown or taken; an ordinary one does not.
    #[test]
    fn a_reduce_only_desired_places_a_reduce_only_order() {
        let mut oms = oms();
        let shown = oms.diff(
            now(1_000),
            &[
                Desired {
                    reduce_only: true,
                    ..want(call("60000"), Some(at("0.05", "10")), None)
                },
                Desired {
                    reduce_only: true,
                    ..cross(Side::Ask, at("60000", "100"), 1_000)
                },
                want(call("61000"), Some(at("0.05", "10")), None),
            ],
        );
        let flags: Vec<(Inst, bool)> = shown
            .iter()
            .map(|request| match request {
                Request::Place(order) => (order.instrument, order.reduce_only),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            flags,
            [
                (call("60000"), true),
                (call("61000"), false),
                (perp(), true)
            ]
        );
    }

    /// Two decisions for one instrument in one burst are merged before
    /// anything is emitted, so they cannot become two requests — and the
    /// later one wins whatever order they arrived in.
    #[test]
    fn two_desireds_for_one_instrument_in_one_burst_are_one_request() {
        let mut oms = oms();
        let requests = oms.diff(
            now(1_000),
            &[
                Desired {
                    as_of: now(1_000),
                    ..want(call("60000"), Some(at("0.05", "10")), None)
                },
                Desired {
                    as_of: now(1_001),
                    ..want(call("60000"), Some(at("0.04", "10")), None)
                },
            ],
        );
        let [Request::Place(order)] = requests.as_slice() else {
            panic!("expected exactly one place: {requests:?}")
        };
        assert_eq!(order.price, Some(px("0.04")), "the later decision wins");
    }

    /// Out of order in the burst, the later decision still wins: the merge is
    /// on `as_of`, not on arrival.
    #[test]
    fn the_later_decision_wins_whatever_order_it_arrives_in() {
        let mut oms = oms();
        let requests = oms.diff(
            now(1_000),
            &[
                Desired {
                    as_of: now(1_001),
                    ..want(call("60000"), Some(at("0.04", "10")), None)
                },
                Desired {
                    as_of: now(1_000),
                    ..want(call("60000"), Some(at("0.05", "10")), None)
                },
            ],
        );
        let [Request::Place(order)] = requests.as_slice() else {
            panic!("expected exactly one place: {requests:?}")
        };
        assert_eq!(order.price, Some(px("0.04")));
    }

    /// The first move of a kill switch. Nothing is wanted
    /// afterwards, so nothing is re-placed until the strategy decides again.
    #[test]
    fn cancel_all_empties_every_slot_and_wants_nothing_after() {
        let mut oms = oms();
        let placed = oms.diff(
            now(1_000),
            &[
                want(
                    call("60000"),
                    Some(at("0.05", "10")),
                    Some(at("0.06", "10")),
                ),
                want(perp(), None, Some(at("61000", "1"))),
            ],
        );
        let ids: Vec<ClientOrderId> = placed
            .iter()
            .map(|request| request.order().expect("a place names its order"))
            .collect();
        assert_eq!(ids.len(), 3);
        for id in &ids {
            oms.apply(&[ack(*id)]);
        }

        assert_eq!(oms.cancel_all(now(1_050)), Some(Request::CancelAll));
        for instrument in [call("60000"), perp()] {
            for side in [Side::Bid, Side::Ask] {
                let slot = oms.slot(&instrument, side);
                assert!(
                    matches!(slot, Slot::Idle | Slot::PendingCancel(_)),
                    "{instrument:?} {side:?} is {slot:?}"
                );
            }
        }

        // Nothing is wanted, so the pulled orders are not immediately
        // re-placed — and the diff emits no second cancel for a slot that
        // already has one in flight.
        let after = oms.diff(now(1_100), &[]);
        assert!(after.is_empty(), "{after:?}");

        for id in &ids {
            oms.apply(&[cancelled(*id)]);
        }
        let settled = oms.diff(now(1_200), &[]);
        assert!(settled.is_empty(), "{settled:?}");
    }

    /// A place whose ack lands after a cancel-all is a working order nothing
    /// wants, and the next diff pulls it. That is why `cancel_all` leaves an
    /// in-flight slot alone rather than making an exception to the one
    /// invariant that matters.
    #[test]
    fn cancel_all_catches_an_order_that_was_still_in_flight() {
        let mut oms = oms();
        let placed = oms.diff(
            now(1_000),
            &[want(call("60000"), Some(at("0.05", "10")), None)],
        );
        let Some(Request::Place(order)) = placed.first() else {
            panic!("expected a place")
        };

        assert_eq!(oms.cancel_all(now(1_050)), Some(Request::CancelAll));
        assert!(matches!(
            oms.slot(&call("60000"), Side::Bid),
            Slot::Pending(_)
        ));

        oms.apply(&[ack(order.id)]);
        let sweep = oms.diff(now(1_100), &[]);
        assert_eq!(sweep.as_slice(), [Request::Cancel(order.id)]);
    }

    /// A cancel-all the venue refuses names no order, and what it moved to
    /// pending is still resting: those slots return to working, the next
    /// diff pulls each of them singly, and a slot whose own place was still
    /// in flight is left to its own reply.
    #[test]
    fn a_refused_cancel_all_returns_its_slots_and_the_next_diff_pulls_them_singly() {
        let mut oms = oms();
        let placed = oms.diff(
            now(1_000),
            &[
                want(
                    call("60000"),
                    Some(at("0.05", "10")),
                    Some(at("0.06", "10")),
                ),
                want(perp(), None, Some(at("61000", "1"))),
            ],
        );
        let ids: Vec<ClientOrderId> = placed
            .iter()
            .map(|request| request.order().expect("a place names its order"))
            .collect();
        // Two of the three are acknowledged; the perp's place is in flight.
        oms.apply(&[ack(ids[0]), ack(ids[1])]);
        assert_eq!(oms.cancel_all(now(1_050)), Some(Request::CancelAll));
        assert!(matches!(
            oms.slot(&call("60000"), Side::Bid),
            Slot::PendingCancel(_)
        ));

        oms.apply(&[Report::Reject(Reject {
            order: None,
            reason: RejectReason::RateLimited,
            venue_time: None,
            recv_time: now(1_001),
        })]);
        assert_eq!(oms.cancel_all_refused(), 1);
        assert_eq!(
            oms.unrouted(),
            0,
            "the refusal was routed to the cancel-all"
        );
        assert!(matches!(
            oms.slot(&call("60000"), Side::Bid),
            Slot::Working(_)
        ));
        assert!(matches!(
            oms.slot(&call("60000"), Side::Ask),
            Slot::Working(_)
        ));
        assert!(
            matches!(oms.slot(&perp(), Side::Ask), Slot::Pending(_)),
            "a place in flight is not the cancel-all's to answer for"
        );

        // Nothing is wanted, so the next diff pulls what is resting.
        let sweep = oms.diff(now(1_100), &[]);
        assert_eq!(
            sweep.as_slice(),
            [Request::Cancel(ids[0]), Request::Cancel(ids[1])]
        );
        // And the venue's own answer still retires a slot: the cancel-all
        // it refused as a whole may have pulled one before refusing.
        oms.apply(&[Report::Cancelled(Retired {
            order: ids[0],
            ..Retired::default()
        })]);
        assert_eq!(oms.slot(&call("60000"), Side::Bid), Slot::Idle);
    }

    /// An unattributed reject answers the cancel-all only while one is in
    /// flight. With none — a session-level refusal a venue could not pin on
    /// an order — it is counted and changes nothing: the single cancel in
    /// flight stays in flight, rather than being returned to working and
    /// pulled a second time on the next diff.
    #[test]
    fn an_unattributed_reject_with_no_cancel_all_in_flight_is_unrouted() {
        let (mut oms, id) = with_a_working_bid();
        let pulled = oms.diff(now(1_100), &[want(call("60000"), None, None)]);
        assert_eq!(pulled.as_slice(), [Request::Cancel(id)]);
        assert!(matches!(
            oms.slot(&call("60000"), Side::Bid),
            Slot::PendingCancel(_)
        ));

        oms.apply(&[Report::Reject(Reject {
            order: None,
            reason: RejectReason::RateLimited,
            venue_time: None,
            recv_time: now(1_101),
        })]);
        assert_eq!(oms.cancel_all_refused(), 0, "no cancel-all was sent");
        assert_eq!(oms.unrouted(), 1);
        assert!(
            matches!(oms.slot(&call("60000"), Side::Bid), Slot::PendingCancel(_)),
            "the single cancel is still in flight"
        );
        assert!(oms.diff(now(1_200), &[]).is_empty(), "nothing is re-sent");

        // A cancel-all's refusal is answered once: a second unattributed
        // reject after it is not a second refusal.
        oms.apply(&[Report::Cancelled(Retired {
            order: id,
            recv_time: now(1_201),
            ..Retired::default()
        })]);
        let placed = oms.diff(
            now(1_300),
            &[want(call("60000"), Some(at("0.05", "10")), None)],
        );
        let id = placed[0].order().unwrap();
        oms.apply(&[ack(id)]);
        assert_eq!(oms.cancel_all(now(1_400)), Some(Request::CancelAll));
        let refusal = Report::Reject(Reject {
            order: None,
            reason: RejectReason::Other,
            venue_time: None,
            recv_time: now(1_401),
        });
        oms.apply(&[refusal, refusal]);
        assert_eq!(oms.cancel_all_refused(), 1);
        assert_eq!(oms.unrouted(), 2);
    }

    /// A cancel-all whose cancels all came back has been answered: a session
    /// reject much later is not its refusal, and the single cancel then in
    /// flight stays in flight rather than being returned and sent again.
    #[test]
    fn a_cancel_all_that_succeeded_is_not_refused_by_a_later_reject() {
        let (mut oms, id) = with_a_working_bid();
        assert_eq!(oms.cancel_all(now(1_050)), Some(Request::CancelAll));
        oms.apply(&[cancelled(id)]);

        let mut decided = want(call("60000"), Some(at("0.05", "10")), None);
        decided.as_of = now(2_000);
        let placed = oms.diff(now(2_000), &[decided]);
        let id = placed[0].order().unwrap();
        oms.apply(&[ack(id)]);
        let mut withdrawn = want(call("60000"), None, None);
        withdrawn.as_of = now(2_100);
        assert_eq!(
            oms.diff(now(2_100), &[withdrawn]).as_slice(),
            [Request::Cancel(id)]
        );

        oms.apply(&[Report::Reject(Reject {
            order: None,
            reason: RejectReason::Other,
            venue_time: None,
            recv_time: now(2_101),
        })]);
        assert_eq!((oms.cancel_all_refused(), oms.unrouted()), (0, 1));
        assert!(matches!(
            oms.slot(&call("60000"), Side::Bid),
            Slot::PendingCancel(_)
        ));
    }

    /// A cancel-all pulls the decisions standing at that time. One decided
    /// before it and restated after — an algo restating its kept `as_of` —
    /// is not taken up again, even once the entry it lived on is gone.
    #[test]
    fn a_decision_older_than_a_cancel_all_does_not_re_place() {
        let (mut oms, id) = with_a_working_bid();
        assert_eq!(oms.cancel_all(now(1_500)), Some(Request::CancelAll));
        oms.apply(&[cancelled(id)]);
        assert!(oms.diff(now(1_600), &[]).is_empty());

        let stale = want(call("60000"), Some(at("0.05", "10")), None);
        assert!(
            oms.diff(now(1_700), &[stale]).is_empty(),
            "decided at 1_000, before the cancel-all"
        );

        let mut fresh = stale;
        fresh.as_of = now(1_700);
        assert!(matches!(
            oms.diff(now(1_700), &[fresh]).as_slice(),
            [Request::Place(_)]
        ));
    }

    /// A refused level is remembered while its decision is fresh, even when
    /// nothing else is wanted or working on the instrument — otherwise the
    /// entry is dropped with the record, and the same decision restated is
    /// sent again.
    #[test]
    fn a_refused_lone_level_is_not_sent_again_after_the_entry_goes_quiet() {
        let mut oms = oms();
        let decided = want(call("60000"), Some(at("0.05", "10")), None);
        let placed = oms.diff(now(1_000), &[decided]);
        let id = placed[0].order().unwrap();
        oms.apply(&[reject(id, RejectReason::PostOnlyWouldCross)]);
        assert!(oms.diff(now(1_001), &[]).is_empty());
        assert!(
            oms.diff(now(1_002), &[decided]).is_empty(),
            "the same decision, restated"
        );
    }

    /// A venue that rounds the price to its tick or trims the size to its
    /// lot accepts something other than what was asked. The slot works what
    /// the venue says it rests, and the decision counts as answered — the
    /// next diff neither asks the venue to round it again nor pulls it —
    /// until the strategy decides afresh, which does.
    #[test]
    fn an_ack_that_accepted_something_else_is_what_rests_and_spends_the_decision() {
        let mut oms = oms();
        let placed = oms.diff(
            now(1_000),
            &[want(call("60000"), Some(at("0.0503", "10")), None)],
        );
        let id = placed[0].order().unwrap();
        // Rounded to a tick of 0.0005 and trimmed to a lot of 5.
        oms.apply(&[Report::Ack(Ack {
            order: id,
            price: Some(px("0.05")),
            remaining: Some(qty("5")),
            ..ack(id).into_ack()
        })]);
        assert_eq!(
            oms.slot(&call("60000"), Side::Bid),
            Slot::Working(Placed {
                id,
                price: px("0.05"),
                qty: qty("10"),
                remaining: qty("5"),
                as_of: now(1_000),
                trigger: None,
            })
        );
        assert_eq!(oms.adjusted(), 1);

        // The same decision again asks nothing: the venue has answered it.
        let again = oms.diff(
            now(1_100),
            &[want(call("60000"), Some(at("0.0503", "10")), None)],
        );
        assert!(again.is_empty(), "{again:?}");

        // A fresh decision asks, and is amended to.
        let mut fresh = want(call("60000"), Some(at("0.0503", "10")), None);
        fresh.as_of = now(1_200);
        let requests = oms.diff(now(1_200), &[fresh]);
        let [Request::Amend(amend)] = requests.as_slice() else {
            panic!("{requests:?}")
        };
        assert_eq!((amend.price, amend.qty), (px("0.0503"), qty("10")));
        // The amend comes back at the venue's tick; an ack that names
        // nothing leaves what was asked.
        oms.apply(&[Report::Ack(Ack {
            order: id,
            price: Some(px("0.05")),
            remaining: None,
            ..ack(id).into_ack()
        })]);
        assert_eq!(
            oms.slot(&call("60000"), Side::Bid)
                .placed()
                .map(|p| (p.price, p.remaining)),
            Some((px("0.05"), qty("10")))
        );
        assert_eq!(oms.adjusted(), 2);
    }

    /// The knob that defaults to zero until measurement settles it: below
    /// it a price move is not worth a message, and a size change is worth one
    /// regardless, because a size that is wrong is wrong by the whole
    /// difference.
    #[test]
    fn a_move_smaller_than_min_requote_is_not_worth_a_message() {
        let mut oms = Oms::new(Config {
            min_requote: px("0.001"),
            ..config()
        });
        let placed = oms.diff(
            now(1_000),
            &[want(call("60000"), Some(at("0.05", "10")), None)],
        );
        let Some(Request::Place(order)) = placed.first() else {
            panic!("expected a place")
        };
        oms.apply(&[ack(order.id)]);

        let inside = oms.diff(
            now(1_100),
            &[want(call("60000"), Some(at("0.0505", "10")), None)],
        );
        assert!(inside.is_empty(), "{inside:?}");

        let resized = oms.diff(
            now(1_200),
            &[want(call("60000"), Some(at("0.0505", "12")), None)],
        );
        assert_eq!(resized.len(), 1, "a size change is worth a message");
        oms.apply(&[ack(order.id)]);

        let outside = oms.diff(
            now(1_300),
            &[want(call("60000"), Some(at("0.06", "12")), None)],
        );
        assert_eq!(outside.len(), 1, "{outside:?}");
    }

    /// A level a venue would refuse never reaches one: the side is withdrawn,
    /// which is always safe, and the count is what says the strategy upstream
    /// is producing them.
    #[test]
    fn a_level_with_no_size_is_refused_rather_than_sent() {
        let mut oms = oms();
        let requests = oms.diff(
            now(1_000),
            &[want(call("60000"), Some(at("0.05", "0")), None)],
        );
        assert!(requests.is_empty(), "{requests:?}");
        assert_eq!(oms.refused(), 1);
    }

    /// A report for an order no slot holds is counted, not guessed at. In
    /// small numbers it is a cancel that crossed with a fill; in large ones it
    /// is what 18b exists to resolve.
    #[test]
    fn a_report_for_an_order_nobody_holds_is_counted() {
        let (mut oms, id) = with_a_working_bid();
        oms.apply(&[ack(ClientOrderId(9_999))]);
        assert_eq!(oms.unrouted(), 1);

        // An ack for an order already working is a report its state cannot
        // produce, and is counted the same way rather than acted on.
        oms.apply(&[ack(id)]);
        assert_eq!(oms.unrouted(), 2);
        assert!(matches!(
            oms.slot(&call("60000"), Side::Bid),
            Slot::Working(_)
        ));

        // A refusal the venue could not attribute is the cancel-all's only
        // while one is in flight; with none, it is a report about nothing
        // that was sent, counted like the others and moving nothing.
        oms.apply(&[Report::default()]);
        assert_eq!((oms.unrouted(), oms.cancel_all_refused()), (3, 0));
        assert!(matches!(
            oms.slot(&call("60000"), Side::Bid),
            Slot::Working(_)
        ));
    }

    /// The emitted burst has to be the same burst on a replay as on the run
    /// it replays — so the walk is over an ordered map, and instruments come
    /// out in the instrument's order rather than the order the strategy
    /// happened to decide them in.
    #[test]
    fn the_requests_come_out_in_a_deterministic_order() {
        let instruments = [call("70000"), call("60000"), perp(), call("65000")];
        let emitted = |seed: usize| -> Vec<Inst> {
            let mut oms = oms();
            // Rotate the input so that insertion order differs between runs.
            let mut desired: Vec<Desired> = instruments
                .iter()
                .map(|instrument| want(*instrument, Some(at("0.05", "10")), None))
                .collect();
            desired.rotate_left(seed);
            oms.diff(now(1_000), &desired)
                .iter()
                .map(|request| match request {
                    Request::Place(order) => order.instrument,
                    other => panic!("expected places, got {other:?}"),
                })
                .collect()
        };

        let first = emitted(0);
        assert_eq!(first.len(), instruments.len());
        for seed in 1..instruments.len() {
            assert_eq!(emitted(seed), first, "insertion order must not show");
        }
        let mut sorted = instruments.to_vec();
        sorted.sort_unstable();
        assert_eq!(first, sorted);
    }

    /// The hedge leg is an instrument with at most one side live, and the
    /// design says that needs no special handling. This is that claim.
    #[test]
    fn the_hedge_leg_is_just_an_instrument_with_one_side() {
        let mut oms = oms();
        let placed = oms.diff(now(1_000), &[want(perp(), None, Some(at("61000", "1")))]);
        let [Request::Place(order)] = placed.as_slice() else {
            panic!("expected one place: {placed:?}")
        };
        assert_eq!(order.side, Side::Ask);
        assert_eq!(oms.slot(&perp(), Side::Bid), Slot::Idle);

        // The delta changes sides: the old side is pulled, and the new one is
        // not placed until it is, because a slot in flight emits nothing and
        // the two sides are independent slots.
        oms.apply(&[ack(order.id)]);
        let flipped = oms.diff(now(1_100), &[want(perp(), Some(at("60900", "1")), None)]);
        let [Request::Place(bid), Request::Cancel(pulled)] = flipped.as_slice() else {
            panic!("the bid is placed and the ask pulled: {flipped:?}")
        };
        assert_eq!(bid.side, Side::Bid);
        assert_ne!(bid.id, order.id, "the other side is a different order");
        assert_eq!(*pulled, order.id);
    }
    /// A place the venue refused is not sent again until the strategy has
    /// decided afresh.
    ///
    /// Without this the fold re-places the identical level the instant the
    /// rejection lands — the same question of the same book, with the same
    /// answer — and, wired into a graph where a report wakes it, that is a
    /// loop bounded only by `max_desired_age`. "The next tick
    /// re-decides" means the *strategy* re-decides, and this is what makes it
    /// true.
    #[test]
    fn a_refused_place_waits_for_a_fresh_decision() {
        let mut oms = oms();
        let requests = oms.diff(
            now(1_000),
            &[want(
                call("60000"),
                Some(at("0.04", "10")),
                Some(at("0.06", "10")),
            )],
        );
        let bid = requests
            .iter()
            .find_map(|request| match request {
                Request::Place(order) if order.side == Side::Bid => Some(*order),
                _ => None,
            })
            .expect("a bid was placed");

        // The venue refuses it: post-only, and the touch moved underneath.
        oms.apply(&[reject(bid.id, RejectReason::PostOnlyWouldCross)]);
        assert_eq!(oms.rejected(), 1, "and it is counted");
        assert_eq!(
            oms.slot(&call("60000"), Side::Bid),
            Slot::Idle,
            "the slot is idle, as the state machine says",
        );

        // Every diff before a fresh decision leaves it alone, however often
        // it is asked — which is the property that stops the loop.
        for tick in 1_001..1_010 {
            let requests = oms.diff(now(tick), &[]);
            assert!(
                !requests
                    .iter()
                    .any(|request| matches!(request, Request::Place(o) if o.side == Side::Bid)),
                "the refused level is not re-sent at {tick}",
            );
        }

        // The ask was never held back: a bid that would cross says nothing
        // about the other side.
        assert!(
            oms.slot(&call("60000"), Side::Ask).in_flight()
                || oms.slot(&call("60000"), Side::Ask).placed().is_some(),
            "the ask is untouched by the bid's refusal",
        );

        // A fresh decision is sent, even at the same price: the strategy has
        // looked again and still wants it.
        let mut again = want(call("60000"), Some(at("0.04", "10")), None);
        again.as_of = now(1_010);
        let requests = oms.diff(now(1_010), &[again]);
        assert!(
            requests
                .iter()
                .any(|request| matches!(request, Request::Place(o) if o.side == Side::Bid)),
            "a decision taken since the refusal is sent",
        );
    }

    /// A decision that arrived while the refused place was in flight was
    /// never asked of the venue, so the refusal does not hold it: the diff
    /// after the reject sends it, with no newer decision needed.
    #[test]
    fn a_reject_spends_the_decision_it_answered_not_one_that_arrived_in_flight() {
        let mut oms = oms();
        let first = want(call("60000"), Some(at("0.04", "10")), None);
        let requests = oms.diff(now(1_000), &[first]);
        let placed = match requests.as_slice() {
            [Request::Place(order)] => *order,
            other => panic!("one place, not {other:?}"),
        };

        // The strategy decides again while the place is in flight: nothing
        // goes out, one request in flight per slot, but the decision is
        // remembered.
        let newer = Desired {
            as_of: now(1_005),
            ..want(call("60000"), Some(at("0.05", "10")), None)
        };
        assert!(oms.diff(now(1_005), &[newer]).is_empty());

        oms.apply(&[reject(placed.id, RejectReason::PostOnlyWouldCross)]);
        let requests = oms.diff(now(1_006), &[]);
        match requests.as_slice() {
            [Request::Place(order)] => {
                assert_eq!(
                    order.price,
                    Some(px("0.05")),
                    "the in-flight decision, not the refused one"
                );
            }
            other => panic!("the newer decision is placed, not {other:?}"),
        }

        // And that one, refused in turn, is held like any other.
        let id = match requests.as_slice() {
            [Request::Place(order)] => order.id,
            _ => unreachable!(),
        };
        oms.apply(&[reject(id, RejectReason::PostOnlyWouldCross)]);
        assert!(oms.diff(now(1_007), &[]).is_empty());
    }

    /// The outer band's order: a limit at the cap, immediate-or-cancel —
    /// never a market order, and never post-only. It is sent once per
    /// decision, and the slot goes back to idle on whatever ends it.
    #[test]
    fn a_cross_is_an_immediate_or_cancel_limit_at_its_cap_sent_once() {
        let mut oms = oms();
        let sent = oms.diff(now(1_000), &[cross(Side::Bid, at("61000", "1000"), 1_000)]);
        let [Request::Place(order)] = sent.as_slice() else {
            panic!("expected one place: {sent:?}")
        };
        assert_eq!(
            order.kind,
            crate::adapters::execution::order::OrderKind::Limit
        );
        assert_eq!(order.tif, TimeInForce::ImmediateOrCancel);
        assert_eq!(order.price, Some(px("61000")), "the cap is the limit");
        assert!(order.is_well_formed());
        assert!(oms.slot(&perp(), Side::Bid).in_flight());

        // A venue reporting in FIX's order acks it before it trades. For
        // that instant it is working, and nothing is wanted on the side —
        // the decision is spent — but it is not pulled: it cannot rest, and
        // a cancel would only be answered with an unknown order.
        oms.apply(&[ack(order.id)]);
        assert!(matches!(oms.slot(&perp(), Side::Bid), Slot::Working(_)));
        let between = oms.diff(now(1_000), &[]);
        assert!(between.is_empty(), "{between:?}");

        // Part of it fills and the venue kills the rest.
        oms.apply(&[fill(order.id, "400", "600"), cancelled(order.id)]);
        assert_eq!(oms.slot(&perp(), Side::Bid), Slot::Idle);

        // The same decision is not crossed on again, however long it stays
        // fresh: the fill changed the delta, and only the strategy knows by
        // how much.
        for tick in [1_001, 2_000_000_000, 3_000_000_000] {
            let again = oms.diff(now(tick), &[]);
            assert!(again.is_empty(), "at {tick}: {again:?}");
        }
    }

    /// A killed IOC must not become a loop: in the graph its report decides
    /// at once, and a decision that crossed again in the next instant would
    /// be killed again in the one after. `Config::retake` spaces them.
    #[test]
    fn a_second_cross_waits_for_the_retake_interval() {
        let mut oms = oms();
        let sent = oms.diff(now(1_000), &[cross(Side::Bid, at("61000", "1000"), 1_000)]);
        let [Request::Place(first)] = sent.as_slice() else {
            panic!("{sent:?}")
        };
        oms.apply(&[cancelled(first.id)]);

        // A fresh decision in the next instant is held, not sent...
        let held = oms.diff(now(1_001), &[cross(Side::Bid, at("61000", "1000"), 1_001)]);
        assert!(held.is_empty(), "{held:?}");
        // ...and the instrument is not forgotten while it is held, or the
        // interval would be forgotten with it.
        let later = 1_000 + 1_000_000_000;
        let sent = oms.diff(now(later), &[cross(Side::Bid, at("61000", "1000"), later)]);
        let [Request::Place(second)] = sent.as_slice() else {
            panic!("a decision past the interval is sent: {sent:?}")
        };
        assert_ne!(second.id, first.id);
    }

    /// `Duration::MAX` spaces crosses for ever, without overflowing
    /// `crossed + retake`: the second is never sent, however late it comes.
    #[test]
    fn a_retake_of_duration_max_never_crosses_again() {
        let mut oms = Oms::new(Config {
            retake: Duration::MAX,
            ..config()
        });
        let sent = oms.diff(now(1_000), &[cross(Side::Bid, at("61000", "1000"), 1_000)]);
        let [Request::Place(first)] = sent.as_slice() else {
            panic!("{sent:?}")
        };
        oms.apply(&[cancelled(first.id)]);

        let late = u64::from(NanoTime::MAX);
        let held = oms.diff(now(late), &[cross(Side::Bid, at("61000", "1000"), late)]);
        assert!(held.is_empty(), "{held:?}");
    }

    /// A passive hedge resting when the book crosses its outer band is pulled
    /// first — a post-only order cannot be amended into a crossing one — and
    /// the cross goes from the idle slot on the next diff.
    #[test]
    fn a_resting_hedge_is_pulled_before_the_cross() {
        let mut oms = oms();
        let placed = oms.diff(now(1_000), &[want(perp(), Some(at("60990", "1000")), None)]);
        let [Request::Place(resting)] = placed.as_slice() else {
            panic!("{placed:?}")
        };
        assert_eq!(
            resting.kind,
            crate::adapters::execution::order::OrderKind::PostOnly
        );
        oms.apply(&[ack(resting.id)]);

        let pulled = oms.diff(now(1_001), &[cross(Side::Bid, at("61050", "1000"), 1_001)]);
        assert_eq!(pulled.as_slice(), [Request::Cancel(resting.id)]);
        oms.apply(&[cancelled(resting.id)]);

        let crossed = oms.diff(now(1_002), &[]);
        let [Request::Place(order)] = crossed.as_slice() else {
            panic!("the cross follows the cancel: {crossed:?}")
        };
        assert_eq!(order.tif, TimeInForce::ImmediateOrCancel);
        assert_eq!(order.price, Some(px("61050")));
    }

    /// An OMS resumed in an epoch mints its ids there, so the
    /// first order of a restarted process is not the first order of the one
    /// before it — and epoch zero is the bare counter a backtest has always
    /// had.
    #[test]
    fn ids_are_minted_under_the_epoch() {
        let placed = |mut oms: Oms| match oms
            .diff(
                now(1_000),
                &[want(call("60000"), Some(at("0.05", "10")), None)],
            )
            .as_slice()
        {
            [Request::Place(order)] => order.id,
            other => panic!("expected one place, got {other:?}"),
        };
        assert_eq!(placed(oms()), ClientOrderId(1));

        let epoch = Epoch::new(7).unwrap();
        let resumed = Oms::resumed(config(), epoch);
        assert_eq!(resumed.epoch(), epoch);
        let id = placed(resumed);
        assert_eq!(id.epoch(), epoch);
        assert_eq!(id.sequence(), 1);
        let next = placed(Oms::resumed(config(), epoch.next()));
        assert_ne!(id, next, "the next process's first order is another number");
        assert_eq!(next.sequence(), 1);
    }

    /// An OMS whose budget is `burst` at once and `rate` a second — stated
    /// at full headroom, so the budget is exactly what the test names.
    fn budgeted(rate: u32, burst: u32) -> Oms {
        let terms = crate::adapters::execution::rate_limit::Terms { rate, burst };
        Oms::new(Config {
            rate: OrderRate::new(terms, terms, 1.0).unwrap(),
            ..config()
        })
    }

    const SEC: u64 = 1_000_000_000;

    /// A two-way on each of `n` contracts, as of `as_of`.
    fn two_ways(n: usize, as_of: u64) -> Vec<Desired> {
        (0..n)
            .map(|i| Desired {
                as_of: now(as_of),
                ..want(
                    call(&(60_000 + 1_000 * i).to_string()),
                    Some(at("0.05", "1")),
                    Some(at("0.06", "1")),
                )
            })
            .collect()
    }

    /// The budget refills at its rate from engine time: spent, nothing goes
    /// until a token has refilled, and what waited goes on the first diff
    /// after — with no new desired to prompt it.
    #[test]
    fn the_budget_refills_and_what_waited_goes_on_a_later_diff() {
        let mut oms = budgeted(2, 3);
        let first = oms.diff(now(SEC), &two_ways(2, SEC));
        assert_eq!(first.len(), 3, "the burst, and no more: {first:?}");
        assert_eq!(
            oms.pacing(),
            Pacing {
                deferred: 1,
                superseded: 0,
                waiting: 1
            }
        );

        // Nothing has refilled a nanosecond before half a second.
        let early = oms.diff(now(SEC + SEC / 2 - 1), &[]);
        assert!(early.is_empty(), "{early:?}");
        assert_eq!(oms.pacing().deferred, 1, "a wait is counted once");

        // At half a second one token has: the sweep sends what waited.
        let later = oms.diff(now(SEC + SEC / 2), &[]);
        let [Request::Place(order)] = later.as_slice() else {
            panic!("the waiting place goes: {later:?}")
        };
        assert_eq!(order.created, now(SEC + SEC / 2), "stamped when sent");
        assert_eq!(oms.pacing().waiting, 0);
    }

    /// Short of tokens, cancels are paid for before amends and amends before
    /// places, whatever the walk order — and the burst still comes out in
    /// walk order.
    #[test]
    fn short_of_tokens_cancels_go_before_amends_before_places() {
        let mut oms = budgeted(1, 4);
        // Four working bids on four contracts: the whole burst.
        let placed = oms.diff(
            now(SEC),
            &(0..4)
                .map(|i| {
                    want(
                        call(&(60_000 + 1_000 * i).to_string()),
                        Some(at("0.05", "1")),
                        None,
                    )
                })
                .collect::<Vec<_>>(),
        );
        assert_eq!(placed.len(), 4);
        for request in &placed {
            oms.apply(&[ack(request.order().unwrap())]);
        }

        // Ten seconds on the bucket is full again (four). Wanted now, in
        // walk order: a place on a new contract (59000 sorts first), an
        // amend (60000), a cancel (61000), an amend (62000), a cancel (63000).
        let decided = |strike: &str, bid: Option<Level>| Desired {
            as_of: now(11 * SEC),
            ..want(call(strike), bid, None)
        };
        let requests = oms.diff(
            now(11 * SEC),
            &[
                decided("59000", Some(at("0.05", "1"))),
                decided("60000", Some(at("0.04", "1"))),
                decided("61000", None),
                decided("62000", Some(at("0.04", "1"))),
                decided("63000", None),
            ],
        );
        let kinds: Vec<(&str, Inst)> = requests
            .iter()
            .map(|request| match request {
                Request::Cancel(id) => ("cancel", instrument_of(&placed, *id)),
                Request::Amend(amend) => ("amend", amend.instrument),
                other => panic!("the place is the one left waiting: {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                ("amend", call("60000")),
                ("cancel", call("61000")),
                ("amend", call("62000")),
                ("cancel", call("63000")),
            ],
            "both cancels and both amends, in walk order; not the place"
        );
        assert_eq!(oms.slot(&call("59000"), Side::Bid), Slot::Idle);
        assert_eq!(oms.pacing().waiting, 1);
    }

    fn instrument_of(placed: &[Request], id: ClientOrderId) -> Inst {
        placed
            .iter()
            .find_map(|request| match request {
                Request::Place(order) if order.id == id => Some(order.instrument),
                _ => None,
            })
            .expect("a placed id")
    }

    /// A request that waited is dropped, never sent stale, when what is
    /// wanted changes before a token arrives: the next diff sends what is
    /// wanted *then*, and the drop is counted.
    #[test]
    fn a_request_superseded_while_it_waits_is_dropped_not_sent_stale() {
        let mut oms = budgeted(1, 1);
        let sent = oms.diff(now(SEC), &two_ways(1, SEC));
        assert_eq!(sent.len(), 1, "{sent:?}");
        // The ask at 0.06 waits. The strategy moves it to 0.07.
        let moved = Desired {
            as_of: now(SEC + 1),
            ..want(call("60000"), Some(at("0.05", "1")), Some(at("0.07", "1")))
        };
        let quiet = oms.diff(now(SEC + 1), &[moved]);
        assert!(quiet.is_empty(), "{quiet:?}");
        assert_eq!(
            oms.pacing(),
            Pacing {
                deferred: 2,
                superseded: 1,
                waiting: 1
            },
            "the 0.06 ask was superseded; the 0.07 ask waits in its place"
        );

        let [Request::Place(ask)] = oms.diff(now(2 * SEC + 1), &[]).as_slice().to_owned()[..]
        else {
            panic!("the ask goes once a token has refilled")
        };
        assert_eq!(ask.price, Some(px("0.07")), "what is wanted now, not then");

        // A wait that ends in nothing wanted is superseded too.
        let mut oms = budgeted(1, 1);
        oms.diff(now(SEC), &two_ways(1, SEC));
        oms.diff(
            now(SEC + 1),
            &[Desired::nothing(call("60000"), now(SEC + 1))],
        );
        assert_eq!(oms.pacing().superseded, 1);
        assert_eq!(oms.pacing().waiting, 0);
    }

    /// What `pacing().waiting` would say if it walked the entries, as it
    /// used to: every hold set on either side, and a pending cancel-all.
    fn waiting_by_walk(oms: &Oms) -> u32 {
        oms.entries
            .values()
            .map(|entry| entry.bids.holds() + entry.asks.holds())
            .sum::<u32>()
            + u32::from(oms.cancel_all_pending)
    }

    /// `waiting` is a count kept where a hold is set and released, not a
    /// walk — so it has to agree with the walk through every way a hold
    /// moves: set, re-planned in place, superseded, paid for, dropped with
    /// what nothing wants, and a cancel-all waiting beside them.
    #[test]
    fn the_waiting_count_agrees_with_a_walk_of_the_books() {
        let mut oms = budgeted(1, 2);
        let agrees = |oms: &Oms, step: &str| {
            assert_eq!(oms.pacing().waiting, waiting_by_walk(oms), "{step}");
        };

        // Three two-ways and two tokens: four held.
        let sent = oms.diff(now(SEC), &two_ways(3, SEC));
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert_eq!(oms.pacing().waiting, 4);
        agrees(&oms, "held");

        // One moved and one withdrawn while they wait: superseded, one held
        // again in its place and one released.
        let moved = Desired {
            as_of: now(SEC + 1),
            ..want(call("61000"), Some(at("0.04", "1")), Some(at("0.06", "1")))
        };
        let withdrawn = Desired::nothing(call("62000"), now(SEC + 1));
        oms.diff(now(SEC + 1), &[moved, withdrawn]);
        assert!(oms.pacing().superseded >= 1);
        agrees(&oms, "superseded");

        // A refill pays for some of what waits, and releases its holds.
        let before = oms.pacing().waiting;
        let paid = oms.diff(now(2 * SEC + 1), &[]);
        assert!(!paid.is_empty(), "a token refilled");
        assert!(oms.pacing().waiting < before);
        agrees(&oms, "paid for");

        // A cancel-all its bucket cannot pay for waits beside them.
        while oms.cancel_all(now(2 * SEC + 1)).is_some() {}
        assert!(oms.cancel_all_pending);
        agrees(&oms, "a cancel-all waiting");

        // It drops every desired, so what waited is superseded, and the
        // cancel-all goes once its bucket refills.
        let mut t = 3 * SEC;
        while oms.pacing().waiting > 0 {
            oms.diff(now(t), &[]);
            agrees(&oms, "released");
            t += SEC;
            assert!(t < 60 * SEC, "stuck waiting {}", oms.pacing().waiting);
        }
        assert_eq!(waiting_by_walk(&oms), 0);
    }

    /// Forty contracts wanted at once: the budget's burst goes, the rest at
    /// its rate on the sweep, and at no instant does what was sent exceed
    /// the burst plus the rate over the time elapsed — the venue's stated
    /// limit is never approached. Everything wanted is placed in the end.
    #[test]
    fn forty_desireds_go_out_within_the_budget_over_time() {
        let mut oms = oms();
        let (rate, burst) = (4, 16);
        assert_eq!(
            TIER.budget().0,
            crate::adapters::execution::rate_limit::Terms { rate, burst }
        );
        // The venue's own limit, metering what the OMS sends.
        let mut venue = Bucket::new(TIER.trading());

        let desired: Vec<Desired> = (0..40)
            .map(|i| Desired {
                as_of: now(SEC),
                ..want(
                    call(&(60_000 + 1_000 * i).to_string()),
                    Some(at("0.05", "1")),
                    None,
                )
            })
            .collect();
        let mut sent = 0;
        let mut t = SEC;
        let mut first = true;
        while sent < 40 {
            let requests = if first {
                oms.diff(now(t), &desired)
            } else {
                // The sweep: nothing new arrives.
                oms.diff(now(t), &[])
            };
            first = false;
            for request in &requests {
                assert!(matches!(request, Request::Place(_)), "{request:?}");
                assert!(venue.try_take(now(t)), "the venue's limit at {t}");
            }
            sent += requests.len();
            let elapsed = (t - SEC) / SEC;
            assert!(
                sent as u64 <= u64::from(burst + rate * elapsed as u32),
                "{sent} sent {elapsed}s in"
            );
            t += SEC;
            assert!(t < 60 * SEC, "stuck at {sent}");
        }
        assert_eq!(sent, 40);
        // 16 at once, then four a second: six more sweeps.
        assert_eq!(t, 8 * SEC);
        assert_eq!(oms.pacing().waiting, 0);
        assert_eq!(oms.pacing().deferred, 24);
        assert_eq!(oms.pacing().superseded, 0);
    }

    /// Cancel-all is paid from its own bucket: a trading budget spent to
    /// nothing does not hold it back, and one it cannot pay for goes at the
    /// head of the first diff that can.
    #[test]
    fn cancel_all_has_its_own_bucket() {
        let terms = crate::adapters::execution::rate_limit::Terms { rate: 1, burst: 2 };
        let one = crate::adapters::execution::rate_limit::Terms { rate: 1, burst: 1 };
        let mut oms = Oms::new(Config {
            rate: OrderRate::new(terms, one, 1.0).unwrap(),
            ..config()
        });
        let placed = oms.diff(now(SEC), &two_ways(1, SEC));
        assert_eq!(placed.len(), 2, "the trading bucket is spent");
        for request in &placed {
            oms.apply(&[ack(request.order().unwrap())]);
        }
        assert_eq!(
            oms.cancel_all(now(SEC)),
            Some(Request::CancelAll),
            "from its own bucket"
        );
        // Its bucket is spent now; the next waits, and pulls nothing yet.
        for request in &placed {
            oms.apply(&[cancelled(request.order().unwrap())]);
        }
        let again = oms.diff(now(SEC + 1), &two_ways(1, SEC + 1));
        assert!(again.is_empty(), "the trading bucket is still empty");
        assert_eq!(oms.cancel_all(now(SEC + 2)), None);
        assert_eq!(oms.pacing().waiting, 3, "a cancel-all and two places");

        // A second on, each bucket holds one: the cancel-all goes first,
        // and one place — the desireds it dropped are wanted again only
        // once the strategy says so.
        let head = oms.diff(now(2 * SEC + 2), &two_ways(1, 2 * SEC + 2));
        assert_eq!(head.first(), Some(&Request::CancelAll), "{head:?}");
        assert_eq!(head.len(), 2, "{head:?}");
    }

    /// a venue with no post-only rests a passive level as a plain
    /// limit, good till cancelled; a cross is unchanged.
    #[test]
    fn a_passive_level_is_sent_as_the_venues_resting_kind() {
        let mut oms = Oms::new(Config {
            passive: Passive::Limit,
            ..config()
        });
        let out = oms.diff(
            now(1_000),
            &[want(call("60000"), Some(at("0.01", "1")), None)],
        );
        let [Request::Place(order)] = out.as_slice() else {
            panic!("{out:?}")
        };
        assert_eq!(
            order.kind,
            crate::adapters::execution::order::OrderKind::Limit
        );
        assert_eq!(order.tif, TimeInForce::GoodTillCancel);
        order.validate().unwrap();
        assert_eq!(Passive::default(), Passive::PostOnly, "the inert one");
    }

    /// past its ratio the OMS holds places and amends, still pulls what
    /// it no longer wants, and a fill buys more room.
    #[test]
    fn a_message_to_trade_ratio_holds_what_adds_risk_and_never_a_cancel() {
        let mut oms = Oms::new(Config {
            ratio: Some(Ratio {
                messages_per_fill: 2,
                free: 2,
            }),
            ..config()
        });
        let c60 = call("60000");
        let c61 = call("61000");
        let out = oms.diff(
            now(1_000),
            &[
                want(c60, Some(at("0.01", "1")), None),
                want(c61, Some(at("0.01", "1")), None),
            ],
        );
        assert_eq!(out.len(), 2);
        let acks: Vec<Report> = out
            .iter()
            .filter_map(Request::order)
            .map(|order| {
                Report::Ack(Ack {
                    order,
                    ..Ack::default()
                })
            })
            .collect();
        oms.apply(&acks);

        // Out of room: a re-price waits.
        let held = oms.diff(
            now(1_000),
            &[Desired {
                as_of: now(1_001),
                ..want(c60, Some(at("0.02", "1")), None)
            }],
        );
        assert!(held.is_empty(), "{held:?}");
        assert_eq!(
            oms.pacing().waiting,
            1,
            "it waits, re-planned like any unpaid request"
        );

        // Withdrawing is never held.
        let pulled = oms.diff(now(1_000), &[Desired::nothing(c61, now(1_002))]);
        assert!(matches!(pulled.as_slice(), [Request::Cancel(_)]));
        assert_eq!(oms.ratio(), Some((3, 0)));

        // A fill buys two more, and the re-price goes.
        let Slot::Working(resting) = oms.slot(&c60, Side::Bid) else {
            panic!()
        };
        oms.apply(&[Report::Fill(Fill {
            order: resting.id,
            exec_id: ExecId::new("f").unwrap(),
            instrument: c60,
            side: Side::Bid,
            qty: qty("0.5"),
            filled: qty("0.5"),
            remaining: qty("0.5"),
            price: px("0.01"),
            fee: Qty::ZERO,
            liquidity: Liquidity::Maker,
            venue_time: None,
            recv_time: now(1_003),
        })]);
        let out = oms.diff(now(1_004), &[]);
        assert!(matches!(out.as_slice(), [Request::Amend(_)]), "{out:?}");
    }

    /// a book that is not open takes cancels and nothing else, and what
    /// was held goes out at the open. A day lifetime is a day order.
    #[test]
    fn a_closed_book_takes_only_cancels_and_the_open_sends_what_waited() {
        let mut oms = Oms::new(Config {
            lifetime: Lifetime::Day,
            ..config()
        });
        let c60 = call("60000");
        let c61 = call("61000");
        let out = oms.diff(now(1_000), &[want(c60, Some(at("0.01", "1")), None)]);
        let [Request::Place(order)] = out.as_slice() else {
            panic!("{out:?}")
        };
        assert_eq!(order.tif, TimeInForce::Day);
        oms.apply(&[Report::Ack(Ack {
            order: order.id,
            ..Ack::default()
        })]);

        oms.trading(TradingState::Halted);
        let held = oms.diff(
            now(1_000),
            &[
                Desired {
                    as_of: now(1_001),
                    ..want(c60, Some(at("0.02", "1")), None)
                },
                Desired {
                    as_of: now(1_001),
                    ..want(c61, Some(at("0.01", "1")), None)
                },
            ],
        );
        assert!(held.is_empty(), "no amend, no place: {held:?}");
        let pulled = oms.diff(now(1_000), &[Desired::nothing(c60, now(1_002))]);
        assert!(matches!(pulled.as_slice(), [Request::Cancel(_)]));

        oms.trading(TradingState::Open);
        let opened = oms.diff(now(1_003), &[]);
        assert!(
            matches!(opened.as_slice(), [Request::Place(order)] if order.instrument == c61),
            "{opened:?}"
        );
    }

    // Depth: a ladder a side. Every test above
    // is a ladder of one.

    /// `levels` as a ladder on `side`, best first.
    fn ladder(side: Side, levels: &[(&str, &str)]) -> Ladder {
        let levels: Vec<Level> = levels.iter().map(|(p, q)| at(p, q)).collect();
        Ladder::from_levels(side, &levels).expect("a monotone ladder")
    }

    /// `call("60000")` wanting `levels` bid, as of `as_of`.
    fn bids(levels: &[(&str, &str)], as_of: u64) -> Desired {
        Desired::rest(
            call("60000"),
            now(as_of),
            Side::Bid,
            ladder(Side::Bid, levels),
        )
    }

    /// Every place in `requests`, acked.
    fn ack_places(oms: &mut Oms, requests: &[Request]) {
        let acks: Vec<Report> = requests
            .iter()
            .filter_map(|request| match request {
                Request::Place(order) => Some(ack(order.id)),
                _ => None,
            })
            .collect();
        oms.apply(&acks);
    }

    /// The prices working or in flight on the bid of `call("60000")`, best
    /// first.
    fn bid_prices(oms: &Oms) -> Vec<Px> {
        let mut prices: Vec<Px> = oms
            .slots(&call("60000"), Side::Bid)
            .iter()
            .filter_map(|slot| slot.placed().map(|placed| placed.price))
            .collect();
        prices.sort_unstable_by(|a, b| b.cmp(a));
        prices
    }

    /// The id of the order working at `price` on the bid of `call("60000")`.
    fn bid_at(oms: &Oms, price: &str) -> ClientOrderId {
        oms.slots(&call("60000"), Side::Bid)
            .iter()
            .find_map(|slot| match slot {
                Slot::Working(resting) if resting.price == px(price) => Some(resting.id),
                _ => None,
            })
            .unwrap_or_else(|| panic!("nothing working at {price}"))
    }

    /// An OMS with `[0.050, 0.049, 0.048] × 10` working on the bid of
    /// `call("60000")`, decided at 1_000.
    fn with_a_working_ladder() -> Oms {
        let mut oms = oms();
        let placed = oms.diff(
            now(1_000),
            &[bids(
                &[("0.050", "10"), ("0.049", "10"), ("0.048", "10")],
                1_000,
            )],
        );
        ack_places(&mut oms, &placed);
        assert_eq!(bid_prices(&oms), [px("0.050"), px("0.049"), px("0.048")]);
        oms
    }

    /// A ladder is built best first, strictly away from the touch, and no
    /// deeper than the crate allows; anything else is refused at
    /// construction.
    #[test]
    fn a_ladder_is_strictly_monotone_away_from_the_touch() {
        let bid = [at("0.050", "1"), at("0.049", "1")];
        assert_eq!(
            Ladder::from_levels(Side::Bid, &bid).map(|l| l.depth()),
            Ok(2)
        );
        assert_eq!(
            Ladder::from_levels(Side::Ask, &bid),
            Err(LadderError::NotMonotone { rank: 1 }),
            "an ask ladder climbs away from the touch"
        );
        assert_eq!(
            Ladder::from_levels(Side::Bid, &[at("0.050", "1"), at("0.050", "2")]),
            Err(LadderError::NotMonotone { rank: 1 }),
            "two levels at one price are two orders the diff cannot tell apart"
        );
        let deep = vec![at("0.050", "1"); MAX_DEPTH + 1];
        assert_eq!(
            Ladder::from_levels(Side::Bid, &deep),
            Err(LadderError::TooDeep(MAX_DEPTH + 1))
        );
        assert_eq!(Ladder::from_levels(Side::Bid, &[]), Ok(Ladder::none()));

        let one = Ladder::one(at("0.050", "1"));
        assert_eq!(one.depth(), 1);
        assert_eq!(one.best(), Some(at("0.050", "1")));
        assert_eq!(Ladder::from(None), Ladder::none());
        assert!(Ladder::none().is_empty());
    }

    /// A ladder with nothing working is one place a level, best first, each
    /// in a slot of its own.
    #[test]
    fn a_ladder_is_placed_best_first() {
        let mut oms = oms();
        let placed = oms.diff(
            now(1_000),
            &[bids(
                &[("0.050", "10"), ("0.049", "5"), ("0.048", "1")],
                1_000,
            )],
        );
        let prices: Vec<(Px, Qty)> = placed
            .iter()
            .map(|request| match request {
                Request::Place(order) => (order.price.expect("a limit"), order.qty),
                other => panic!("expected places, got {other:?}"),
            })
            .collect();
        assert_eq!(
            prices,
            [
                (px("0.050"), qty("10")),
                (px("0.049"), qty("5")),
                (px("0.048"), qty("1"))
            ]
        );
        assert_eq!(
            oms.slots(&call("60000"), Side::Bid)
                .iter()
                .filter(|slot| slot.in_flight())
                .count(),
            3
        );
        assert_eq!(
            oms.slot(&call("60000"), Side::Bid)
                .placed()
                .map(|p| p.price),
            Some(px("0.050")),
            "`slot` is the one nearest the touch"
        );
    }

    /// The shifting ladder: one tick away from the touch is one cancel and
    /// one place. The orders at 0.049 and 0.048 are matched by price and
    /// keep their queue place; rank matching would have amended all three.
    #[test]
    fn a_ladder_shifting_one_tick_is_one_cancel_and_one_place() {
        let mut oms = with_a_working_ladder();
        let (top, middle, bottom) = (
            bid_at(&oms, "0.050"),
            bid_at(&oms, "0.049"),
            bid_at(&oms, "0.048"),
        );
        let shifted = oms.diff(
            now(1_100),
            &[bids(
                &[("0.049", "10"), ("0.048", "10"), ("0.047", "10")],
                1_100,
            )],
        );
        match shifted.as_slice() {
            [Request::Cancel(pulled), Request::Place(order)] => {
                assert_eq!(*pulled, top, "the top comes off");
                assert_eq!(order.price, Some(px("0.047")), "a new bottom goes on");
            }
            other => panic!("expected one cancel and one place, got {other:?}"),
        }
        assert_eq!(bid_at(&oms, "0.049"), middle, "untouched");
        assert_eq!(bid_at(&oms, "0.048"), bottom, "untouched");

        // And the other way: towards the touch.
        let mut oms = with_a_working_ladder();
        let bottom = bid_at(&oms, "0.048");
        let shifted = oms.diff(
            now(1_100),
            &[bids(
                &[("0.051", "10"), ("0.050", "10"), ("0.049", "10")],
                1_100,
            )],
        );
        assert!(
            matches!(
                shifted.as_slice(),
                [Request::Cancel(pulled), Request::Place(order)]
                    if *pulled == bottom && order.price == Some(px("0.051"))
            ),
            "{shifted:?}"
        );
    }

    /// The shifting ladder on the ask, where away from the touch is up:
    /// `[100, 101, 102]` becoming `[101, 102, 103]` pulls 100 and places
    /// 103, and 101 and 102 keep their ids. The bid's test with every
    /// comparison mirrored — a sign slip in `ahead` or `touch_order` would
    /// pass that one and fail this.
    #[test]
    fn an_ask_ladder_shifting_one_tick_is_one_cancel_and_one_place() {
        let asks = |levels: &[(&str, &str)], as_of: u64| Desired {
            bids: Ladder::none(),
            asks: ladder(Side::Ask, levels),
            ..bids(&[], as_of)
        };
        let ask_at = |oms: &Oms, price: &str| -> ClientOrderId {
            oms.slots(&call("60000"), Side::Ask)
                .iter()
                .find_map(|slot| match slot {
                    Slot::Working(resting) if resting.price == px(price) => Some(resting.id),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("nothing working at {price}"))
        };
        let mut oms = oms();
        let placed = oms.diff(
            now(1_000),
            &[asks(&[("100", "10"), ("101", "10"), ("102", "10")], 1_000)],
        );
        let prices: Vec<Option<Px>> = placed
            .iter()
            .map(|request| match request {
                Request::Place(order) => order.price,
                other => panic!("expected places, got {other:?}"),
            })
            .collect();
        assert_eq!(
            prices,
            [Some(px("100")), Some(px("101")), Some(px("102"))],
            "best first, which on the ask is lowest first"
        );
        ack_places(&mut oms, &placed);
        let (low, middle, high) = (
            ask_at(&oms, "100"),
            ask_at(&oms, "101"),
            ask_at(&oms, "102"),
        );

        let shifted = oms.diff(
            now(1_100),
            &[asks(&[("101", "10"), ("102", "10"), ("103", "10")], 1_100)],
        );
        match shifted.as_slice() {
            [Request::Cancel(pulled), Request::Place(order)] => {
                assert_eq!(*pulled, low, "the old best comes off");
                assert_eq!(order.side, Side::Ask);
                assert_eq!(order.price, Some(px("103")), "a new worst goes on");
            }
            other => panic!("expected one cancel and one place, got {other:?}"),
        }
        assert_eq!(ask_at(&oms, "101"), middle, "untouched");
        assert_eq!(ask_at(&oms, "102"), high, "untouched");
    }

    /// A slot with a cancel in flight answers for no level: the ladder
    /// wanting that price back is placed afresh beside the order being
    /// pulled, not left to an order that is on its way out.
    #[test]
    fn a_slot_being_pulled_answers_for_nothing() {
        let mut oms = with_a_working_ladder();
        let bottom = bid_at(&oms, "0.048");
        let shrunk = oms.diff(
            now(1_100),
            &[bids(&[("0.050", "10"), ("0.049", "10")], 1_100)],
        );
        assert_eq!(shrunk.as_slice(), [Request::Cancel(bottom)]);

        // Wanted back while the cancel is still in flight.
        let back = oms.diff(
            now(1_200),
            &[bids(
                &[("0.050", "10"), ("0.049", "10"), ("0.048", "10")],
                1_200,
            )],
        );
        match back.as_slice() {
            [Request::Place(order)] => {
                assert_eq!(order.price, Some(px("0.048")));
                assert_ne!(order.id, bottom, "a new order, not the one being pulled");
            }
            other => panic!("expected one place, got {other:?}"),
        }
        let pulled = oms
            .slots(&call("60000"), Side::Bid)
            .iter()
            .filter(|slot| matches!(slot, Slot::PendingCancel(resting) if resting.id == bottom))
            .count();
        assert_eq!(pulled, 1, "the cancel is still in flight, untouched");
    }

    /// A reject from an older decision records nothing: it can never match
    /// again, and stored it would evict a record of the current decision,
    /// whose level would then be sent a second time.
    #[test]
    fn a_stale_reject_does_not_evict_a_record_of_the_current_decision() {
        let mut rungs = Rungs::<Inst>::default();
        let current = now(1_100);
        let prices: Vec<Px> = (0..MAX_DEPTH)
            .map(|rank| px(&format!("0.05{rank}")))
            .collect();
        for &price in &prices {
            rungs.spend(current, price, current);
        }
        rungs.spend(now(1_000), px("0.060"), current);
        assert!(!rungs.is_spent(now(1_000), px("0.060")));
        for &price in &prices {
            assert!(rungs.is_spent(current, price), "{price:?} is still spent");
        }
    }

    /// Pass 1: a level whose price is working and whose size is not is an
    /// amend of that order's size, and of nothing else.
    #[test]
    fn a_resized_level_amends_only_its_own_order() {
        let mut oms = with_a_working_ladder();
        let middle = bid_at(&oms, "0.049");
        let resized = oms.diff(
            now(1_100),
            &[bids(
                &[("0.050", "10"), ("0.049", "4"), ("0.048", "10")],
                1_100,
            )],
        );
        assert_eq!(
            resized.as_slice(),
            [Request::Amend(Amend {
                order: middle,
                instrument: call("60000"),
                price: px("0.049"),
                qty: qty("4"),
                trigger: None,
            })]
        );
    }

    /// Pass 2: a ladder whose every price moved, none onto a working one,
    /// is an amend per order, best to best.
    #[test]
    fn a_ladder_moved_off_every_price_amends_best_to_best() {
        let mut oms = with_a_working_ladder();
        let ids = [
            bid_at(&oms, "0.050"),
            bid_at(&oms, "0.049"),
            bid_at(&oms, "0.048"),
        ];
        let moved = oms.diff(
            now(1_100),
            &[bids(
                &[("0.0505", "10"), ("0.0495", "10"), ("0.0485", "10")],
                1_100,
            )],
        );
        let amends: Vec<(ClientOrderId, Px)> = moved
            .iter()
            .map(|request| match request {
                Request::Amend(amend) => (amend.order, amend.price),
                other => panic!("expected amends, got {other:?}"),
            })
            .collect();
        assert_eq!(
            amends,
            [
                (ids[0], px("0.0505")),
                (ids[1], px("0.0495")),
                (ids[2], px("0.0485"))
            ]
        );
    }

    /// `min_requote` is judged per order: a ladder that moved less than it
    /// sends nothing, and one that moved by it amends every order.
    #[test]
    fn min_requote_is_judged_per_order() {
        let mut oms = Oms::new(Config {
            min_requote: px("0.001"),
            ..config()
        });
        let placed = oms.diff(
            now(1_000),
            &[bids(&[("0.050", "10"), ("0.048", "10")], 1_000)],
        );
        ack_places(&mut oms, &placed);
        let (top, behind) = (bid_at(&oms, "0.050"), bid_at(&oms, "0.048"));
        let small = oms.diff(
            now(1_100),
            &[bids(&[("0.0505", "10"), ("0.0485", "10")], 1_100)],
        );
        assert!(small.is_empty(), "{small:?}");
        let big = oms.diff(
            now(1_200),
            &[bids(&[("0.051", "10"), ("0.049", "10")], 1_200)],
        );
        let amends: Vec<(ClientOrderId, Px)> = big
            .iter()
            .map(|request| match request {
                Request::Amend(amend) => (amend.order, amend.price),
                other => panic!("expected amends, got {other:?}"),
            })
            .collect();
        assert_eq!(amends, [(top, px("0.051")), (behind, px("0.049"))]);
    }

    /// Pass 3: a ladder that shrinks pulls what it no longer wants, worst
    /// price first.
    #[test]
    fn a_shrinking_ladder_cancels_the_excess_worst_first() {
        let mut oms = with_a_working_ladder();
        let (middle, bottom) = (bid_at(&oms, "0.049"), bid_at(&oms, "0.048"));
        let shrunk = oms.diff(now(1_100), &[bids(&[("0.050", "10")], 1_100)]);
        assert_eq!(
            shrunk.as_slice(),
            [Request::Cancel(bottom), Request::Cancel(middle)]
        );
    }

    /// A post-only reject at rank 0 says the touch moved and nothing about
    /// the ranks behind it: the refused level waits for a fresh decision,
    /// and the rest of the ladder is not held back with it.
    #[test]
    fn a_refused_level_waits_and_the_rest_of_the_ladder_does_not() {
        let mut oms = oms();
        let want = bids(&[("0.050", "10"), ("0.049", "10"), ("0.048", "10")], 1_000);
        let placed = oms.diff(now(1_000), &[want]);
        let ids: Vec<ClientOrderId> = placed
            .iter()
            .map(|request| match request {
                Request::Place(order) => order.id,
                other => panic!("{other:?}"),
            })
            .collect();
        oms.apply(&[
            reject(ids[0], RejectReason::PostOnlyWouldCross),
            ack(ids[1]),
            ack(ids[2]),
        ]);
        assert_eq!(bid_prices(&oms), [px("0.049"), px("0.048")]);

        let again = oms.diff(now(1_050), &[want]);
        assert!(again.is_empty(), "the refused level is spent: {again:?}");

        let fresh = oms.diff(
            now(1_100),
            &[bids(
                &[("0.050", "10"), ("0.049", "10"), ("0.048", "10")],
                1_100,
            )],
        );
        assert!(
            matches!(fresh.as_slice(), [Request::Place(order)] if order.price == Some(px("0.050"))),
            "a fresh decision re-sends it, and only it: {fresh:?}"
        );
    }

    /// A fill to nothing retires its slot, and the next diff places the
    /// level again if it is still wanted — the refill.
    #[test]
    fn a_filled_rung_is_placed_again() {
        let mut oms = with_a_working_ladder();
        let middle = bid_at(&oms, "0.049");
        oms.apply(&[fill(middle, "10", "0")]);
        assert_eq!(bid_prices(&oms), [px("0.050"), px("0.048")]);
        let refill = oms.diff(
            now(1_100),
            &[bids(
                &[("0.050", "10"), ("0.049", "10"), ("0.048", "10")],
                1_100,
            )],
        );
        assert!(
            matches!(
                refill.as_slice(),
                [Request::Place(order)] if order.price == Some(px("0.049")) && order.id != middle
            ),
            "{refill:?}"
        );
    }

    /// One request in flight per slot: a place in flight answers for its
    /// level — it is not placed again beside itself — and a slot in flight
    /// is neither amended nor cancelled until its report lands.
    #[test]
    fn a_slot_in_flight_answers_its_level_and_emits_nothing() {
        let mut oms = oms();
        let placed = oms.diff(
            now(1_000),
            &[bids(&[("0.050", "10"), ("0.049", "10")], 1_000)],
        );
        let [Request::Place(top), Request::Place(_)] = placed.as_slice() else {
            panic!("{placed:?}")
        };
        // Only the top is acked; 0.049 is still in flight.
        oms.apply(&[ack(top.id)]);

        let same = oms.diff(
            now(1_100),
            &[bids(&[("0.050", "10"), ("0.049", "10")], 1_100)],
        );
        assert!(same.is_empty(), "nothing is placed beside itself: {same:?}");

        // The ladder moves one tick away: 0.049 is kept (in flight), 0.050
        // is pulled, 0.048 placed. The slot in flight sends nothing.
        let moved = oms.diff(
            now(1_200),
            &[bids(&[("0.049", "10"), ("0.048", "10")], 1_200)],
        );
        assert!(
            matches!(
                moved.as_slice(),
                [Request::Cancel(pulled), Request::Place(order)]
                    if *pulled == top.id && order.price == Some(px("0.048"))
            ),
            "{moved:?}"
        );

        // Moved again, off every price: the slot in flight is paired and
        // waits; the working one is amended.
        let mut oms = oms_with_one_acked_of_two();
        let moved = oms.diff(
            now(1_100),
            &[bids(&[("0.0505", "10"), ("0.0495", "10")], 1_100)],
        );
        assert!(
            matches!(moved.as_slice(), [Request::Amend(amend)] if amend.price == px("0.0505")),
            "one amend, and no place beside the order in flight: {moved:?}"
        );
    }

    /// `[0.050, 0.049] × 10` bid, with only the 0.050 acked.
    fn oms_with_one_acked_of_two() -> Oms {
        let mut oms = oms();
        let placed = oms.diff(
            now(1_000),
            &[bids(&[("0.050", "10"), ("0.049", "10")], 1_000)],
        );
        let [Request::Place(top), Request::Place(_)] = placed.as_slice() else {
            panic!("{placed:?}")
        };
        oms.apply(&[ack(top.id)]);
        oms
    }

    /// `CancelAll` empties every slot of every ladder.
    #[test]
    fn cancel_all_empties_every_rung() {
        let mut oms = with_a_working_ladder();
        assert_eq!(oms.cancel_all(now(1_100)), Some(Request::CancelAll));
        let slots = oms.slots(&call("60000"), Side::Bid);
        let ids: Vec<ClientOrderId> = slots
            .iter()
            .filter_map(|slot| match slot {
                Slot::PendingCancel(resting) => Some(resting.id),
                Slot::Idle => None,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(ids.len(), 3);
        oms.apply(&ids.iter().map(|&id| cancelled(id)).collect::<Vec<_>>());
        assert!(
            oms.slots(&call("60000"), Side::Bid)
                .iter()
                .all(|slot| *slot == Slot::Idle)
        );
        assert!(
            oms.diff(now(1_200), &[]).is_empty(),
            "nothing is wanted after"
        );
    }

    /// A cross is a cap, not a shape: a crossing ladder deeper than one is
    /// refused whole, and counted.
    #[test]
    fn a_crossing_ladder_deeper_than_one_is_refused() {
        let mut oms = oms();
        let deep = Desired {
            intent: Intent::Cross,
            ..bids(&[("0.050", "10"), ("0.049", "10")], 1_000)
        };
        assert!(oms.diff(now(1_000), &[deep]).is_empty());
        assert_eq!(oms.refused(), 1);

        let one = Desired {
            intent: Intent::Cross,
            ..bids(&[("0.050", "10")], 1_000)
        };
        let crossed = oms.diff(now(1_000), &[one]);
        assert!(
            matches!(crossed.as_slice(), [Request::Place(order)] if order.tif == TimeInForce::ImmediateOrCancel),
            "{crossed:?}"
        );
    }

    /// Short of tokens, the budget's priority holds across the slots of one
    /// side: the shifting ladder's cancel goes first and its place waits,
    /// then goes on a later diff — and the order that waited is not
    /// superseded by the slot the cancel freed.
    #[test]
    fn a_shifting_ladder_short_of_tokens_pulls_before_it_places() {
        let mut oms = budgeted(1, 3);
        let placed = oms.diff(
            now(SEC),
            &[bids(
                &[("0.050", "10"), ("0.049", "10"), ("0.048", "10")],
                SEC,
            )],
        );
        assert_eq!(placed.len(), 3);
        ack_places(&mut oms, &placed);
        let top = bid_at(&oms, "0.050");

        let shifted = oms.diff(
            now(2 * SEC),
            &[bids(
                &[("0.049", "10"), ("0.048", "10"), ("0.047", "10")],
                2 * SEC,
            )],
        );
        assert_eq!(shifted.as_slice(), [Request::Cancel(top)]);
        assert_eq!(oms.pacing().waiting, 1, "the place waits");
        oms.apply(&[cancelled(top)]);

        let later = oms.diff(now(3 * SEC), &[]);
        assert!(
            matches!(later.as_slice(), [Request::Place(order)] if order.price == Some(px("0.047"))),
            "{later:?}"
        );
        assert_eq!(oms.pacing().superseded, 0, "{:?}", oms.pacing());
        assert_eq!(oms.pacing().waiting, 0);
        assert_eq!(oms.pacing().waiting, waiting_by_walk(&oms));
    }

    /// The burst is the same whatever order the slots were filled in:
    /// slots are not ranked, and the requests come out in the side's stated
    /// order — cancels worst first, amends and places best first.
    #[test]
    fn a_ladders_requests_come_out_in_a_stated_order_whatever_the_slots_hold() {
        // Two OMSs reach the same ladder through different slot orders.
        let mut straight = with_a_working_ladder();
        let bottom = bid_at(&straight, "0.048");
        let mut shuffled = oms();
        for (price, as_of) in [("0.048", 1_000), ("0.050", 1_001), ("0.049", 1_002)] {
            let placed = shuffled.diff(
                now(as_of),
                &[bids(
                    &[("0.050", "10"), ("0.049", "10"), ("0.048", "10")]
                        .into_iter()
                        .filter(|(p, _)| bid_prices(&shuffled).contains(&px(p)) || *p == price)
                        .collect::<Vec<_>>(),
                    as_of,
                )],
            );
            ack_places(&mut shuffled, &placed);
        }
        assert_eq!(bid_prices(&shuffled), bid_prices(&straight));
        assert_ne!(
            shuffled
                .slots(&call("60000"), Side::Bid)
                .map(|slot| slot.placed().map(|p| p.price)),
            straight
                .slots(&call("60000"), Side::Bid)
                .map(|slot| slot.placed().map(|p| p.price)),
            "the slots hold the ladder in different orders"
        );

        let next = bids(&[("0.0505", "10"), ("0.049", "3")], 2_000);
        let shape = |requests: &[Request]| -> Vec<(u8, Px, Qty)> {
            requests
                .iter()
                .map(|request| match request {
                    Request::Cancel(_) => (0, Px::ZERO, Qty::ZERO),
                    Request::Amend(amend) => (1, amend.price, amend.qty),
                    Request::Place(order) => (2, order.price.expect("a limit"), order.qty),
                    Request::CancelAll => (3, Px::ZERO, Qty::ZERO),
                })
                .collect()
        };
        let a = straight.diff(now(2_000), &[next]);
        let b = shuffled.diff(now(2_000), &[next]);
        assert_eq!(a[0], Request::Cancel(bottom));
        assert_eq!(shape(&a), shape(&b));
        assert_eq!(
            shape(&a),
            [
                (0, Px::ZERO, Qty::ZERO),
                (1, px("0.0505"), qty("10")),
                (1, px("0.049"), qty("3")),
            ],
            "0.048 is pulled, 0.050 moves up to 0.0505, 0.049 resizes"
        );
    }

    /// An ack at the venue's own tick answers its level, per order: the
    /// same decision again neither amends it back nor places the asked
    /// price beside it, and the rest of the ladder is untouched.
    #[test]
    fn an_adjusted_rung_answers_its_level() {
        let mut oms = oms();
        let want = bids(&[("0.050", "10"), ("0.049", "10")], 1_000);
        let placed = oms.diff(now(1_000), &[want]);
        let [Request::Place(top), Request::Place(behind)] = placed.as_slice() else {
            panic!("{placed:?}")
        };
        oms.apply(&[
            ack(top.id),
            Report::Ack(Ack {
                price: Some(px("0.0485")),
                ..ack(behind.id).into_ack()
            }),
        ]);
        assert_eq!(oms.adjusted(), 1);
        assert_eq!(bid_prices(&oms), [px("0.050"), px("0.0485")]);
        let again = oms.diff(now(1_100), &[want]);
        assert!(again.is_empty(), "{again:?}");
    }

    // Triggers (module docs).

    use crate::adapters::execution::edge::Fired;
    use crate::adapters::execution::order::{OrderKind, Reference, TriggerKind};

    /// A sell stop on the hedge leg: `level` resting untriggered under a
    /// stop at `trigger` on the mark, reduce-only, as of `as_of`.
    fn stop(level: Level, trigger: &str, as_of: u64) -> Desired {
        let trigger = Trigger::stop(Reference::Mark, px(trigger));
        Desired::stop(perp(), now(as_of), Side::Ask, trigger, level).reduce_only()
    }

    fn fired(id: ClientOrderId) -> Report {
        Report::Triggered(Fired {
            order: id,
            venue_id: VenueId::new("v2").unwrap(),
            venue_time: None,
            recv_time: now(1_002),
        })
    }

    /// The one triggered order on a side, wherever it sits.
    fn stop_on(oms: &Oms, side: Side) -> Slot {
        oms.triggered(&perp(), side)
            .into_iter()
            .find(|slot| slot.placed().is_some())
            .unwrap_or(Slot::Idle)
    }

    /// An OMS with the sell stop of [`stop`] acked, and its id.
    fn with_a_resting_stop() -> (Oms, ClientOrderId) {
        let mut oms = oms();
        let placed = oms.diff(now(1_000), &[stop(at("0.049", "1"), "0.05", 1_000)]);
        let [Request::Place(order)] = placed.as_slice() else {
            panic!("{placed:?}");
        };
        oms.apply(&[ack(order.id)]);
        (oms, order.id)
    }

    /// A stop goes out as a limit at its cap, resting under its trigger,
    /// reduce-only and good till cancelled — never post-only, which would
    /// refuse it on the instant it fired.
    #[test]
    fn a_stop_is_placed_as_a_limit_under_its_trigger() {
        let mut oms = oms();
        let placed = oms.diff(now(1_000), &[stop(at("0.049", "1"), "0.05", 1_000)]);
        let [Request::Place(order)] = placed.as_slice() else {
            panic!("{placed:?}");
        };
        assert_eq!(order.side, Side::Ask);
        assert_eq!(order.kind, OrderKind::Limit);
        assert_eq!(order.tif, TimeInForce::GoodTillCancel);
        assert_eq!(order.price, Some(px("0.049")));
        assert!(order.reduce_only);
        assert_eq!(
            order.trigger,
            Some(Trigger {
                kind: TriggerKind::Stop,
                reference: Reference::Mark,
                price: px("0.05"),
            })
        );
        order.validate().unwrap();
        assert!(matches!(stop_on(&oms, Side::Ask), Slot::Pending(_)));
        assert_eq!(
            oms.slot(&perp(), Side::Ask),
            Slot::Idle,
            "not a plain order"
        );
    }

    /// The point of a second remembered decision: the hedge working a leg
    /// and the stop on it are placed together, and a burst that changes one
    /// leaves the other alone.
    #[test]
    fn a_stop_and_the_hedge_on_one_leg_do_not_replace_each_other() {
        let (mut oms, id) = with_a_resting_stop();
        let hedge = Desired {
            as_of: now(1_100),
            ..want(perp(), Some(at("0.048", "2")), None)
        };
        let placed = oms.diff(now(1_100), &[hedge]);
        let [Request::Place(order)] = placed.as_slice() else {
            panic!("{placed:?}");
        };
        assert_eq!(order.trigger, None);
        oms.apply(&[ack(order.id)]);
        // The hedge withdrawn, the stop untouched — and the other way round.
        let withdrawn = oms.diff(now(1_200), &[Desired::nothing(perp(), now(1_200))]);
        assert!(
            matches!(withdrawn.as_slice(), [Request::Cancel(other)] if *other != id),
            "{withdrawn:?}"
        );
        let pulled = oms.diff(now(1_300), &[Desired::no_trigger(perp(), now(1_300))]);
        assert_eq!(pulled.as_slice(), [Request::Cancel(id)]);
    }

    /// Moving the trigger is an amend that restates it; the same decision
    /// again is nothing.
    #[test]
    fn moving_the_trigger_is_an_amend() {
        let (mut oms, id) = with_a_resting_stop();
        let same = oms.diff(now(1_100), &[stop(at("0.049", "1"), "0.05", 1_100)]);
        assert!(same.is_empty(), "{same:?}");
        let moved = oms.diff(now(1_200), &[stop(at("0.049", "1"), "0.0495", 1_200)]);
        assert_eq!(
            moved.as_slice(),
            [Request::Amend(Amend {
                order: id,
                instrument: perp(),
                price: px("0.049"),
                qty: qty("1"),
                trigger: Some(Trigger::stop(Reference::Mark, px("0.0495"))),
            })]
        );
        oms.apply(&[ack(id)]);
        let Slot::Working(resting) = stop_on(&oms, Side::Ask) else {
            panic!("{:?}", stop_on(&oms, Side::Ask));
        };
        assert_eq!(resting.trigger.map(|t| t.price), Some(px("0.0495")));
        // A change of reference is a different order: pulled, and placed
        // once the pull is answered — never both at once.
        let reference = Desired {
            intent: Intent::Trigger(Trigger::stop(Reference::Index, px("0.0495"))),
            ..stop(at("0.049", "1"), "0.0495", 1_300)
        };
        let pulled = oms.diff(now(1_300), &[reference]);
        assert_eq!(pulled.as_slice(), [Request::Cancel(id)]);
        let waiting = oms.diff(now(1_350), &[]);
        assert!(waiting.is_empty(), "never two stops at once: {waiting:?}");
        oms.apply(&[cancelled(id)]);
        let placed = oms.diff(now(1_400), &[]);
        assert!(
            matches!(placed.as_slice(), [Request::Place(order)]
                if order.trigger.map(|t| t.reference) == Some(Reference::Index)),
            "{placed:?}"
        );
    }

    /// Fired, the order answers the stop: left working, never amended back
    /// under a trigger, and not re-armed when it fills away — nor by the
    /// fresh decisions a strategy makes every tick restating the same stop.
    /// A decision for a different stop places one.
    #[test]
    fn a_fired_stop_is_left_to_trade_and_not_re_armed() {
        let (mut oms, id) = with_a_resting_stop();
        oms.apply(&[fired(id)]);
        let Slot::Working(resting) = stop_on(&oms, Side::Ask) else {
            panic!("{:?}", stop_on(&oms, Side::Ask));
        };
        assert_eq!(resting.trigger, None, "a plain order now");
        let still = oms.diff(now(1_100), &[stop(at("0.049", "1"), "0.05", 1_000)]);
        assert!(still.is_empty(), "{still:?}");
        // The strategy restates the same stop, tick after tick, before it
        // has seen the fill: the fired order is not pulled.
        let restated = oms.diff(now(1_150), &[stop(at("0.049", "1"), "0.05", 1_150)]);
        assert!(restated.is_empty(), "{restated:?}");

        oms.apply(&[fill(id, "1", "0")]);
        let filled = oms.diff(now(1_200), &[stop(at("0.049", "1"), "0.05", 1_200)]);
        assert!(
            filled.is_empty(),
            "the stop that fired is not re-armed: {filled:?}"
        );

        let other = oms.diff(now(1_300), &[stop(at("0.048", "1"), "0.049", 1_300)]);
        assert!(
            matches!(other.as_slice(), [Request::Place(order)]
                if order.trigger.map(|t| t.price) == Some(px("0.049"))),
            "{other:?}"
        );
    }

    /// A decision for a different stop pulls the fired order — never
    /// amended — and places the new stop once the pull is answered, never
    /// beside it. With nothing wanted, the fired order is pulled alone.
    #[test]
    fn a_newer_decision_pulls_a_fired_order() {
        let (mut oms, id) = with_a_resting_stop();
        oms.apply(&[fired(id)]);
        let newer = oms.diff(now(1_100), &[stop(at("0.048", "1"), "0.049", 1_100)]);
        assert_eq!(newer.as_slice(), [Request::Cancel(id)]);
        let waiting = oms.diff(now(1_150), &[]);
        assert!(
            waiting.is_empty(),
            "nothing beside the order being pulled: {waiting:?}"
        );
        oms.apply(&[cancelled(id)]);
        let placed = oms.diff(now(1_200), &[]);
        let [Request::Place(order)] = placed.as_slice() else {
            panic!("{placed:?}");
        };
        assert_eq!(order.trigger.map(|t| t.price), Some(px("0.049")));

        let (mut oms, id) = with_a_resting_stop();
        oms.apply(&[fired(id)]);
        let none = oms.diff(now(1_100), &[Desired::no_trigger(perp(), now(1_100))]);
        assert_eq!(none.as_slice(), [Request::Cancel(id)]);
    }

    /// A fill is a firing the venue did not announce: a partly filled stop
    /// is not amended back up under its trigger, which the venue would
    /// refuse on every diff.
    #[test]
    fn a_fill_on_a_stop_says_it_fired() {
        let (mut oms, id) = with_a_resting_stop();
        oms.apply(&[fill(id, "0.4", "0.6")]);
        let Slot::Working(resting) = stop_on(&oms, Side::Ask) else {
            panic!("{:?}", stop_on(&oms, Side::Ask));
        };
        assert_eq!((resting.trigger, resting.remaining), (None, qty("0.6")));
        let again = oms.diff(now(1_100), &[stop(at("0.049", "1"), "0.05", 1_000)]);
        assert!(again.is_empty(), "{again:?}");
    }

    /// One level on one side: a deeper stop, or a stop both ways, is
    /// refused and counted, and nothing is placed.
    #[test]
    fn a_deep_or_two_sided_stop_is_refused() {
        let mut oms = oms();
        let deep = Desired {
            asks: ladder(Side::Ask, &[("0.049", "1"), ("0.050", "1")]),
            ..stop(at("0.049", "1"), "0.05", 1_000)
        };
        let both = Desired {
            instrument: call("60000"),
            bids: Ladder::one(at("0.040", "1")),
            ..stop(at("0.049", "1"), "0.05", 1_000)
        };
        let placed = oms.diff(now(1_000), &[deep, both]);
        assert!(placed.is_empty(), "{placed:?}");
        assert_eq!(
            oms.refused(),
            3,
            "the deep side, and each side of the two-sided one"
        );
    }

    /// A stale stop is withdrawn like any other decision, and a cancel-all
    /// pulls stops with everything else.
    #[test]
    fn a_stale_stop_is_pulled_and_a_cancel_all_takes_stops() {
        let (mut oms, id) = with_a_resting_stop();
        let stale = oms.diff(now(1_000) + Duration::from_secs(3601), &[]);
        assert_eq!(stale.as_slice(), [Request::Cancel(id)]);

        let (mut oms, id) = with_a_resting_stop();
        assert_eq!(oms.cancel_all(now(1_100)), Some(Request::CancelAll));
        assert!(matches!(stop_on(&oms, Side::Ask), Slot::PendingCancel(_)));
        oms.apply(&[cancelled(id)]);
        assert!(oms.diff(now(1_200), &[]).is_empty());
        assert_eq!(stop_on(&oms, Side::Ask), Slot::Idle);
    }

    /// A stop that moves to the other side waits for the first to be pulled:
    /// a sell stop and a buy stop never rest together.
    #[test]
    fn a_stop_that_changes_side_waits_for_the_pull() {
        let (mut oms, id) = with_a_resting_stop();
        let flipped = Desired {
            bids: Ladder::one(at("0.051", "1")),
            asks: Ladder::none(),
            ..stop(at("0.049", "1"), "0.05", 1_100)
        };
        let pulled = oms.diff(now(1_100), &[flipped]);
        assert_eq!(pulled.as_slice(), [Request::Cancel(id)]);
        oms.apply(&[cancelled(id)]);
        let placed = oms.diff(now(1_200), &[]);
        assert!(
            matches!(placed.as_slice(), [Request::Place(order)] if order.side == Side::Bid),
            "{placed:?}"
        );
    }
}
