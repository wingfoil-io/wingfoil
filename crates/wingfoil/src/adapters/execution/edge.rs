//! The strategy↔venue edge: every value that crosses the swap point.
//! [`Request`] goes out; [`Report`] comes back, and beside it what the venue
//! says about the account ([`Account`], [`Holdings`], [`Cashflow`]) and
//! its own state ([`MmpTrip`], [`TradingState`]). All plain values; the
//! wiring that carries them is [`venue`](crate::adapters::execution::venue).
//!
//! [`Order`] is an intent to open something, and that is all it can say. The
//! OMS also has to say "stop showing that" and "show it a tick lower", and
//! a kill switch opens with cancel-*all* — so the edge out of
//! the strategy carries a [`Request`], of which [`Place`](Request::Place) is
//! one variant, and the edge back carries a [`Report`], of which
//! [`Fill`](Report::Fill) is one. Both are generic over the instrument, as
//! [`Order`] is.
//!
//! # One report stream, not two
//!
//! An ack and the fill it precedes have to arrive in that order, and two
//! edges cannot promise it. So every venue-to-strategy fact is a [`Report`],
//! including [`Reject`] — which also keeps rejects where the OMS is already
//! looking.
//!
//! # Amend is first-class
//!
//! Not sugar for cancel-then-place. A quoter re-prices as the touch moves,
//! so the amend path is the hot one rather than an edge case: cancel-replace
//! would double the message rate into a venue that rate-limits, and would
//! leave a window with nothing showing.
//!
//! [`Amend`] carries price and quantity on one order id and nothing else. A
//! change of side or instrument is cancel-then-place, because it is a
//! different order. What amend does *not* buy is queue priority: assume an
//! amend that moves the price loses its place exactly as a cancel-replace
//! would, which is both the conventional rule and the conservative one.
//!
//! # Why a [`Cancel`](Request::Cancel) names only an id
//!
//! Not an instrument as well. Every venue addresses a resting order by its
//! id, and whatever holds the orders — the OMS, a simulator, the venue codec
//! — is already holding them keyed in a way that answers "which
//! instrument". An [`Amend`] carries its instrument for one reader: a
//! per-order size cap that judges each request on its own.
//!
//! # Both are `Copy`, and both ride a `Burst`
//!
//! For the reason the rest of the vocabulary is: a request goes
//! round a feedback edge, where a value that allocates is cloned on every hop
//! and on every `TimeQueue` comparison.
//!
//! # A defaulted request does not cancel the book
//!
//! `Burst<T>` requires a [`Default`], so both types have one, and neither
//! means anything. The defaulted [`Request`] is `Place(Order::default())` —
//! inert because a defaulted [`Order`] has zero quantity and fails
//! [`Order::validate`] — and deliberately not
//! [`CancelAll`](Request::CancelAll), which is the one variant that does
//! something drastic with no arguments to get wrong. The defaulted [`Report`]
//! is a [`Reject`] naming no order, which the OMS cannot route and therefore
//! cannot act on.

use crate::adapters::execution::exec_id::VenueId;
use crate::adapters::execution::order::{ClientOrderId, Fill, Order, OrderError, Trigger};
use crate::adapters::market::{Px, Qty};
use crate::{Burst, NanoTime};
use std::fmt;

/// What the strategy asks the venue to do. Travels as `Burst<Request>`.
///
/// This is the edge a simulated venue sits on and the edge a live
/// order-entry codec renders, and it is the same edge on both sides of the
/// swap point: a signature that diverges between them means the swap point is
/// broken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request<I> {
    /// Open a new order.
    Place(Order<I>),
    /// Move a resting order's price and/or quantity.
    Amend(Amend<I>),
    /// Pull one resting order.
    Cancel(ClientOrderId),
    /// Pull everything, on every instrument.
    ///
    /// No arguments on purpose: it is what a kill switch opens with, and a
    /// gap is exactly when there is no state left to trust enough to
    /// enumerate what to cancel.
    CancelAll,
}

/// A new price and quantity for a resting order.
///
/// Absolute, not a delta: the OMS knows what is working and what it wants, so
/// a relative amendment would only be a subtraction done twice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Amend<I> {
    /// The order to move.
    pub order: ClientOrderId,
    /// The contract it rests on.
    ///
    /// Carried so that an amend is judged on its own — a per-order ceiling
    /// needs the contract — without anything on the request edge
    /// remembering which orders were placed: a table there would have to be
    /// kept in step with the OMS's slots from the wrong side of the edge,
    /// and its one miss aborts the run.
    pub instrument: I,
    /// The price to rest at now.
    pub price: Px,
    /// The quantity to show now.
    pub qty: Qty,
    /// The trigger to rest under now, for an order that has not yet fired:
    /// `None` for one that has none — a plain order, or a triggered one the
    /// venue has already fired, which is a plain order from then on.
    ///
    /// Said on every amend, as the price is, because an amend is absolute.
    pub trigger: Option<Trigger>,
}

/// Why a [`Request`] is not sendable.
///
/// [`Order::validate`]'s cases plus the one an [`Amend`] adds. Same idiom: the
/// message states the fact and the argument lives on the variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestError {
    /// The order a [`Place`](Request::Place) carries says two things.
    Order(OrderError),
    /// An [`Amend`] to zero or less.
    ///
    /// Amending a quote away is [`Cancel`](Request::Cancel), which says so;
    /// a venue asked to amend to nothing either rejects it or cancels the
    /// order, and which of those it does is not something to leave to a
    /// venue's choice.
    AmendToNothing(Qty),
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Order(e) => write!(f, "the order placed is not sendable: {e}"),
            Self::AmendToNothing(qty) => {
                write!(f, "an amend to a quantity of {qty}, which is not positive")
            }
        }
    }
}

impl std::error::Error for RequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Order(e) => Some(e),
            Self::AmendToNothing(_) => None,
        }
    }
}

impl From<OrderError> for RequestError {
    fn from(e: OrderError) -> Self {
        Self::Order(e)
    }
}

impl<I> Request<I> {
    /// The order this addresses, where it addresses one.
    ///
    /// [`CancelAll`](Self::CancelAll) names none — that is the point of it.
    pub const fn order(&self) -> Option<ClientOrderId> {
        match self {
            Request::Place(order) => Some(order.id),
            Request::Amend(amend) => Some(amend.order),
            Request::Cancel(id) => Some(*id),
            Request::CancelAll => None,
        }
    }

    /// Whether this request says one thing.
    ///
    /// Call it on the way out, as [`Order::validate`] is called: the sim
    /// refuses it before it reaches a book and the codec before it reaches a
    /// socket. A venue would refuse it too, but asynchronously, after the
    /// quoting loop has already counted the order as working.
    ///
    /// # Errors
    ///
    /// [`RequestError`], one variant per way a request can contradict itself.
    pub fn validate(&self) -> Result<(), RequestError> {
        match self {
            Request::Place(order) => Ok(order.validate()?),
            Request::Amend(amend) if amend.qty <= Qty::ZERO => {
                Err(RequestError::AmendToNothing(amend.qty))
            }
            Request::Amend(_) | Request::Cancel(_) | Request::CancelAll => Ok(()),
        }
    }
}

/// See the module docs: a placeholder, and an inert one.
impl<I: Default> Default for Request<I> {
    fn default() -> Self {
        Request::Place(Order::default())
    }
}

/// What the venue says came of it. Travels as `Burst<Report>`.
///
/// One stream, so that an ack and the fill it precedes cannot be reordered by
/// arriving on separate edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report<I> {
    /// The venue accepted a place or an amend. The order is working.
    Ack(Ack),
    /// The venue refused a request.
    Reject(Reject),
    /// An execution.
    Fill(Fill<I>),
    /// A resting order was pulled — by us, or by the venue's own
    /// cancel-on-disconnect or MMP.
    Cancelled(Retired),
    /// A resting order ended without being pulled: a lifetime that ran out,
    /// or a contract that expired under it.
    Expired(Retired),
    /// A triggered order fired: the reference reached its trigger, and it is
    /// now the plain order it named. Still ours, still working — what it is
    /// changed, not whether it rests. Its fills, or its end, follow as for
    /// any other order.
    Triggered(Fired),
}

/// The venue accepted a place or an amend.
///
/// Which of the two it acknowledges is not a field: the OMS knows from the
/// state the slot was in, and a field that said so could disagree with it.
///
/// What it accepted *is*: [`price`](Self::price) and
/// [`remaining`](Self::remaining) say what rests now, where the venue says.
/// A venue that rounds to its tick or its lot, or trims a size by its own
/// rule, accepts something other than what was asked, and an OMS that
/// believed the ask would carry a wrong level and a wrong size until the
/// next fill. Where the venue names neither, both are `None` and the OMS
/// keeps what it asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ack {
    /// The order accepted.
    pub order: ClientOrderId,
    /// The price the order rests at now, where the venue says — FIX `Price`
    /// (44) on a `150=0` or `150=5` report. `None` where it named none, or
    /// for a market order.
    pub price: Option<Px>,
    /// What is showing after the accept, where the venue says — FIX
    /// `LeavesQty` (151), or its total less what has filled. `None` where it
    /// named none.
    pub remaining: Option<Qty>,
    /// The venue's own id for it, verbatim, or empty where the venue named
    /// none.
    ///
    /// A [`VenueId`]: arbitrary bounded venue text that has to survive
    /// verbatim, held inline so carrying it costs the type nothing — the
    /// same representation as a fill's `ExecId`, under its own name because
    /// it names an order, not an execution. The OMS does not read it; order reconciliation
    /// does, and an ack that did not carry it could not be reconciled after
    /// the fact.
    pub venue_id: VenueId,
    /// The venue's own timestamp, if it sends one.
    pub venue_time: Option<NanoTime>,
    /// Engine time the report was received (`Ctx::time`, never
    /// `NanoTime::now`).
    pub recv_time: NanoTime,
}

/// A resting order that is no longer resting, for a reason that is not a
/// fill.
///
/// One type behind two [`Report`] variants, because cancelled and expired are
/// the same facts about the order and differ only in what ended it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retired {
    /// The order that ended.
    pub order: ClientOrderId,
    /// What was still showing when it did — zero for an order that had
    /// filled in full, which is the case a venue may report either way.
    pub remaining: Qty,
    /// The venue's own timestamp, if it sends one.
    pub venue_time: Option<NanoTime>,
    /// Engine time the report was received.
    pub recv_time: NanoTime,
}

/// A triggered order that fired.
///
/// No price or size: firing changes neither, and what then trades is said
/// by the fills.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fired {
    /// The order that fired.
    pub order: ClientOrderId,
    /// The venue's id for the order it is now, where firing changed it —
    /// a venue may hold the untriggered order under one id and make the
    /// order it fires into under another — or empty where it did not, or
    /// did not say. Read by order reconciliation, as an ack's is.
    pub venue_id: VenueId,
    /// The venue's own timestamp, if it sends one.
    pub venue_time: Option<NanoTime>,
    /// Engine time the report was received.
    pub recv_time: NanoTime,
}

/// The venue refused a request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reject {
    /// The order refused, or `None` for a request that named none — a
    /// rejected [`CancelAll`](Request::CancelAll), or a refusal the venue
    /// could not attribute.
    pub order: Option<ClientOrderId>,
    /// Why.
    pub reason: RejectReason,
    /// The venue's own timestamp, if it sends one.
    pub venue_time: Option<NanoTime>,
    /// Engine time the report was received.
    pub recv_time: NanoTime,
}

/// Why the venue refused a request.
///
/// A closed set with an `Other`, and small deliberately: the OMS's response to
/// a reject is the same whatever it says — the slot goes idle and the next
/// tick re-decides — so this is read by metrics and by risk, not by the
/// state machine. The one exception is
/// [`UnknownOrder`](Self::UnknownOrder), which is the only reason that says
/// something about what is *working*.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RejectReason {
    /// A post-only order would have crossed.
    ///
    /// Ordinary, not an error: the touch moved between the instant the quoter
    /// decided and the instant the order arrived, and the venue refused to let
    /// it cross — which is the protection working. Its *rate* is a metric,
    /// because it reads directly on how stale the decision loop is. Latching
    /// on a storm of them is risk's, not the OMS's.
    PostOnlyWouldCross,
    /// The venue has no such order: it filled, expired or was already pulled.
    ///
    /// The one reason the state machine reads, because it is the one that says
    /// the order is gone rather than that the request was.
    UnknownOrder,
    /// Refused by the venue's own risk or margin check.
    RiskLimit,
    /// Refused for sending too much, too fast.
    RateLimited,
    /// The price is outside the band the venue allows an order to rest or
    /// trade at — a futures exchange's price banding, an equity's limit-up
    /// limit-down band. Ordinary in a fast market, like a post-only reject,
    /// and read the same way: the slot goes idle and the strategy re-decides
    /// off a fresh touch.
    PriceBand,
    /// Anything else the venue said.
    ///
    /// The default, so that a report nobody has classified reads as
    /// unexplained rather than as one of the reasons above.
    #[default]
    Other,
}

impl<I> Report<I> {
    /// The order this is about, where it is about one.
    pub const fn order(&self) -> Option<ClientOrderId> {
        match self {
            Report::Ack(ack) => Some(ack.order),
            Report::Reject(reject) => reject.order,
            Report::Fill(fill) => Some(fill.order),
            Report::Cancelled(retired) | Report::Expired(retired) => Some(retired.order),
            Report::Triggered(fired) => Some(fired.order),
        }
    }

    /// Engine time the report was received.
    pub const fn recv_time(&self) -> NanoTime {
        match self {
            Report::Ack(ack) => ack.recv_time,
            Report::Reject(reject) => reject.recv_time,
            Report::Fill(fill) => fill.recv_time,
            Report::Cancelled(retired) | Report::Expired(retired) => retired.recv_time,
            Report::Triggered(fired) => fired.recv_time,
        }
    }
}

/// See the module docs: a placeholder naming no order, so that nothing can
/// route it.
impl<I> Default for Report<I> {
    fn default() -> Self {
        Report::Reject(Reject::default())
    }
}

/// What the venue says the account is, at one instant.
///
/// `Copy` and small, like everything else on these edges. Every field is an
/// `Option` and `None` is never a pass: the risk limits read an unknown
/// equity and an uncomputed utilisation as breaches, because a cap nobody
/// could evaluate has not been respected.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Account {
    /// The account's equity, in the settlement currency of the book it
    /// margins. `None` where the venue has not said.
    ///
    /// This is what anchors a day's loss and sizes a risk cap, so it is the
    /// account's number and not a fold over our own fills: a fold knows what
    /// we did, not what we have.
    pub equity: Option<Qty>,
    /// Initial margin as a fraction of equity — a margin model's in a
    /// backtest, the venue's own figure live. `None` where the book could
    /// not be repriced.
    pub utilisation: Option<f64>,
    /// The engine time this was read at (`Ctx::time`, never
    /// `NanoTime::now`), so a consumer can tell a quiet account from a stale
    /// one.
    pub as_of: NanoTime,
}

/// One position, as the venue holds it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held<I> {
    /// The contract.
    pub instrument: I,
    /// Signed, positive long, in the unit the contract's orders are sized in
    /// — the unit [`Position::net`](crate::adapters::execution::position::Position::net) is in,
    /// so the two compare without a conversion.
    pub net: Qty,
}

/// What the venue says it holds, at one instant.
///
/// Either the whole book or only what changed, and [`whole`](Self::whole)
/// says which, because the two mean different things about an instrument
/// the burst does not name: after a snapshot it is flat, after an update it
/// is whatever it was. A venue that pushes changes and answers a snapshot
/// on connect sends both; a simulated one, which cannot lose an update,
/// sends only changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Holdings<I: Default> {
    /// Whether [`held`](Self::held) is every position the account has,
    /// so that anything it does not name is flat.
    pub whole: bool,
    /// The positions stated, in the venue's own order. A snapshot followed
    /// by later changes in one instant is one `whole` burst with the changes
    /// after the snapshot's rows, applied in order.
    pub held: Burst<Held<I>>,
    /// The engine time it arrived at (`Ctx::time`).
    pub as_of: NanoTime,
}

/// Money the venue moved on a position with no execution behind it: funding
/// on a perpetual, a coupon on a bond, a dividend on a share, a borrow fee on
/// a short, swap points on an FX position, accrued interest at a bond trade.
///
/// A cashflow, not a fill — it changes what the position has earned and
/// leaves what it holds alone, so it moves no net and reconciles against
/// nothing. It is **not** a future's daily variation margin: that realises
/// the mark the position fold already carries as unrealised PnL, and booking
/// it here would count it twice. `Copy` and small, like every value on these
/// edges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cashflow<I> {
    /// The position it was charged or paid on.
    pub instrument: I,
    /// Signed, in the instrument's settlement currency. **Positive is money
    /// in**: received by the holder. A long perpetual pays while it trades
    /// over the index and a short is paid; a long bond receives its coupon.
    pub amount: Qty,
    /// The venue's own timestamp for it, where it gave one.
    pub venue_time: Option<NanoTime>,
    /// The engine time it arrived at (`Ctx::time`).
    pub recv_time: NanoTime,
}

/// The venue's market maker protection fired: every protected order has been
/// pulled — the pulls themselves arrive as [`Report::Cancelled`] — and new
/// ones are
/// refused until [`frozen_until`](Self::frozen_until).
///
/// A gap as the quote book sees it, and possibly the first thing that says
/// so: a caller latches its risk on one. A venue without the protection
/// never sends one.
///
/// `Copy` and small, like every value on these edges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MmpTrip {
    /// When the venue lets protected orders in again, or `None` where it
    /// said until reset — or did not say, which is read the same way,
    /// since a freeze nobody can see the end of has not ended.
    pub frozen_until: Option<NanoTime>,
    /// The venue's own timestamp for it, where it gave one.
    pub venue_time: Option<NanoTime>,
    /// The engine time it arrived at (`Ctx::time`).
    pub recv_time: NanoTime,
}

/// Whether a venue's book is taking orders — the venue's own clock
///.
///
/// A continuous venue never states one and is open. A session venue has an
/// open, auctions, halts and a close, and the OMS holds everything but a
/// cancel while it is anything but [`Open`](Self::Open): an order sent into
/// a halted or closed book is refused at best, and at worst rests into an
/// auction at a price decided on a market that was not there.
///
/// Defaults to [`Closed`](Self::Closed), the inert one for a filler value:
/// a defaulted state that read as open would be permission nobody stated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TradingState {
    /// Continuous trading.
    Open,
    /// An auction — the open's, the close's, or a volatility interruption's.
    Auction,
    /// Trading halted.
    Halted,
    /// Closed: nothing trades, and day orders have expired.
    #[default]
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Burst;
    use crate::adapters::market::Side;

    type Instrument = u32;
    type Order = super::Order<Instrument>;
    type Request = super::Request<Instrument>;
    type Report = super::Report<Instrument>;
    type Amend = super::Amend<Instrument>;

    fn qty(s: &str) -> Qty {
        Qty::parse(s).unwrap()
    }

    fn px(s: &str) -> Px {
        Px::parse(s).unwrap()
    }

    fn resting() -> Order {
        Order::post_only(
            ClientOrderId(7),
            Instrument::default(),
            Side::Bid,
            qty("10"),
            px("0.05"),
        )
    }

    /// The same property `Order` and `Fill` have, and for the same reason:
    /// both ride the feedback edge, where an allocation is paid on every hop.
    #[test]
    fn requests_and_reports_are_plain_values() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<Request>();
        assert_copy::<Report>();
    }

    #[test]
    fn they_can_ride_a_burst() {
        let mut requests: Burst<Request> = Burst::new();
        requests.push(Request::Cancel(ClientOrderId(1)));
        requests.push(Request::CancelAll);
        assert_eq!(requests.len(), 2);
        let reports: Burst<Report> = Burst::new();
        assert!(reports.is_empty());
    }

    /// The `Default` exists for `Burst` and for nothing else, so what matters
    /// is that it cannot do anything. `CancelAll` would have been the other
    /// obvious choice and is the one variant that needs no arguments to empty
    /// the book — which is exactly why it is not the default.
    #[test]
    fn a_defaulted_request_does_not_cancel_the_book() {
        let request = Request::default();
        assert_ne!(request, Request::CancelAll);
        assert_eq!(
            request.validate(),
            Err(RequestError::Order(OrderError::NonPositiveQuantity(
                Qty::ZERO
            ))),
            "the inert default order is what makes the default request inert"
        );
    }

    /// A defaulted report names no order, so nothing keyed on an order id can
    /// route it into a state machine.
    #[test]
    fn a_defaulted_report_is_about_nothing() {
        assert_eq!(Report::default().order(), None);
        assert_eq!(
            Report::default(),
            Report::Reject(Reject {
                reason: RejectReason::Other,
                ..Reject::default()
            }),
            "an unclassified refusal reads as unexplained, not as a known reason"
        );
    }

    /// `CancelAll` is the only request that addresses no order; everything
    /// else is addressed, which is what lets the OMS route a report back to
    /// the slot that asked.
    #[test]
    fn every_request_but_cancel_all_names_its_order() {
        let id = ClientOrderId(7);
        assert_eq!(Request::Place(resting()).order(), Some(id));
        assert_eq!(
            Request::Amend(Amend {
                order: id,
                instrument: Instrument::default(),
                price: px("0.04"),
                qty: qty("10"),
                trigger: None,
            })
            .order(),
            Some(id)
        );
        assert_eq!(Request::Cancel(id).order(), Some(id));
        assert_eq!(Request::CancelAll.order(), None);
    }

    /// Validation is `Order::validate` plus the one case an amend adds.
    #[test]
    fn an_amend_to_nothing_is_not_a_cancel() {
        let to = |q: &str| {
            Request::Amend(Amend {
                order: ClientOrderId(7),
                instrument: Instrument::default(),
                price: px("0.04"),
                qty: qty(q),
                trigger: None,
            })
        };
        to("1").validate().unwrap();
        assert_eq!(
            to("0").validate(),
            Err(RequestError::AmendToNothing(qty("0")))
        );
        assert_eq!(
            to("-1").validate(),
            Err(RequestError::AmendToNothing(qty("-1")))
        );
        // Pulling a quote is said with the variant that means it.
        Request::Cancel(ClientOrderId(7)).validate().unwrap();
        Request::CancelAll.validate().unwrap();
    }

    /// A place carries an order, so it inherits every way an order can
    /// contradict itself rather than restating them.
    #[test]
    fn a_place_validates_the_order_it_carries() {
        Request::Place(resting()).validate().unwrap();
        let crossing = Order {
            price: None,
            ..resting()
        };
        assert_eq!(
            Request::Place(crossing).validate(),
            Err(RequestError::Order(OrderError::RestingWithoutPrice(
                crate::adapters::execution::order::OrderKind::PostOnly
            )))
        );
    }

    /// Every report is about one order except a refusal the venue could not
    /// attribute, and every one carries the engine time it arrived — which is
    /// what the OMS's staleness rule reads.
    #[test]
    fn a_report_says_which_order_and_when() {
        let now = NanoTime::from(1_000u64);
        let ack = Report::Ack(Ack {
            order: ClientOrderId(7),
            recv_time: now,
            ..Ack::default()
        });
        assert_eq!(
            (ack.order(), ack.recv_time()),
            (Some(ClientOrderId(7)), now)
        );

        let retired = Report::Cancelled(Retired {
            order: ClientOrderId(7),
            remaining: qty("4"),
            recv_time: now,
            ..Retired::default()
        });
        assert_eq!(retired.order(), Some(ClientOrderId(7)));

        let unattributed = Report::Reject(Reject {
            order: None,
            reason: RejectReason::RateLimited,
            recv_time: now,
            ..Reject::default()
        });
        assert_eq!(unattributed.order(), None);
        assert_eq!(unattributed.recv_time(), now);
    }

    /// A firing is about one order, like every other report but an
    /// unattributed refusal.
    #[test]
    fn a_firing_names_its_order() {
        let now = NanoTime::from(1_000u64);
        let fired = Report::Triggered(Fired {
            order: ClientOrderId(7),
            recv_time: now,
            ..Fired::default()
        });
        assert_eq!(
            (fired.order(), fired.recv_time()),
            (Some(ClientOrderId(7)), now)
        );
    }
}
