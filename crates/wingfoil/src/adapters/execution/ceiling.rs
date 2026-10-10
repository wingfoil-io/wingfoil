//! A per-order ceiling on the request edge: the backstop under the risk
//! limits, in both run modes.
//!
//! The limits cap what the *book* is exposed to, and a strategy sizes every
//! order it sends a clip at a time. Neither is a statement about one order
//! reaching the venue. This is: a [`Place`](Request::Place) or an
//! [`Amend`](Request::Amend) larger than the [`Cap`] for its contract is a
//! bug in whatever sized it, not a market event, and [`Capped`] **aborts the
//! run** on it rather than sending it or quietly dropping it. A dropped
//! request is one the OMS waits on forever; an aborted run is one the venue's
//! cancel-on-disconnect and a watchdog clean up after, loudly.
//!
//! [`Capped`] is a [`Venue`] around a venue, so it sits exactly on the swap
//! point's request edge and the strategy cannot tell it is there — and it
//! wraps a simulated venue as well as a live one, so a backtest that would
//! have tripped it trips it too. It holds no model: every comparison is
//! `Qty` against `Qty` in the contract's own order unit, and what the cap is
//! for each contract is the caller's [`Cap`].

use std::fmt;
use std::fmt::Debug;

use crate::adapters::market::Qty;
use crate::prelude::*;

use crate::adapters::execution::edge::Request;
use crate::adapters::execution::order::{ClientOrderId, Instrument};
use crate::adapters::execution::venue::{Session, Venue};

/// The largest single order on each contract, in that contract's order unit,
/// or `None` for a contract nothing bounds — which nothing may then send.
///
/// Cloned once into the request edge's filter at wiring, so `Clone` and
/// `'static` — not `Copy`, so a cap that is a table (a map of contract to
/// clip, loaded from a file) implements it as readily as a constant.
pub trait Cap<I>: Clone + Debug + 'static {
    /// The ceiling for `instrument`, or `None` where there is none.
    fn of(&self, instrument: &I) -> Option<Qty>;
}

/// A request past the [`Cap`], or one it cannot judge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Breach<I: Debug> {
    /// An order larger than its contract's ceiling.
    Over {
        /// The order.
        order: ClientOrderId,
        /// The contract.
        instrument: I,
        /// What it asked for.
        qty: Qty,
        /// What it may ask for.
        ceiling: Qty,
    },
    /// A contract the cap has no number for. A contract nobody bounded is
    /// not one anything may send.
    Unbounded {
        /// The order.
        order: ClientOrderId,
        /// The contract.
        instrument: I,
    },
}

impl<I: Debug> fmt::Display for Breach<I> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Over {
                order,
                instrument,
                qty,
                ceiling,
            } => write!(
                f,
                "order {order:?} for {qty} on {instrument:?} is over the ceiling of {ceiling}"
            ),
            Self::Unbounded { order, instrument } => write!(
                f,
                "order {order:?} is on {instrument:?}, a contract with no ceiling"
            ),
        }
    }
}

impl<I: Debug> std::error::Error for Breach<I> {}

/// Whether `qty` of `instrument` fits under `cap`.
///
/// # Errors
///
/// [`Breach::Over`] past the ceiling, [`Breach::Unbounded`] for a contract
/// with none.
pub fn admit<I: Instrument>(
    cap: &impl Cap<I>,
    order: ClientOrderId,
    instrument: I,
    qty: Qty,
) -> Result<(), Breach<I>> {
    let ceiling = cap
        .of(&instrument)
        .ok_or(Breach::Unbounded { order, instrument })?;
    if qty.raw().abs() > ceiling.raw() {
        return Err(Breach::Over {
            order,
            instrument,
            qty,
            ceiling,
        });
    }
    Ok(())
}

/// The ceiling on every request: a place or an amend is judged against its
/// own contract, and a cancel is never over anything.
///
/// Stateless on purpose. A judge that remembered what it placed, to look an
/// amend's contract up by its id, would be tracking order state from the
/// wrong side of the edge: the OMS legitimately amends after a cancel it
/// sent was refused, or re-prices a place acked after a cancel-all, and a
/// table that forgot the id on the cancel aborted the run on both. So the
/// [`Amend`](crate::adapters::execution::edge::Amend) carries its contract instead, and nothing
/// here can be out of step with the slots.
#[derive(Debug)]
pub struct Judge<C> {
    cap: C,
}

impl<C> Judge<C> {
    /// A judge of `cap`.
    pub const fn new(cap: C) -> Self {
        Self { cap }
    }

    /// Judge one request.
    ///
    /// # Errors
    ///
    /// As [`admit`].
    pub fn judge<I: Instrument>(&self, request: &Request<I>) -> Result<(), Breach<I>>
    where
        C: Cap<I>,
    {
        match request {
            Request::Place(order) => admit(&self.cap, order.id, order.instrument, order.qty),
            Request::Amend(amend) => admit(&self.cap, amend.order, amend.instrument, amend.qty),
            Request::Cancel(_) | Request::CancelAll => Ok(()),
        }
    }
}

/// A venue whose request edge is held to a [`Cap`].
///
/// Wiring it is the whole of its behaviour: every burst the strategy sends is
/// judged, whole, before the venue it wraps sees any of it, and the first
/// breach aborts the run. What comes back is the wrapped venue's session,
/// untouched.
pub struct Capped<'a, I: Instrument, C> {
    venue: &'a dyn Venue<I>,
    cap: C,
}

impl<'a, I: Instrument, C> Capped<'a, I, C> {
    /// `venue`, capped at `cap`.
    pub fn new(venue: &'a dyn Venue<I>, cap: C) -> Self {
        Self { venue, cap }
    }
}

impl<I: Instrument + Send + Sync + 'static, C: Cap<I>> Venue<I> for Capped<'_, I, C> {
    fn wire(&self, requests: &Stream<Burst<Request<I>>>) -> Session<I> {
        let judge = Judge::new(self.cap.clone());
        let checked = requests.try_map_filter(move |burst: &Burst<Request<I>>| {
            for request in burst {
                judge.judge(request)?;
            }
            Ok((burst.clone(), true))
        });
        self.venue.wire(&checked)
    }
}

#[cfg(test)]
mod tests {
    use crate::adapters::market::{Px, Side};

    use super::*;
    use crate::adapters::execution::order::Order;

    /// Two tickers, only the first bounded.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
    enum Ticker {
        #[default]
        Bounded,
        Unbounded,
    }

    #[derive(Clone, Copy, Debug)]
    struct Hundred;

    impl Cap<Ticker> for Hundred {
        fn of(&self, instrument: &Ticker) -> Option<Qty> {
            (*instrument == Ticker::Bounded).then(|| Qty::parse("100").unwrap())
        }
    }

    fn place(ticker: Ticker, size: &str) -> Request<Ticker> {
        Request::Place(Order::post_only(
            ClientOrderId(1),
            ticker,
            Side::Bid,
            Qty::parse(size).unwrap(),
            Px::parse("10").unwrap(),
        ))
    }

    #[test]
    fn at_the_cap_is_inside_and_past_it_or_unbounded_is_not() {
        let judge = Judge::new(Hundred);
        assert_eq!(judge.judge(&place(Ticker::Bounded, "100")), Ok(()));
        assert!(matches!(
            judge.judge(&place(Ticker::Bounded, "100.5")),
            Err(Breach::Over { .. })
        ));
        assert_eq!(
            judge.judge(&place(Ticker::Unbounded, "1")),
            Err(Breach::Unbounded {
                order: ClientOrderId(1),
                instrument: Ticker::Unbounded,
            })
        );
        assert_eq!(judge.judge(&Request::<Ticker>::CancelAll), Ok(()));
    }

    /// On the graph: a burst inside the cap reaches the venue, and the first
    /// one over it aborts the run — the error names the order — rather than
    /// being dropped or resized on the way through.
    #[test]
    fn capped_passes_what_is_inside_and_aborts_the_run_on_the_first_breach() {
        use crate::adapters::execution::edge::Report;
        use crate::{NanoTime, RunFor, RunMode};

        /// A venue that answers every burst with an empty one, so that what
        /// reached it can be counted and the only thing the run can say is
        /// what the ceiling said.
        struct Echo;
        impl Venue<Ticker> for Echo {
            fn wire(&self, requests: &Stream<Burst<Request<Ticker>>>) -> Session<Ticker> {
                Session::quiet(
                    requests.map(|_: &Burst<Request<Ticker>>| Burst::<Report<Ticker>>::new()),
                )
            }
        }

        let order = |id: u64, size: &str| {
            Request::Place(Order::post_only(
                ClientOrderId(id),
                Ticker::Bounded,
                Side::Bid,
                Qty::parse(size).unwrap(),
                Px::parse("10").unwrap(),
            ))
        };

        let g = GraphBuilder::new();
        let requests = g.replay_results([Ok((order(1, "100"), NanoTime::new(1)))]);
        let session = Capped::new(&Echo, Hundred).wire(&requests);
        let seen = session.reports.accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .expect("inside the ceiling, the run completes");
        assert_eq!(runner.value(&seen).len(), 1, "the burst reached the venue");

        let g = GraphBuilder::new();
        let requests = g.replay_results([
            Ok((order(1, "100"), NanoTime::new(1))),
            Ok((order(2, "101"), NanoTime::new(2))),
        ]);
        let session = Capped::new(&Echo, Hundred).wire(&requests);
        let _seen = session.reports.accumulate();
        let err = g
            .build()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .expect_err("a tick past the ceiling aborts the run");
        let text = format!("{err:#}");
        assert!(text.contains("over the ceiling"), "{text}");
        assert!(text.contains("ClientOrderId(2)"), "{text}");
    }
}
