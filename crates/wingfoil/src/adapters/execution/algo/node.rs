//! The parents as a graph node: [`AlgoOp`], wired through [`AlgoOps`].
//!
//! [`Algos`] has three mutating entry points — [`apply`](Algos::apply),
//! [`start`](Algos::start) and [`decide`](Algos::decide) — and, as with the
//! OMS, the order they are called in within one instant is part of what they
//! mean. It is the node's, stated once:
//!
//! 1. **reports** — [`Algos::apply`], the whole burst in order, so a fill
//!    lands on the parent that was live when it happened;
//! 2. **parents** — [`Algos::start`], each in burst order, a later one on an
//!    instrument replacing an earlier;
//! 3. **clock** — a beat: it renews an iceberg's `as_of`, and runs the
//!    node with nothing new;
//!
//! then [`Algos::decide`] at the engine time, on **every** cycle the node
//! runs — the clock's included, which is what brings a TWAP clip due and
//! keeps an iceberg fresh against the OMS's `max_desired_age`.
//!
//! Its desireds go to the OMS node's desired input. The reports it reads
//! are the OMS's own — the venue's, the previous instant's requests
//! answered — so it sits on the forward side of the one feedback cut and
//! adds none.

use std::marker::PhantomData;

use crate::op::{Activation, Ctx, Op, Tick};
use crate::prelude::*;
use anyhow::Result;

use super::{Algos, Parent, Progress};
use crate::adapters::execution::edge::Report;
use crate::adapters::execution::oms::Desired;
use crate::adapters::execution::order::Instrument;

/// The algos as an op: an [`Algos`] held as node state, fed its three
/// inputs in the fixed order the [module docs](self) state, emitting the
/// instant's desireds and every parent's progress that moved.
///
/// Wire it with [`AlgoOps`]; the op itself is public so a compiled graph can
/// name it.
///
/// - `State` — the [`Algos`].
/// - `Out` — `(desireds, progress)`, every cycle the node runs; either burst
///   may be empty.
pub struct AlgoOp<I>(PhantomData<I>);

#[crate::op(build = algo_op)]
impl<I> Op for AlgoOp<I>
where
    I: Instrument + 'static,
{
    type Cfg = ();
    type State = Algos<I>;
    type In<'a> = (
        (&'a Burst<Report<I>>, bool),
        (&'a Burst<Parent<I>>, bool),
        (&'a (), bool),
    );
    type Out = (Burst<Desired<I>>, Burst<Progress<I>>);
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        algos: &mut Algos<I>,
        input: Self::In<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<(Burst<Desired<I>>, Burst<Progress<I>>)>> {
        let ((reports, reported), (parents, arrived), (_, beat)) = input;
        let now = ctx.time();
        let mut progress = Burst::new();
        if reported {
            algos.apply(reports, now, &mut progress);
        }
        if arrived {
            for parent in parents {
                algos.start(*parent, now, &mut progress);
            }
        }
        let desired = algos.decide(now, beat, &mut progress);
        Ok(Tick::Value((desired, progress)))
    }
}

/// The parents node, wired on a stream of parents. Out of any prelude:
/// `use wingfoil::adapters::execution::algo::AlgoOps;`.
///
/// Three inputs, applied within one instant in this order whichever of them
/// ticked:
///
/// 1. `reports` — the venue's reports, the OMS's own stream: every fill on a
///    live parent's instrument and side is that parent's;
/// 2. `self` — new parents, each replacing whatever is live on its
///    instrument;
/// 3. `clock` — a beat: the decide runs on it, which is what sends a TWAP
///    clip when it falls due and renews an iceberg's `as_of`.
///
/// The clock is the caller's contract, and nothing here checks it: it has
/// to beat at least as often as the finest schedule wanted, inside the
/// OMS's `max_desired_age` for an iceberg to stay shown, and at or after a
/// parent's `by` for it to lapse — a parent only ends on a cycle.
///
/// Returns the desireds — for the OMS node's desired input, ticking only on
/// a cycle that decided any — and the progress of every parent that
/// started, filled, sent a clip or ended, ticking only when one did.
pub trait AlgoOps<I: Instrument + 'static> {
    /// The parents node: see the trait docs for the inputs and the order
    /// they are applied in.
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    fn algo(
        &self,
        reports: &Stream<Burst<Report<I>>>,
        clock: &Stream<()>,
    ) -> (Stream<Burst<Desired<I>>>, Stream<Burst<Progress<I>>>);
}

impl<I: Instrument + 'static> AlgoOps<I> for Stream<Burst<Parent<I>>> {
    fn algo(
        &self,
        reports: &Stream<Burst<Report<I>>>,
        clock: &Stream<()>,
    ) -> (Stream<Burst<Desired<I>>>, Stream<Burst<Progress<I>>>) {
        let (reports, clock) = (reports.handle(), clock.handle());
        let node = self.wire(move |b, parents| b.algo_op(reports, parents, clock));
        let desired = node.filter_map(|(desired, _): &(Burst<Desired<I>>, Burst<Progress<I>>)| {
            (!desired.is_empty()).then(|| desired.clone())
        });
        let progress =
            node.filter_map(|(_, progress): &(Burst<Desired<I>>, Burst<Progress<I>>)| {
                (!progress.is_empty()).then(|| progress.clone())
            });
        (desired, progress)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::adapters::market::{Level, Px, Qty, Side};
    use crate::{NanoTime, RunFor, RunMode};

    use super::*;
    use crate::adapters::execution::algo::Status;
    use crate::adapters::execution::edge::{Ack, Request, Retired};
    use crate::adapters::execution::exec_id::{ExecId, VenueId};
    use crate::adapters::execution::oms::{
        Config, Intent, Ladder, Lifetime, OmsOps, Pacing, Passive,
    };
    use crate::adapters::execution::order::{ClientOrderId, Epoch, Fill, Liquidity};
    use crate::adapters::execution::rate_limit::{OrderRate, Terms};

    /// A venue listing tickers by number.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
    struct Ticker(u8);

    const A: Ticker = Ticker(1);
    const B: Ticker = Ticker(2);

    type Desired = crate::adapters::execution::oms::Desired<Ticker>;
    type Parent = crate::adapters::execution::algo::Parent<Ticker>;
    type Report = crate::adapters::execution::edge::Report<Ticker>;

    const SECOND: u64 = 1_000_000_000;

    /// Nothing here waits on a token.
    const RATE: OrderRate = match OrderRate::new(
        Terms {
            rate: 50,
            burst: 100,
        },
        Terms { rate: 5, burst: 20 },
        0.8,
    ) {
        Ok(rate) => rate,
        Err(_) => panic!("valid"),
    };

    fn config() -> Config {
        Config {
            max_desired_age: Duration::from_secs(5),
            min_requote: Px::ZERO,
            retake: Duration::from_millis(100),
            rate: RATE,
            passive: Passive::PostOnly,
            ratio: None,
            lifetime: Lifetime::GoodTillCancel,
        }
    }

    fn t(nanos: u64) -> NanoTime {
        NanoTime::new(nanos)
    }

    fn q(text: &str) -> Qty {
        Qty::parse(text).unwrap()
    }

    fn px(text: &str) -> Px {
        Px::parse(text).unwrap()
    }

    fn id(n: u32) -> ClientOrderId {
        ClientOrderId::new(Epoch::ZERO, n)
    }

    fn fill(order: ClientOrderId, side: Side, qty: &str, remaining: &str, at: u64) -> Report {
        Report::Fill(Fill {
            order,
            exec_id: ExecId::new("e").unwrap(),
            instrument: A,
            side,
            qty: q(qty),
            filled: q(qty),
            remaining: q(remaining),
            price: px("100"),
            liquidity: Liquidity::Taker,
            recv_time: t(at),
            ..Default::default()
        })
    }

    fn killed(order: ClientOrderId, remaining: &str, at: u64) -> Report {
        Report::Cancelled(Retired {
            order,
            remaining: q(remaining),
            recv_time: t(at),
            ..Retired::default()
        })
    }

    fn ack(order: ClientOrderId, at: u64) -> Report {
        Report::Ack(Ack {
            order,
            venue_id: VenueId::new("v").unwrap(),
            recv_time: t(at),
            ..Ack::default()
        })
    }

    /// What one run is fed, each row at its engine instant.
    #[derive(Clone, Default)]
    struct Script {
        parents: Vec<(Parent, u64)>,
        reports: Vec<(Report, u64)>,
        clock: Vec<u64>,
    }

    /// What one run emitted, each with the instant it ticked at.
    #[derive(Debug, PartialEq)]
    struct Run {
        desired: Vec<(NanoTime, Burst<Desired>)>,
        progress: Vec<(NanoTime, Burst<Progress<Ticker>>)>,
        requests: Vec<(NanoTime, Burst<Request<Ticker>>)>,
    }

    fn rows<T: Clone + Default + 'static>(
        g: &GraphBuilder,
        rows: Vec<(T, u64)>,
    ) -> Stream<Burst<T>> {
        g.replay_results(rows.into_iter().map(|(row, at)| Ok((row, t(at)))))
    }

    /// The parents node into an OMS, scripted reports standing in for the
    /// venue: what the parents decide, and what the OMS then sends.
    fn run(script: Script) -> Run {
        let g = GraphBuilder::new();
        let reports = rows(&g, script.reports);
        let parents = rows(&g, script.parents);
        let clock =
            rows(&g, script.clock.into_iter().map(|at| ((), at)).collect()).map(|_: &Burst<()>| ());
        let (desired, progress) = parents.algo(&reports, &clock);
        let (requests, _pacing): (_, Stream<Pacing>) = desired
            .wire_oms(config(), Epoch::ZERO, &reports)
            .sweep(&clock)
            .build();
        let desired = desired.with_time().accumulate();
        let progress = progress.with_time().accumulate();
        let requests = requests.with_time().accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        Run {
            desired: runner.value(&desired),
            progress: runner.value(&progress),
            requests: runner.value(&requests),
        }
    }

    fn cross(side: Side, qty: &str, as_of: u64) -> Desired {
        let mut want = Desired::nothing(A, t(as_of));
        let level = Ladder::one(Level::new(px("101"), q(qty)));
        match side {
            Side::Bid => want.bids = level,
            Side::Ask => want.asks = level,
        }
        want.intent = Intent::Cross;
        want
    }

    /// `(id, side, qty)` of a place, or a panic naming
    /// what it was.
    fn place(request: &Request<Ticker>) -> (ClientOrderId, Side, Qty) {
        match request {
            Request::Place(order) => (order.id, order.side, order.qty),
            other => panic!("expected a place, got {other:?}"),
        }
    }

    /// A TWAP of four clips over four seconds on a clock of one a second,
    /// against a venue that fills each IOC in part and kills the rest.
    fn twap_script() -> Script {
        let parent = Parent::twap(A, Side::Bid, q("8"), px("101"), t(4 * SECOND), 4).unwrap();
        Script {
            parents: vec![(parent, 0)],
            reports: vec![
                // Clip one: 2 asked, 1 filled, the rest killed.
                (fill(id(1), Side::Bid, "1", "1", 10), 10),
                (killed(id(1), "1", 11), 11),
                // Clip two: 3 asked (the carry), all filled.
                (fill(id(2), Side::Bid, "3", "0", SECOND + 10), SECOND + 10),
                // Clip three: 2 asked, killed unfilled.
                (killed(id(3), "2", 2 * SECOND + 10), 2 * SECOND + 10),
            ],
            clock: vec![SECOND, 2 * SECOND, 3 * SECOND, 4 * SECOND],
        }
    }

    /// Each clip crosses the schedule less what has filled, at the engine
    /// time it falls due; the OMS sends each as one IOC; what is unfilled at
    /// the deadline is reported and the run is left with nothing wanted.
    #[test]
    fn a_twap_crosses_its_clips_and_carries_what_the_cap_refused() {
        let run = run(twap_script());

        let desired: Vec<_> = run
            .desired
            .iter()
            .map(|(at, burst)| (*at, burst.to_vec()))
            .collect();
        assert_eq!(
            desired,
            [
                (t(0), vec![cross(Side::Bid, "2", 0)]),
                // The fill re-sizes the live clip at its own `as_of`.
                (t(10), vec![cross(Side::Bid, "1", 0)]),
                (t(SECOND), vec![cross(Side::Bid, "3", SECOND)]),
                (t(SECOND + 10), vec![Desired::nothing(A, t(SECOND))]),
                (t(2 * SECOND), vec![cross(Side::Bid, "2", 2 * SECOND)]),
                (t(3 * SECOND), vec![cross(Side::Bid, "4", 3 * SECOND)]),
                (t(4 * SECOND), vec![Desired::nothing(A, t(4 * SECOND))]),
            ]
        );

        // One IOC a clip, never a second inside one: the re-size at 10 is
        // the same decision, which the OMS has already crossed.
        let places: Vec<_> = run
            .requests
            .iter()
            .map(|(at, burst)| (*at, burst.iter().map(place).collect::<Vec<_>>()))
            .collect();
        assert_eq!(
            places,
            [
                (t(0), vec![(id(1), Side::Bid, q("2"))]),
                (t(SECOND), vec![(id(2), Side::Bid, q("3"))]),
                (t(2 * SECOND), vec![(id(3), Side::Bid, q("2"))]),
                (t(3 * SECOND), vec![(id(4), Side::Bid, q("4"))]),
            ]
        );

        let last = run.progress.last().unwrap();
        assert_eq!(last.0, t(4 * SECOND));
        let done = last.1[0];
        assert_eq!(done.status, Status::Lapsed);
        assert_eq!(
            (done.filled, done.remaining, done.clips),
            (q("4"), q("4"), 4)
        );
    }

    /// A clip decided while the last IOC is still in flight is re-sized by
    /// that IOC's fill before the slot frees: the OMS sends what is short
    /// of the schedule, not what the clip first said.
    #[test]
    fn a_clip_waiting_on_an_ioc_in_flight_is_resized_by_its_fill() {
        let parent = Parent::twap(A, Side::Bid, q("4"), px("101"), t(2 * SECOND), 2).unwrap();
        let run = run(Script {
            parents: vec![(parent, 0)],
            reports: vec![(fill(id(1), Side::Bid, "2", "0", SECOND + 5), SECOND + 5)],
            clock: vec![SECOND],
        });
        let places: Vec<_> = run
            .requests
            .iter()
            .map(|(at, burst)| (*at, burst.iter().map(place).collect::<Vec<_>>()))
            .collect();
        // At one second the first IOC is still unanswered: the clip for 4
        // is remembered, and sent for 2 once the fill frees the slot.
        assert_eq!(
            places,
            [
                (t(0), vec![(id(1), Side::Bid, q("2"))]),
                (t(SECOND + 5), vec![(id(2), Side::Bid, q("2"))]),
            ]
        );
    }

    /// An iceberg rests its clip, refills on a fill through the OMS's amend,
    /// and withdraws when filled.
    #[test]
    fn an_iceberg_refills_and_withdraws() {
        let parent =
            Parent::iceberg(A, Side::Ask, q("3"), px("101"), t(10 * SECOND), q("2")).unwrap();
        let run = run(Script {
            parents: vec![(parent, 0)],
            reports: vec![
                (ack(id(1), 5), 5),
                (fill(id(1), Side::Ask, "1", "1", 100), 100),
                (ack(id(1), 105), 105),
                (fill(id(1), Side::Ask, "2", "0", 200), 200),
            ],
            clock: vec![SECOND],
        });
        let requests: Vec<_> = run
            .requests
            .iter()
            .map(|(at, burst)| (*at, burst.to_vec()))
            .collect();
        assert_eq!(requests.len(), 2, "{requests:?}");
        assert_eq!(requests[0].0, t(0));
        assert_eq!(place(&requests[0].1[0]), (id(1), Side::Ask, q("2")));
        // After one of three fills, two are left: the part-filled order is
        // amended back up to two.
        match requests[1].1[..] {
            [Request::Amend(amend)] => {
                assert_eq!(requests[1].0, t(100));
                assert_eq!((amend.order, amend.qty), (id(1), q("2")));
            }
            ref other => panic!("expected one amend, got {other:?}"),
        }
        let statuses: Vec<_> = run
            .progress
            .iter()
            .flat_map(|(at, burst)| burst.iter().map(|p| (*at, p.status, p.filled)))
            .collect();
        assert_eq!(
            statuses,
            [
                (t(0), Status::Working, Qty::ZERO),
                (t(100), Status::Working, q("1")),
                (t(200), Status::Working, q("3")),
                (t(200), Status::Filled, q("3")),
            ]
        );
    }

    /// A refused iceberg place is not re-sent by the reject that answered
    /// it, nor by a report after it: it waits for the next beat, which is
    /// the strategy re-deciding.
    #[test]
    fn a_refused_iceberg_waits_for_the_next_beat() {
        let parent =
            Parent::iceberg(A, Side::Ask, q("3"), px("101"), t(10 * SECOND), q("2")).unwrap();
        let rejected = |order, at| {
            Report::Reject(crate::adapters::execution::edge::Reject {
                order: Some(order),
                reason: crate::adapters::execution::edge::RejectReason::PostOnlyWouldCross,
                venue_time: None,
                recv_time: t(at),
            })
        };
        let run = run(Script {
            parents: vec![(parent, 0)],
            reports: vec![(rejected(id(1), 5), 5), (ack(id(9), 50), 50)],
            clock: vec![SECOND],
        });
        let places: Vec<_> = run
            .requests
            .iter()
            .map(|(at, burst)| (*at, burst.iter().map(place).collect::<Vec<_>>()))
            .collect();
        assert_eq!(
            places,
            [
                (t(0), vec![(id(1), Side::Ask, q("2"))]),
                (t(SECOND), vec![(id(2), Side::Ask, q("2"))]),
            ]
        );
    }

    /// A parent on another instrument is its own; a second on one replaces
    /// the first, and the replacement is what the OMS diffs against.
    #[test]
    fn parents_are_keyed_on_the_instrument() {
        let first = Parent::sweep(A, Side::Bid, q("1"), px("101"), t(SECOND)).unwrap();
        let other = Parent::sweep(B, Side::Ask, q("1"), px("99"), t(SECOND)).unwrap();
        let second = Parent::sweep(A, Side::Ask, q("2"), px("101"), t(SECOND)).unwrap();
        let run = run(Script {
            parents: vec![(first, 0), (other, 0), (second, 500)],
            ..Script::default()
        });
        let desired: Vec<_> = run
            .desired
            .iter()
            .map(|(at, burst)| (*at, burst.iter().map(|d| d.instrument).collect::<Vec<_>>()))
            .collect();
        assert_eq!(desired, [(t(0), vec![A, B]), (t(500), vec![A])]);
        assert_eq!(run.desired[1].1[0], cross(Side::Ask, "2", 500));
        let replaced: Vec<_> = run
            .progress
            .iter()
            .flat_map(|(at, burst)| burst.iter().map(|p| (*at, p.parent.side, p.status)))
            .filter(|(_, _, status)| *status == Status::Replaced)
            .collect();
        assert_eq!(replaced, [(t(500), Side::Bid, Status::Replaced)]);
    }

    /// The same script twice is the same run, instant for instant.
    #[test]
    fn a_run_is_deterministic() {
        let mut script = twap_script();
        script.parents.push((
            Parent::iceberg(B, Side::Ask, q("5"), px("99"), t(3 * SECOND), q("1")).unwrap(),
            SECOND / 2,
        ));
        let first = run(script.clone());
        assert!(first.desired.len() >= 7, "{:?}", first.desired);
        assert_eq!(first, run(script));
    }
}
