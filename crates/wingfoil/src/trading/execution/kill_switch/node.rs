//! The kill switch as a graph node: [`SwitchOp`], wired through
//! [`KillSwitchOps`].
//!
//! ```
//! use std::time::Duration;
//! use wingfoil::prelude::*;
//! use wingfoil::trading::execution::kill_switch::{
//!     Assessed, Breaches, KillSwitchOps, Limit, Stance, StanceOps, within,
//! };
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq)]
//! enum Risk { Gross }
//!
//! impl Limit for Risk {
//!     const ALL: &'static [Risk] = &[Risk::Gross];
//! }
//!
//! /// The caller's reading: gross exposure, and the caller's cap over it.
//! fn halted(gross: &Stream<Burst<f64>>, clear: &Stream<()>) -> Stream<Stance> {
//!     let assessed: Stream<Assessed<Risk>> =
//!         gross.kill_switch(clear, Duration::ZERO, None, |_, _, gross: &f64| {
//!             let mut standing = Breaches::NONE;
//!             if !within(*gross, 1_000_000.0) {
//!                 standing.set(Risk::Gross);
//!             }
//!             standing
//!         });
//!     assessed.stance()
//! }
//! ```
//!
//! [`Switch`] has two mutating entry points — [`record`](Switch::record) and
//! [`clear`](Switch::clear) — and the order they are called in within one
//! instant is part of what the latch means. It is the node's, stated once:
//!
//! 1. **readings** — for each reading in the burst, in order, the caller's
//!    `assess` (its caps over its own reading type, handed the switch for
//!    [`Switch::daily_loss`]) and then [`Switch::record`] of what it found.
//!    Every reading, never the latest: a breach and a reading back inside
//!    in one instant is a book that went through its limit, and keeping
//!    only the second would never latch;
//! 2. **clear** — the operator's [`Switch::clear`], after the readings, so
//!    a clear is judged against what stands *now*: a clear and a breach in
//!    one instant is refused. A refusal is ordinary — the switch meaning
//!    what it appears to — so it is carried in the output
//!    ([`Assessed::cleared`]), never raised as a run error.
//!
//! Then the [`Assessed`] reading of the switch, every cycle the node runs.
//!
//! # A restart, and the first cycle
//!
//! `Cfg` carries the day start and the [`Snapshot`] a restarted process
//! read back, if any; the switch is built from them — [`Switch::restore`] or
//! [`Switch::new`] — on the node's first cycle, as the OMS's and the FIX
//! bridge's state is. The `/new-op` recipe's "a latch that starts on needs a
//! `start` hook" is about state a cycle *reads* before anything has set it;
//! nothing reads this switch before its first cycle, and the stream's slot
//! before then is [`Assessed::default`], whose stance is
//! [`Stance::Halted`]. A `start` hook would also have to anchor the reading
//! type and the `assess` closure in its forwarder, which a `start` takes no
//! input to infer from.
//!
//! # Stance, never `bool`
//!
//! Every edge out of here carries a [`Stance`], whose default is
//! [`Halted`](Stance::Halted) deliberately: a reader that sees a filler
//! value, before the switch has assessed anything, reads it halted. A `bool`
//! defaults to `false`, and whichever way round it was named, one of the
//! two readings of `false` is permission nobody computed.
//! [`StanceOps::stance`] is the stance alone, for a quoter to read
//! passively and a cancel-all to key on.
//!
//! A graph drives the kill switch through [`KillSwitchOps`], and only
//! through it. The pure [`Switch`] stays for tests and for tools that are
//! not graphs.

use std::marker::PhantomData;
use std::time::Duration;

use crate::op::{Activation, Ctx, Op, Tick};
use crate::prelude::*;
use anyhow::Result;

use super::{Breaches, ClearError, Limit, Snapshot, Stance, Switch};
use crate::NanoTime;
use crate::trading::market::Qty;

/// What the switch said after one cycle of [`SwitchOp`]. `Copy`, so it rides
/// an edge like the rest of the execution vocabulary.
///
/// The default — the slot before the node's first cycle — is halted, with
/// nothing standing or latched: inert, not open.
#[derive(Debug, PartialEq, Eq)]
pub struct Assessed<L: Limit> {
    /// What the strategy may do: [`Switch::stance`].
    pub stance: Stance,
    /// What was out of limit at the last assessment: [`Switch::standing`].
    pub standing: Breaches<L>,
    /// Every limit the latch holds: [`Switch::latched`].
    pub latched: Breaches<L>,
    /// When the latch was set, where it is: the latch's `at`.
    pub latched_since: Option<NanoTime>,
    /// The equity the day opened at, once a day is anchored:
    /// [`Switch::opening_equity`].
    pub opening_equity: Option<Qty>,
    /// The operator's clear, where one was asked for this cycle: `Ok` where
    /// it cleared, the refusal and what stands where it did not. A refusal
    /// may name a limit `standing` does not show — one a reading earlier in
    /// this same cycle's burst found, which `latched` holds.
    pub cleared: Option<Result<(), ClearError<L>>>,
    /// What a restart carries from here: [`Switch::snapshot`].
    pub snapshot: Snapshot<L>,
}

// Written out rather than derived: a derive would ask `L` for `Default`,
// and `Assessed` is a plain value whatever its limits are.
impl<L: Limit> Clone for Assessed<L> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<L: Limit> Copy for Assessed<L> {}

impl<L: Limit> Default for Assessed<L> {
    fn default() -> Self {
        Assessed {
            stance: Stance::default(),
            standing: Breaches::NONE,
            latched: Breaches::NONE,
            latched_since: None,
            opening_equity: None,
            cleared: None,
            snapshot: Snapshot::default(),
        }
    }
}

impl<L: Limit> Assessed<L> {
    /// The switch as it stands, and what this cycle's clear answered.
    fn of(switch: &Switch<L>, cleared: Option<Result<(), ClearError<L>>>) -> Assessed<L> {
        Assessed {
            stance: switch.stance(),
            standing: switch.standing(),
            latched: switch.latched(),
            latched_since: switch.latch().map(|latch| latch.at),
            opening_equity: switch.opening_equity(),
            cleared,
            snapshot: switch.snapshot(),
        }
    }
}

/// The kill switch as an op: a [`Switch`] held as node state, fed the
/// caller's readings and then the operator's clear in the fixed order the
/// [module docs](self) state, emitting what it said.
///
/// Wire it with [`KillSwitchOps`]; the op itself is public so a compiled
/// graph can name it.
///
/// - `Cfg` — the day start ([`Switch::new`]), the [`Snapshot`] a restarted
///   process carries in, if any, and `assess`, the caller's caps over one
///   reading.
/// - `State` — the [`Switch`], built or restored on the first cycle.
/// - `Out` — the [`Assessed`], every cycle the node runs.
pub struct SwitchOp<L, T, F>(PhantomData<(L, T, F)>);

#[crate::op(build = switch_op, alias = kill_switch)]
impl<L, T, F> Op for SwitchOp<L, T, F>
where
    L: Limit,
    T: Clone + Default + 'static,
    F: Fn(&mut Switch<L>, NanoTime, &T) -> Breaches<L> + 'static,
{
    type Cfg = (Duration, Option<Snapshot<L>>, F);
    type State = Option<Switch<L>>;
    type In<'a> = ((&'a Burst<T>, bool), (&'a (), bool));
    type Out = Assessed<L>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut (Duration, Option<Snapshot<L>>, F),
        state: &mut Option<Switch<L>>,
        input: Self::In<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Assessed<L>>> {
        let (day_start, restored, assess) = cfg;
        let switch = state.get_or_insert_with(|| match restored {
            Some(snapshot) => Switch::restore(*day_start, *snapshot),
            None => Switch::new(*day_start),
        });
        let ((readings, read), (_, clear)) = input;
        let now = ctx.time();
        if read {
            for reading in readings {
                let standing = assess(switch, now, reading);
                switch.record(now, standing);
            }
        }
        let cleared = clear.then(|| switch.clear(now));
        Ok(Tick::Value(Assessed::of(switch, cleared)))
    }
}

/// The kill switch, wired on a stream of the caller's own readings. Out of
/// any prelude: `use wingfoil::trading::execution::kill_switch::KillSwitchOps;`.
///
/// Two inputs, applied within one instant in this order whichever of them
/// ticked:
///
/// 1. `self` — the readings, each assessed by `assess` and recorded, in
///    order;
/// 2. `clear` — the operator's clear, judged against what stands after
///    them. Refused while a breach stands; the refusal is in the output.
///
/// Returns the [`Assessed`] reading every cycle the node runs.
pub trait KillSwitchOps<T: Clone + Default + 'static> {
    /// The switch over these readings: days start `day_start` into the UTC
    /// day, a restarted process carries `restored` in, and `assess` is the
    /// caller's caps over one reading at the engine time — handed the
    /// switch for [`Switch::daily_loss`]. Read every cap through
    /// [`within`](super::within), and set the limit an input it could not
    /// evaluate needed: a cap nobody evaluated has not been respected.
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    fn kill_switch<L, F>(
        &self,
        clear: &Stream<()>,
        day_start: Duration,
        restored: Option<Snapshot<L>>,
        assess: F,
    ) -> Stream<Assessed<L>>
    where
        L: Limit,
        F: Fn(&mut Switch<L>, NanoTime, &T) -> Breaches<L> + 'static;
}

impl<T: Clone + Default + 'static> KillSwitchOps<T> for Stream<Burst<T>> {
    fn kill_switch<L, F>(
        &self,
        clear: &Stream<()>,
        day_start: Duration,
        restored: Option<Snapshot<L>>,
        assess: F,
    ) -> Stream<Assessed<L>>
    where
        L: Limit,
        F: Fn(&mut Switch<L>, NanoTime, &T) -> Breaches<L> + 'static,
    {
        let clear = clear.handle();
        self.wire(move |b, readings| b.switch_op(readings, clear, (day_start, restored, assess)))
    }
}

/// The stance alone, off the switch's [`Assessed`] stream. Out of any
/// prelude: `use wingfoil::trading::execution::kill_switch::StanceOps;`.
pub trait StanceOps {
    /// What the strategy may do, every cycle the switch runs — a
    /// [`Stance`], never a `bool`, so a reader that sees the filler before
    /// the first assessment reads it [`Halted`](Stance::Halted).
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    fn stance(&self) -> Stream<Stance>;
}

impl<L: Limit> StanceOps for Stream<Assessed<L>> {
    fn stance(&self) -> Stream<Stance> {
        self.map(|assessed: &Assessed<L>| assessed.stance)
    }
}

#[cfg(test)]
mod tests {
    use crate::trading::execution::kill_switch::within;
    use crate::{RunFor, RunMode};

    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Risk {
        Gross,
        DailyLoss,
    }

    impl Limit for Risk {
        const ALL: &'static [Risk] = &[Risk::Gross, Risk::DailyLoss];
    }

    /// The caller's reading: gross exposure and the account's equity.
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    struct Reading {
        gross: f64,
        equity: Option<Qty>,
    }

    const SEC: u64 = 1_000_000_000;

    fn t(secs: u64) -> NanoTime {
        NanoTime::new(secs * SEC)
    }

    /// A million gross, and 2% of the day's opening equity.
    fn assess(switch: &mut Switch<Risk>, now: NanoTime, reading: &Reading) -> Breaches<Risk> {
        let mut standing = Breaches::NONE;
        if !within(reading.gross, 1_000_000.0) {
            standing.set(Risk::Gross);
        }
        if !switch
            .daily_loss(now, reading.equity)
            .is_some_and(|(fraction, _)| fraction <= 0.02)
        {
            standing.set(Risk::DailyLoss);
        }
        standing
    }

    fn reading(gross: f64, equity: &str) -> Reading {
        Reading {
            gross,
            equity: Some(Qty::parse(equity).unwrap()),
        }
    }

    fn breaches(limits: &[Risk]) -> Breaches<Risk> {
        limits.iter().copied().collect()
    }

    /// One run of the switch over `readings` and `clears`, from `restored`.
    fn run(
        readings: Vec<(Reading, u64)>,
        clears: Vec<u64>,
        restored: Option<Snapshot<Risk>>,
    ) -> (Vec<(NanoTime, Assessed<Risk>)>, Vec<(NanoTime, Stance)>) {
        let g = GraphBuilder::new();
        let readings = g.replay(readings.into_iter().map(|(reading, at)| (reading, t(at))));
        let clear = g
            .replay(clears.into_iter().map(|at| ((), t(at))))
            .map(|_: &Burst<()>| ());
        let assessed = readings.kill_switch(&clear, Duration::ZERO, restored, assess);
        let stance = assessed.stance().with_time().accumulate();
        let assessed = assessed.with_time().accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        (runner.value(&assessed), runner.value(&stance))
    }

    /// A breach and the operator's clear in one instant: the reading is
    /// recorded first, so the clear is refused against what stands — in the
    /// output, not as a run error. Once a reading finds the book inside,
    /// the latch outlives it until a clear, which then succeeds.
    #[test]
    fn a_clear_in_the_instant_of_a_breach_is_refused_and_succeeds_once_inside() {
        let (assessed, stance) = run(
            vec![
                (reading(500_000.0, "100000"), 1),
                (reading(1_500_000.0, "100000"), 2),
                (reading(500_000.0, "100000"), 3),
            ],
            vec![2, 4],
            None,
        );
        let gross = breaches(&[Risk::Gross]);
        let at: Vec<_> = assessed.iter().map(|(at, _)| *at).collect();
        assert_eq!(at, [t(1), t(2), t(3), t(4)]);

        let (_, first) = assessed[0];
        assert_eq!(first.stance, Stance::Open);
        assert_eq!(first.opening_equity, Some(Qty::parse("100000").unwrap()));

        let (_, breached) = assessed[1];
        assert_eq!(breached.stance, Stance::Halted);
        assert_eq!(breached.standing, gross);
        assert_eq!(breached.latched, gross);
        assert_eq!(breached.latched_since, Some(t(2)));
        assert_eq!(
            breached.cleared,
            Some(Err(ClearError::StillBreached(gross)))
        );

        let (_, inside) = assessed[2];
        assert_eq!(
            (
                inside.stance,
                inside.standing,
                inside.latched,
                inside.cleared
            ),
            (Stance::Halted, Breaches::NONE, gross, None)
        );

        let (_, cleared) = assessed[3];
        assert_eq!(
            (cleared.stance, cleared.latched, cleared.cleared),
            (Stance::Open, Breaches::NONE, Some(Ok(())))
        );
        assert_eq!(cleared.latched_since, None);

        assert_eq!(
            stance,
            vec![
                (t(1), Stance::Open),
                (t(2), Stance::Halted),
                (t(3), Stance::Halted),
                (t(4), Stance::Open),
            ]
        );
    }

    /// A breach and a reading back inside, in one burst: both are recorded
    /// in order, so the switch latches and nothing stands. Keeping only the
    /// latest reading would never have latched.
    #[test]
    fn every_reading_in_a_burst_is_recorded_in_order() {
        let (assessed, _) = run(
            vec![
                (reading(1_500_000.0, "100000"), 1),
                (reading(500_000.0, "100000"), 1),
            ],
            vec![],
            None,
        );
        assert_eq!(assessed.len(), 1);
        let (at, a) = assessed[0];
        assert_eq!(at, t(1));
        assert_eq!(
            (a.stance, a.standing, a.latched, a.latched_since),
            (
                Stance::Halted,
                Breaches::NONE,
                breaches(&[Risk::Gross]),
                Some(t(1))
            )
        );
    }

    /// A breach, a reading back inside, and the operator's clear, all in one
    /// instant: the clear is refused on what the burst found, so the cycle
    /// the book went through its limit never reads as open. The clear a
    /// reading later succeeds.
    #[test]
    fn a_clear_in_the_instant_of_a_burst_that_breached_is_refused() {
        let (assessed, stance) = run(
            vec![
                (reading(1_500_000.0, "100000"), 1),
                (reading(500_000.0, "100000"), 1),
                (reading(500_000.0, "100000"), 2),
            ],
            vec![1, 2],
            None,
        );
        assert_eq!(assessed.len(), 2);
        let (at, a) = assessed[0];
        assert_eq!(at, t(1));
        assert_eq!(a.stance, Stance::Halted);
        assert_eq!(a.standing, Breaches::NONE);
        assert_eq!(
            a.cleared,
            Some(Err(ClearError::StillBreached(breaches(&[Risk::Gross]))))
        );
        let (at, a) = assessed[1];
        assert_eq!(at, t(2));
        assert_eq!((a.stance, a.cleared), (Stance::Open, Some(Ok(()))));
        assert_eq!(stance, vec![(t(1), Stance::Halted), (t(2), Stance::Open)]);
    }

    /// A restart from the snapshot carries the latch, what stood and the
    /// day's opening: a clear before any reading is refused on what stood,
    /// the first reading inside leaves it latched, and only then does a
    /// clear succeed. The day's loss is measured against the carried open.
    #[test]
    fn a_restore_carries_the_latch() {
        let (first, _) = run(
            vec![
                (reading(500_000.0, "100000"), 1),
                (reading(500_000.0, "97000"), 2),
            ],
            vec![],
            None,
        );
        let (_, last) = *first.last().unwrap();
        let loss = breaches(&[Risk::DailyLoss]);
        assert_eq!(last.latched, loss);
        let snapshot = last.snapshot;

        let (assessed, stance) = run(
            vec![(reading(500_000.0, "99000"), 11)],
            vec![10, 12],
            Some(snapshot),
        );
        let at: Vec<_> = assessed.iter().map(|(at, _)| *at).collect();
        assert_eq!(at, [t(10), t(11), t(12)]);

        let (_, refused) = assessed[0];
        assert_eq!(refused.cleared, Some(Err(ClearError::StillBreached(loss))));
        assert_eq!(refused.latched_since, Some(t(2)));
        assert_eq!(refused.opening_equity, Some(Qty::parse("100000").unwrap()));

        let (_, inside) = assessed[1];
        assert_eq!(
            (inside.stance, inside.standing, inside.latched),
            (Stance::Halted, Breaches::NONE, loss)
        );

        let (_, cleared) = assessed[2];
        assert_eq!(cleared.cleared, Some(Ok(())));
        assert_eq!(
            stance,
            vec![
                (t(10), Stance::Halted),
                (t(11), Stance::Halted),
                (t(12), Stance::Open),
            ]
        );
    }

    /// Before the first assessment the stance is the filler, and the filler
    /// is halted.
    #[test]
    fn the_default_is_halted() {
        assert_eq!(Assessed::<Risk>::default().stance, Stance::Halted);
    }
}
