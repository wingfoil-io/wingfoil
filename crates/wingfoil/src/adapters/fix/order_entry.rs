//! FIX order entry for the execution layer: the tag=value either side of
//! [`FixBridge`], and a [`Venue`] over a FIX session.
//!
//! [`FixBridge`]: crate::trading::execution::fix::FixBridge
//!
//! Gated on `fix` **and** `execution`. The execution layer knows no wire
//! protocol — its [`fix`](crate::trading::execution::fix) module is the
//! *shape* FIX imposes on order entry, as values — and this adapter knew no
//! orders. This module is the bridge, and the only place the two meet:
//!
//! ```text
//!   requests ─► FixBridge ─► Message ─► FixCodec::encode ─► FixSender ─► venue
//!   reports  ◄─ FixBridge ◄─ ExecReport ◄─ FixCodec::decode ◄─ inbound ◄─┘
//! ```
//!
//! # What is generic, and what a venue states
//!
//! [`FixCodec`] writes and reads standard FIX 4.4 order entry:
//! NewOrderSingle (`35=D`), OrderCancelReplaceRequest (`G`),
//! OrderCancelRequest (`F`) and OrderMassCancelRequest (`q`) out;
//! ExecutionReport (`8`), OrderCancelReject (`9`), BusinessMessageReject
//! (`j`) and OrderMassCancelReport (`r`) in. Order type, time in force,
//! post-only (`ExecInst` `6`) and the execution fields (`LastQty`, `CumQty`,
//! `LeavesQty`, `LastPx`, `ExecID`, `LastLiquidityInd`, `TransactTime`) are
//! the standard's and the same everywhere.
//!
//! What differs between venues is a [`Dialect`]: how an instrument is named
//! (`Symbol`, or `SecurityID` with its source), how a `ClOrdID` is written
//! (the venue's length cap), which reject codes mean what, and any field a
//! venue requires that the standard leaves optional (an account, a
//! `HandlInst`). A venue integration is a `Dialect` and a [`FixOrderVenue`]
//! built on it — no codec of its own.
//!
//! # Nothing is dropped quietly
//!
//! A slot the OMS has a request in flight on waits until something answers
//! it, so every way an answer can go missing here is either turned into one
//! or logged as an error:
//!
//! - **A message the session would not take** — [`FixSender::send`] refused
//!   it, its queue full or the session closed — is answered as the venue's
//!   refusal of it in the same instant, inside the bridge's node
//!   ([`FixOps::fix_bridge_sending`]): a new order is forgotten, a
//!   refused cancel or replace leaves the order resting.
//! - **A business reject** (`35=j`) of an order-entry message (`RefMsgType`
//!   `D` / `G` / `F` / `q`) names it by its `ClOrdID` (`BusinessRejectRefID`)
//!   and is that refusal — one without `RefMsgType` is logged as an error; a **refused mass
//!   cancel** (`35=r`, `MassCancelResponse` `0`) is the refusal of the mass
//!   cancel.
//! - **A session reject** (`35=3`) names only the refused message's
//!   *sequence number*, which this module never sees — the session stamps
//!   it — so it cannot be attributed to an order. It is logged as an error.
//! - **An execution that cannot be read** — a field missing or unparseable,
//!   an `ExecID` longer than [`ExecId`] holds, a side or instrument the
//!   dialect does not know, a `ClOrdID` it cannot parse — is logged as an
//!   error naming what was wrong. It is never booked on a guess.
//! - **A trade bust or correction** (`150=H` / `G`, or FIX 4.2's
//!   `ExecTransType` `1` / `2`) has no kind on this edge, so it is not booked
//!   as a fill — booking a correction as a new fill counts it twice — and is
//!   logged as an error: the position needs reconciling against the venue.
//!
//! - **A refusal whose own `ClOrdID` is unreadable** but whose
//!   `OrigClOrdID` is not is attributed by the latter, under
//!   [`ClOrdId::UNKNOWN`].
//! - **Done for day** (`150=3`) changes nothing: a good-till-cancel order
//!   is still working tomorrow, and a day order's end arrives as `150=C`. A
//!   **restatement** (`150=D`) or **stop** (`150=7`) has no kind on this edge
//!   and is logged as an error.
//!
//! Status (`150=I`, or `ExecTransType` `3`) and pending reports
//! (`6` / `E` / `A`) answer no request and decode to `None` without a word.
//!
//! # What it does not do
//!
//! - **No session.** Logon, sequence numbers, heartbeats and reconnect are
//!   the session's ([`fix_connect_tls`](super::fix_connect_tls) and friends);
//!   this module writes to its [`FixSender`] and reads its inbound stream.
//! - **No triggers.** [`FixBridge`] refuses a triggered order rather
//!   than send it as the plain order it names.
//! - **No answer for a message lost after it was queued.** `send` returning
//!   `Ok` means the session's queue took the message, not that it reached
//!   the wire. If the session's socket write then fails, the session
//!   reconnects and the message is gone: nothing refuses it, and the slot
//!   it was sent for waits until the venue's own state answers — an order
//!   status request after the reconnect, or the strategy's staleness. That
//!   is the session's to fix (answering what it drops from its queue), not
//!   this module's, which cannot see past `send`. What this module does
//!   do is state the session's status as the venue's [`TradingState`], so
//!   the OMS holds new places and amends while the link is down rather
//!   than queueing them into a socket that is not there.
//! - **No order status.** An order resting at the venue under a `ClOrdID`
//!   this process did not mint — the last process's, say — has no
//!   representation on the edge: its reports are logged and dropped, its
//!   fills never reach the position fold, and a cancel-all rendered per
//!   order does not pull it. Reconnecting into live orders is an
//!   integration's to handle, with an `OrderStatusRequest` or a mass
//!   status, until this edge grows a kind for it.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::NanoTime;
use crate::prelude::*;
use crate::trading::execution::edge::{RejectReason, Request, TradingState};
use crate::trading::execution::exec_id::{ExecId, VenueId};
use crate::trading::execution::fix::{
    ClOrdId, ExecKind, ExecReport, FixOps, Message, NewOrder, Trade,
};
use crate::trading::execution::order::{Epoch, Instrument, Liquidity, OrderKind, TimeInForce};
use crate::trading::execution::venue::{Session, Venue};
use crate::trading::market::{Px, Qty, SCALE, Side, mul_div};

use super::{FixMessage, FixSender, FixSessionStatus, parse_sending_time, utc_timestamp};

/// FIX tags this module reads or writes.
pub mod tag {
    /// `ClOrdID`.
    pub const CL_ORD_ID: u32 = 11;
    /// `Commission`.
    pub const COMMISSION: u32 = 12;
    /// `CommType`.
    pub const COMM_TYPE: u32 = 13;
    /// `CumQty`.
    pub const CUM_QTY: u32 = 14;
    /// `ExecID`.
    pub const EXEC_ID: u32 = 17;
    /// `ExecInst`.
    pub const EXEC_INST: u32 = 18;
    /// `ExecTransType` (FIX 4.2).
    pub const EXEC_TRANS_TYPE: u32 = 20;
    /// `SecurityIDSource`.
    pub const SECURITY_ID_SOURCE: u32 = 22;
    /// `LastPx`.
    pub const LAST_PX: u32 = 31;
    /// `LastQty`.
    pub const LAST_QTY: u32 = 32;
    /// `OrderID`.
    pub const ORDER_ID: u32 = 37;
    /// `OrderQty`.
    pub const ORDER_QTY: u32 = 38;
    /// `OrdStatus`.
    pub const ORD_STATUS: u32 = 39;
    /// `OrdType`.
    pub const ORD_TYPE: u32 = 40;
    /// `OrigClOrdID`.
    pub const ORIG_CL_ORD_ID: u32 = 41;
    /// `Price`.
    pub const PRICE: u32 = 44;
    /// `RefSeqNum`.
    pub const REF_SEQ_NUM: u32 = 45;
    /// `RefMsgType`.
    pub const REF_MSG_TYPE: u32 = 372;
    /// `SecurityID`.
    pub const SECURITY_ID: u32 = 48;
    /// `Side`.
    pub const SIDE: u32 = 54;
    /// `Symbol`.
    pub const SYMBOL: u32 = 55;
    /// `Text`.
    pub const TEXT: u32 = 58;
    /// `TimeInForce`.
    pub const TIME_IN_FORCE: u32 = 59;
    /// `TransactTime`.
    pub const TRANSACT_TIME: u32 = 60;
    /// `CommCurrency`.
    pub const COMM_CURRENCY: u32 = 479;
    /// `CxlRejReason`.
    pub const CXL_REJ_REASON: u32 = 102;
    /// `OrdRejReason`.
    pub const ORD_REJ_REASON: u32 = 103;
    /// `ExecType`.
    pub const EXEC_TYPE: u32 = 150;
    /// `LeavesQty`.
    pub const LEAVES_QTY: u32 = 151;
    /// `BusinessRejectRefID`.
    pub const BUSINESS_REJECT_REF_ID: u32 = 379;
    /// `MassCancelRequestType`.
    pub const MASS_CANCEL_REQUEST_TYPE: u32 = 530;
    /// `MassCancelResponse`.
    pub const MASS_CANCEL_RESPONSE: u32 = 531;
    /// `LastLiquidityInd`.
    pub const LAST_LIQUIDITY_IND: u32 = 851;
}

/// What one venue's FIX says differently from the standard.
///
/// Two methods are required, because the standard leaves the choice open:
/// how an instrument is named on the way out and recognised on the way in.
/// The rest default to FIX 4.4 and are overridden where a venue departs from
/// it.
pub trait Dialect<I>: Clone + 'static {
    /// Append the fields naming `instrument` to an outbound message —
    /// `Symbol` (`55`), or `SecurityID` (`48`) with its `SecurityIDSource`
    /// (`22`), or whatever the venue keys on.
    fn instrument(&self, instrument: &I, fields: &mut Vec<(u32, String)>);

    /// The instrument an execution report is for, or `None` for one this
    /// dialect does not trade. A trade whose instrument is `None` is not
    /// booked (it cannot be booked against nothing) and is logged as an
    /// error, so a venue that omits the field on some reports answers from
    /// what it trades.
    fn instrument_of(&self, report: &FixMessage) -> Option<I>;

    /// A `ClOrdID` as it goes on the wire. Decimal by default: the bridge's
    /// ids carry the process's epoch, so they are at most 16 digits.
    fn cl_ord_id(&self, id: ClOrdId) -> String {
        id.0.to_string()
    }

    /// A `ClOrdID` read back, or `None` for one this process could not
    /// have minted. The inverse of [`cl_ord_id`](Self::cl_ord_id).
    fn parse_cl_ord_id(&self, text: &str) -> Option<ClOrdId> {
        text.parse().ok().map(ClOrdId)
    }

    /// Why an order was refused (`150=8`). By default `OrdRejReason` (`103`)
    /// `5` is an unknown order and `3` a limit, as FIX 4.4 has them.
    fn order_reject(&self, report: &FixMessage) -> RejectReason {
        match report.field(tag::ORD_REJ_REASON) {
            Some("5") => RejectReason::UnknownOrder,
            Some("3") => RejectReason::RiskLimit,
            _ => RejectReason::Other,
        }
    }

    /// Why a cancel or replace was refused (`35=9`): the one reading the
    /// bridge acts on, since [`UnknownOrder`](RejectReason::UnknownOrder)
    /// is what retires the order and its slot.
    ///
    /// By default `CxlRejReason` (`102`) `1` is an unknown order, and
    /// `OrdStatus` (`39`) outranks it where the venue states it: `4`, `8`
    /// and `C` say the order is over, whatever the code said; a status that
    /// says the order is live (`0`, `1`, `5`, `6`, `7`, `9`, `A`, `E`)
    /// keeps it, since the code named the request, not the order, and
    /// forgetting it would orphan it at the venue. `2` (filled) keeps it
    /// too: the fill that filled it may still be behind this reject, and
    /// retires it itself. A status that says neither way (`3` done for day,
    /// `B` calculated) leaves the code to stand. A venue whose statuses
    /// mean otherwise says so here.
    fn cancel_reject(&self, report: &FixMessage) -> RejectReason {
        let code = match report.field(tag::CXL_REJ_REASON) {
            Some("1") => RejectReason::UnknownOrder,
            _ => RejectReason::Other,
        };
        match report.field(tag::ORD_STATUS) {
            Some("4" | "8" | "C") => RejectReason::UnknownOrder,
            Some(status @ ("0" | "1" | "2" | "5" | "6" | "7" | "9" | "A" | "E"))
                if code == RejectReason::UnknownOrder =>
            {
                log::warn!(
                    "fix order entry: a cancel reject reads as an unknown order \
                     but OrdStatus {status} says the order is live; kept"
                );
                RejectReason::Other
            }
            _ => code,
        }
    }

    /// Whether a report's commission is in the instrument's settlement
    /// currency — the only currency a [`Fill`]'s fee can be booked in. By
    /// default, yes where the report names no `CommCurrency` (`479`) and no
    /// where it names one: this module does not know what the instrument
    /// settles in, so a currency it cannot compare is one it cannot book. A
    /// venue that always names the settlement currency says so by
    /// overriding this.
    ///
    /// [`Fill`]: crate::trading::execution::order::Fill
    fn commission_in_settlement_currency(&self, report: &FixMessage) -> bool {
        report.field(tag::COMM_CURRENCY).is_none()
    }

    /// The commission on an execution of `qty` at `price`, in the
    /// settlement currency, or `None` where the report states none or
    /// states one this dialect cannot read. By default `Commission` (`12`)
    /// read as `CommType` (`13`) says: `3` an absolute amount, `1` an amount
    /// per unit (times `LastQty`), `2` a fraction of `LastQty × LastPx` (FIX
    /// states a percentage commission as its fraction, five percent as
    /// `0.05`) — of the *price* notional, so a venue whose contracts carry a
    /// multiplier states its own reading here. A `Commission` with any other
    /// type, or none, or that does not parse, or in a currency
    /// [`commission_in_settlement_currency`] does not vouch for, is not
    /// read: a fee booked on a guess is worse than none. The codec logs the
    /// first report per venue whose stated commission went unread.
    ///
    /// [`commission_in_settlement_currency`]: Self::commission_in_settlement_currency
    fn commission(&self, report: &FixMessage, qty: Qty, price: Px) -> Option<Qty> {
        let commission = Qty::parse(report.field(tag::COMMISSION)?).ok()?;
        if !self.commission_in_settlement_currency(report) {
            return None;
        }
        match report.field(tag::COMM_TYPE) {
            Some("3") => Some(commission),
            Some("1") => mul_div(commission.raw(), qty.raw(), SCALE).map(Qty::from_raw),
            Some("2") => mul_div(qty.raw(), price.raw(), SCALE)
                .and_then(|notional| mul_div(commission.raw(), notional, SCALE))
                .map(Qty::from_raw),
            _ => None,
        }
    }

    /// Fields the venue requires that the standard leaves optional — an
    /// account, a `HandlInst` — appended to every outbound `message`.
    /// None by default.
    fn extra(&self, message: &Message<I>, fields: &mut Vec<(u32, String)>) {
        let _ = (message, fields);
    }
}

/// Why an inbound message that answers an order could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unread {
    /// A field the report needs is absent or does not parse.
    Field(u32),
    /// The `ClOrdID` (or `BusinessRejectRefID`) is not one the dialect
    /// can read.
    ClOrdId,
    /// An `ExecID` longer than [`ExecId`] holds.
    ExecId,
    /// A `Side` that is neither a buy nor a sell.
    Side,
    /// An instrument the dialect does not trade.
    Instrument,
    /// A partial fill that states neither `LeavesQty` nor `OrderQty`, so
    /// what is left of the order is unknown.
    Remaining,
    /// A trade bust or correction, which this edge has no kind for.
    Correction,
    /// A session-level reject, which names a sequence number, not an order.
    SessionReject,
    /// The venue restated the order (`150=D`) on its own authority.
    Restated,
    /// The venue stopped the order (`150=7`), guaranteeing it a price.
    Stopped,
}

impl fmt::Display for Unread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Field(tag) => write!(f, "tag {tag} absent or unreadable"),
            Self::ClOrdId => write!(f, "a ClOrdID the dialect cannot read"),
            Self::ExecId => write!(f, "an ExecID longer than {} bytes", ExecId::CAPACITY),
            Self::Side => write!(f, "a Side that is neither a buy nor a sell"),
            Self::Instrument => write!(f, "an instrument the dialect does not trade"),
            Self::Remaining => write!(f, "a partial fill stating neither LeavesQty nor OrderQty"),
            Self::Correction => write!(
                f,
                "a trade bust or correction, not booked — reconcile the position against the venue"
            ),
            Self::Restated => write!(
                f,
                "a restatement by the venue, which this edge has no kind for — \
                 reconcile the order against the venue"
            ),
            Self::Stopped => write!(
                f,
                "a stop by the venue, which this edge has no kind for — \
                 the order's fill will follow as an execution"
            ),
            Self::SessionReject => write!(
                f,
                "a session reject, which names a sequence number and no order — \
                 the request it refused is still in flight"
            ),
        }
    }
}

/// FIX 4.4 order entry, in a venue's [`Dialect`].
#[derive(Debug)]
pub struct FixCodec<D> {
    dialect: D,
    /// Whether this venue has already been logged for a commission it
    /// states and the dialect does not read. Per codec rather than per
    /// process, so one venue's unreadable fees do not silence another's;
    /// a clone starts from the original's state, so a venue wired through
    /// two clones logs at most twice.
    commission_unread: AtomicBool,
}

impl<D: Clone> Clone for FixCodec<D> {
    fn clone(&self) -> Self {
        FixCodec {
            dialect: self.dialect.clone(),
            commission_unread: AtomicBool::new(self.commission_unread.load(Ordering::Relaxed)),
        }
    }
}

impl<D> FixCodec<D> {
    /// A codec speaking `dialect`.
    pub const fn new(dialect: D) -> FixCodec<D> {
        FixCodec {
            dialect,
            commission_unread: AtomicBool::new(false),
        }
    }

    /// The dialect.
    pub const fn dialect(&self) -> &D {
        &self.dialect
    }

    /// Render one order-entry message, stamped `TransactTime` `at`.
    pub fn encode<I: Instrument>(&self, message: &Message<I>, at: NanoTime) -> FixMessage
    where
        D: Dialect<I>,
    {
        let d = &self.dialect;
        let id = |id: ClOrdId| d.cl_ord_id(id);
        let mut fields: Vec<(u32, String)> = Vec::with_capacity(12);
        let msg_type = match *message {
            Message::New(NewOrder {
                cl_ord_id,
                instrument,
                side,
                qty,
                price,
                kind,
                tif,
            }) => {
                fields.push((tag::CL_ORD_ID, id(cl_ord_id)));
                d.instrument(&instrument, &mut fields);
                fields.push((tag::SIDE, side_tag(side).into()));
                fields.push((tag::ORDER_QTY, qty.to_string()));
                order_kind(kind, price, &mut fields);
                fields.push((tag::TIME_IN_FORCE, tif_tag(tif).into()));
                "D"
            }
            Message::Replace {
                cl_ord_id,
                orig,
                instrument,
                side,
                kind,
                tif,
                price,
                qty,
            } => {
                fields.push((tag::CL_ORD_ID, id(cl_ord_id)));
                fields.push((tag::ORIG_CL_ORD_ID, id(orig)));
                d.instrument(&instrument, &mut fields);
                fields.push((tag::SIDE, side_tag(side).into()));
                fields.push((tag::ORDER_QTY, qty.to_string()));
                order_kind(kind, Some(price), &mut fields);
                fields.push((tag::TIME_IN_FORCE, tif_tag(tif).into()));
                "G"
            }
            Message::Cancel {
                cl_ord_id,
                orig,
                instrument,
                side,
                qty,
            } => {
                fields.push((tag::CL_ORD_ID, id(cl_ord_id)));
                fields.push((tag::ORIG_CL_ORD_ID, id(orig)));
                d.instrument(&instrument, &mut fields);
                fields.push((tag::SIDE, side_tag(side).into()));
                fields.push((tag::ORDER_QTY, qty.to_string()));
                "F"
            }
            Message::MassCancel { cl_ord_id } => {
                fields.push((tag::CL_ORD_ID, id(cl_ord_id)));
                // 7: cancel all orders.
                fields.push((tag::MASS_CANCEL_REQUEST_TYPE, "7".into()));
                "q"
            }
        };
        fields.push((tag::TRANSACT_TIME, utc_timestamp(at)));
        d.extra(message, &mut fields);
        FixMessage {
            msg_type: msg_type.into(),
            seq_num: 0,
            sending_time: NanoTime::ZERO,
            fields,
        }
    }

    /// Read one inbound message as an execution report, or `None` for one
    /// that answers no request (admin traffic, a status or pending report)
    /// — or that answers one but cannot be read, which is logged as an
    /// error naming what was wrong (see the module docs).
    pub fn decode<I: Instrument>(&self, msg: &FixMessage) -> Option<ExecReport<I>>
    where
        D: Dialect<I>,
    {
        match self.read(msg) {
            Ok(report) => report,
            Err(why) => {
                log::error!("fix order entry: dropped a report — {why}: {msg:?}");
                None
            }
        }
    }

    fn read<I: Instrument>(&self, msg: &FixMessage) -> Result<Option<ExecReport<I>>, Unread>
    where
        D: Dialect<I>,
    {
        let d = &self.dialect;
        let (id_tag, kind) = match msg.msg_type.as_str() {
            "9" => (tag::CL_ORD_ID, ExecKind::Rejected(d.cancel_reject(msg))),
            // A business reject of an order-entry message names it by
            // `BusinessRejectRefID`. One of anything else — a market-data
            // request, whose id may be a number too — is not this module's,
            // and read as an order's it would retire whichever order's id
            // the number happened to be.
            "j" => match msg.field(tag::REF_MSG_TYPE) {
                Some("D" | "G" | "F" | "q") => (
                    tag::BUSINESS_REJECT_REF_ID,
                    ExecKind::Rejected(RejectReason::Other),
                ),
                // Required by FIX 4.4; without it the refusal cannot be
                // told from one of a message that is not an order's.
                None => return Err(Unread::Field(tag::REF_MSG_TYPE)),
                Some(_) => return Ok(None),
            },
            "r" => match msg.field(tag::MASS_CANCEL_RESPONSE) {
                // Refused. An accepted mass cancel is answered order by
                // order, by each order's own cancel.
                Some("0") => (tag::CL_ORD_ID, ExecKind::Rejected(RejectReason::Other)),
                _ => return Ok(None),
            },
            "3" => return Err(Unread::SessionReject),
            "8" => {
                // FIX 4.2 states the transaction beside the execution:
                // only a new one is an execution; a cancel or correction of
                // an earlier one is not a second fill, and a status is none.
                match msg.field(tag::EXEC_TRANS_TYPE) {
                    None | Some("0") => {}
                    Some("3") => return Ok(None),
                    Some(_) => return Err(Unread::Correction),
                }
                let exec_type = msg
                    .field(tag::EXEC_TYPE)
                    .ok_or(Unread::Field(tag::EXEC_TYPE))?;
                let kind = match exec_type {
                    "0" => ExecKind::New {
                        // A venue echoes `44=0` on a market order: no price.
                        price: px(msg, tag::PRICE).ok().filter(|price| price.raw() > 0),
                        remaining: leaves(msg),
                    },
                    "5" => ExecKind::Replaced {
                        price: px(msg, tag::PRICE)?,
                        qty: qty(msg, tag::ORDER_QTY)?,
                    },
                    "4" => ExecKind::Canceled {
                        remaining: unfilled(msg),
                    },
                    "C" => ExecKind::Expired {
                        remaining: unfilled(msg),
                    },
                    // Done for day: no more fills *today*. A day order's end
                    // arrives as `C`; a good-till-cancel order is still
                    // working tomorrow, and read as ended here it would rest
                    // at the venue with nothing tracking it.
                    "3" => return Ok(None),
                    // The venue changed or held the order on its own
                    // authority. This edge has no kind for either, and the
                    // order's state is now the venue's to say.
                    "D" => return Err(Unread::Restated),
                    "7" => return Err(Unread::Stopped),
                    "8" => ExecKind::Rejected(d.order_reject(msg)),
                    "H" | "G" => return Err(Unread::Correction),
                    // `F` is FIX 4.4's trade; `1` / `2` are 4.2's partial and
                    // full fill, which some sessions still send.
                    "F" | "1" | "2" => ExecKind::Trade(self.trade(msg, exec_type == "2")?),
                    // Status and pending reports answer no request.
                    "I" | "6" | "E" | "A" => return Ok(None),
                    // A trigger fired (`L`), a calculation, a suspension —
                    // nothing on this edge; the order's state at the venue
                    // is what the next report says it is. Logged, because
                    // a report that changes an order and reaches nothing
                    // here is one to know about.
                    other => {
                        log::warn!(
                            "fix order entry: ExecType {other} has no kind on this edge; \
                             not read: {msg:?}"
                        );
                        return Ok(None);
                    }
                };
                (tag::CL_ORD_ID, kind)
            }
            _ => return Ok(None),
        };
        let orig = match msg.msg_type.as_str() {
            "8" | "9" => msg
                .field(tag::ORIG_CL_ORD_ID)
                .and_then(|text| d.parse_cl_ord_id(text)),
            _ => None,
        };
        let text = msg.field(id_tag).ok_or(Unread::Field(id_tag))?;
        let cl_ord_id = match d.parse_cl_ord_id(text) {
            Some(cl_ord_id) => cl_ord_id,
            // A refusal whose own id is unreadable — a venue echoing `NONE`
            // — is still the refusal of what is in flight on the order its
            // `OrigClOrdID` names, and the bridge attributes it that way.
            None if matches!(kind, ExecKind::Rejected(_)) && orig.is_some() => {
                log::warn!(
                    "fix order entry: a refusal on an unreadable ClOrdID {text:?}, \
                     attributed by OrigClOrdID"
                );
                ClOrdId::UNKNOWN
            }
            // An execution is never dropped quietly. A refusal with no
            // readable id at all may be another client's on a shared
            // session — nothing here could attribute it either way.
            None if matches!(kind, ExecKind::Trade(_)) => return Err(Unread::ClOrdId),
            None => {
                // An order this process never sent — the last one's, say —
                // and no execution or refusal on it: nothing here to book.
                log::debug!("fix order entry: a report on ClOrdID {text:?}, not ours");
                return Ok(None);
            }
        };
        let venue_time = msg
            .field(tag::TRANSACT_TIME)
            .and_then(parse_sending_time)
            .unwrap_or(msg.sending_time);
        // The venue's own id, where it names one. One longer than `VenueId`
        // holds is not truncated — a truncated id names the wrong order at
        // settlement — but left off, and logged.
        let venue_id = msg
            .field(tag::ORDER_ID)
            .and_then(|text| match VenueId::new(text) {
                Ok(id) => Some(id),
                Err(why) => {
                    log::warn!("fix order entry: OrderID {text:?} not carried — {why}");
                    None
                }
            });
        Ok(Some(ExecReport {
            cl_ord_id,
            orig,
            venue_id,
            kind,
            venue_time,
        }))
    }

    /// One execution. `done` is FIX 4.2's `150=2`: the order is filled, so
    /// nothing is left whatever the report omits.
    fn trade<I: Instrument>(&self, msg: &FixMessage, done: bool) -> Result<Trade<I>, Unread>
    where
        D: Dialect<I>,
    {
        let exec_id = msg.field(tag::EXEC_ID).ok_or(Unread::Field(tag::EXEC_ID))?;
        let filled = qty(msg, tag::CUM_QTY)?;
        let remaining = match qty(msg, tag::LEAVES_QTY) {
            Ok(leaves) => leaves,
            Err(_) if done => Qty::ZERO,
            Err(_) => match qty(msg, tag::ORDER_QTY) {
                Ok(total) if total >= filled => total - filled,
                _ => return Err(Unread::Remaining),
            },
        };
        let last_qty = qty(msg, tag::LAST_QTY)?;
        let last_px = px(msg, tag::LAST_PX)?;
        Ok(Trade {
            exec_id: ExecId::new(exec_id).map_err(|_| Unread::ExecId)?,
            instrument: self.dialect.instrument_of(msg).ok_or(Unread::Instrument)?,
            side: side_of(msg)?,
            qty: last_qty,
            filled,
            remaining,
            price: last_px,
            liquidity: match msg.field(tag::LAST_LIQUIDITY_IND) {
                Some("1") => Liquidity::Maker,
                _ => Liquidity::Taker,
            },
            fee: self.fee(msg, last_qty, last_px),
        })
    }

    /// The fee the dialect reads off `msg`, zero where it reads none — and,
    /// the first time this venue states a commission the dialect does not
    /// read, one warning: a fee silently booked as zero is a fold short of
    /// it, and one line per venue says so where one per fill would bury it.
    fn fee<I: Instrument>(&self, msg: &FixMessage, qty: Qty, price: Px) -> Qty
    where
        D: Dialect<I>,
    {
        match self.dialect.commission(msg, qty, price) {
            Some(fee) => fee,
            None => {
                if let Some(text) = msg.field(tag::COMMISSION)
                    && !self.commission_unread.swap(true, Ordering::Relaxed)
                {
                    log::warn!(
                        "fix order entry: this venue states Commission {text} (CommType {:?}, \
                         CommCurrency {:?}) and the dialect does not read it; its fees are \
                         booked as zero (logged once)",
                        msg.field(tag::COMM_TYPE),
                        msg.field(tag::COMM_CURRENCY),
                    );
                }
                Qty::ZERO
            }
        }
    }
}

/// A FIX session as an execution-layer [`Venue`].
///
/// [`wire`](Venue::wire) puts [`FixOps::fix_bridge_sending`] on the
/// requests, with `inbound` decoded into its executions and every message
/// it renders encoded onto `sender` — a non-blocking push onto the
/// session's outbound queue, stamped `TransactTime` at the engine time. The
/// venue's answers arrive on `inbound`, cycles later; nothing waits. A
/// message the session will not take is answered as refused in the same
/// instant, inside the bridge's node (module docs, *Nothing is dropped
/// quietly*), so the venue wires no edge of its own.
///
/// The session states reports and the trading state, and nothing else: the
/// other five [`Session`] streams are silent. The trading state is the FIX
/// session's own — [`Halted`](TradingState::Halted) from the start and
/// while it is not logged in (disconnected, logging in, logged out, or in
/// error), [`TradingState::Open`] while it is — so the OMS holds its places
/// and amends until the logon and while the link is down, sends only
/// cancels meanwhile, and sends what is still wanted on the first diff
/// after the logon. A sequence gap changes it not
/// at all: the session is up, and the resend it has asked for is what
/// closes the gap.
///
/// # What a dropped link leaves
///
/// A message the session had queued when its socket write failed is gone
/// with the reconnect (`FixSender::send` returning `Ok` means the queue took
/// it, not the wire), and nothing here answers it: the slot it was sent for
/// waits on the venue's own state. So does every order the venue still
/// works from before the drop. Neither has a timeout on this edge, and a
/// resting order the venue knows under a `ClOrdID` this process did not
/// mint has no representation at all — a report on one is logged and
/// dropped (module docs, *Nothing is dropped quietly*). An integration
/// that reconnects into live orders asks the venue for them (an
/// `OrderStatusRequest`, or a mass status) and reconciles positions
/// against [`Session::holdings`]; this venue carries neither yet.
pub struct FixOrderVenue<D> {
    /// The order session's inbound messages.
    pub inbound: Stream<Burst<FixMessage>>,
    /// The order session's status, which is this venue's trading state.
    pub status: Stream<Burst<FixSessionStatus>>,
    /// The order session's outbound queue.
    pub sender: FixSender,
    /// The process's epoch, which the bridge mints its `ClOrdID`s under.
    pub epoch: Epoch,
    /// Whether the venue has an OrderMassCancelRequest; without one a
    /// cancel-all goes out as one cancel per live order.
    pub mass_cancel: bool,
    /// The codec, in the venue's dialect.
    pub codec: FixCodec<D>,
}

impl<I, D> Venue<I> for FixOrderVenue<D>
where
    I: Instrument + 'static,
    D: Dialect<I>,
{
    fn wire(&self, requests: &Stream<Burst<Request<I>>>) -> Session<I> {
        let codec = self.codec.clone();
        let executions = self.inbound.filter_map(move |ms: &Burst<FixMessage>| {
            let executions: Burst<ExecReport<I>> =
                ms.iter().filter_map(|m| codec.decode(m)).collect();
            (!executions.is_empty()).then_some(executions)
        });
        let codec = self.codec.clone();
        let sender = self.sender.clone();
        let reports = requests.fix_bridge_sending(
            self.mass_cancel,
            self.epoch,
            &executions,
            move |message: &Message<I>, now: NanoTime| {
                sender.send(codec.encode(message, now)).map_err(|e| {
                    log::error!("fix order entry: the session refused {message:?}: {e}");
                    RejectReason::Other
                })
            },
        );
        // The venue starts halted — the OMS starts open, and a session not
        // yet connected is not a venue to place on — and from then on the
        // last status of each instant that says anything about whether the
        // session is up decides; a sequence gap says nothing about it.
        let g = self.status.graph();
        let stated = self
            .status
            .filter_map(|statuses: &Burst<FixSessionStatus>| {
                statuses.iter().rev().find_map(|status| match status {
                    FixSessionStatus::LoggedIn => Some(TradingState::Open),
                    FixSessionStatus::Disconnected
                    | FixSessionStatus::LoggingIn
                    | FixSessionStatus::LoggedOut(_)
                    | FixSessionStatus::Error(_) => Some(TradingState::Halted),
                    FixSessionStatus::SequenceGap { .. } => None,
                })
            });
        let trading = g.combine(&[g.constant(TradingState::Halted), stated]).map(
            |states: &Burst<TradingState>| {
                *states
                    .last()
                    .expect("invariant: a combined burst ticks only with a value")
            },
        );
        Session::quiet(reports).with_trading(trading)
    }
}

/// `OrdType`, `Price` and, for post-only, `ExecInst` `6`
/// (participate don't initiate).
fn order_kind(kind: OrderKind, price: Option<Px>, fields: &mut Vec<(u32, String)>) {
    match kind {
        OrderKind::Market => fields.push((tag::ORD_TYPE, "1".into())),
        OrderKind::Limit | OrderKind::PostOnly => {
            fields.push((tag::ORD_TYPE, "2".into()));
            if let Some(price) = price {
                fields.push((tag::PRICE, price.to_string()));
            }
            if kind == OrderKind::PostOnly {
                fields.push((tag::EXEC_INST, "6".into()));
            }
        }
    }
}

/// What rests after an accept, where the report says: `LeavesQty`, else
/// `OrderQty − CumQty` where it states both, else `OrderQty` alone (nothing
/// has filled on a new order), else nothing.
///
/// Not [`unfilled`], deliberately: on a `150=0` `LeavesQty` *is* what rests,
/// and is preferred; on an order's end `LeavesQty` is zero by definition
/// and says nothing about what was showing, so [`unfilled`] prefers the
/// difference. Two notions, each with the precedence its report calls for.
fn leaves(msg: &FixMessage) -> Option<Qty> {
    if let Ok(leaves) = qty(msg, tag::LEAVES_QTY) {
        return Some(leaves);
    }
    match (qty(msg, tag::ORDER_QTY), qty(msg, tag::CUM_QTY)) {
        (Ok(total), Ok(filled)) if total >= filled => Some(total - filled),
        (Ok(total), Err(_)) => Some(total),
        _ => None,
    }
}

/// What was left showing when an order ended: `OrderQty − CumQty` where the
/// report states both, else `LeavesQty`, else nothing. See [`leaves`] for
/// why the precedence is the other way round from an accept's.
fn unfilled(msg: &FixMessage) -> Qty {
    match (qty(msg, tag::ORDER_QTY), qty(msg, tag::CUM_QTY)) {
        (Ok(total), Ok(filled)) if total >= filled => total - filled,
        _ => qty(msg, tag::LEAVES_QTY).unwrap_or(Qty::ZERO),
    }
}

/// A trade's side. Buy-minus is a buy; sell-plus and the short sells are
/// sells; anything else is not read as either.
fn side_of(msg: &FixMessage) -> Result<Side, Unread> {
    match msg.field(tag::SIDE) {
        Some("1" | "3") => Ok(Side::Bid),
        Some("2" | "4" | "5" | "6") => Ok(Side::Ask),
        _ => Err(Unread::Side),
    }
}

fn qty(msg: &FixMessage, tag: u32) -> Result<Qty, Unread> {
    msg.field(tag)
        .and_then(|text| Qty::parse(text).ok())
        .ok_or(Unread::Field(tag))
}

fn px(msg: &FixMessage, tag: u32) -> Result<Px, Unread> {
    msg.field(tag)
        .and_then(|text| Px::parse(text).ok())
        .ok_or(Unread::Field(tag))
}

const fn side_tag(side: Side) -> &'static str {
    match side {
        Side::Bid => "1",
        Side::Ask => "2",
    }
}

const fn tif_tag(tif: TimeInForce) -> &'static str {
    match tif {
        TimeInForce::Day => "0",
        TimeInForce::GoodTillCancel => "1",
        TimeInForce::ImmediateOrCancel => "3",
        TimeInForce::FillOrKill => "4",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::execution::edge::Report;
    use crate::trading::execution::order::{ClientOrderId, Order};
    use crate::{RunFor, RunMode};

    /// Two listed stocks, named by `Symbol`.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
    enum Stock {
        #[default]
        Aapl,
        Msft,
    }

    #[derive(Clone, Debug)]
    struct Exchange;

    impl Dialect<Stock> for Exchange {
        fn instrument(&self, instrument: &Stock, fields: &mut Vec<(u32, String)>) {
            let symbol = match instrument {
                Stock::Aapl => "AAPL",
                Stock::Msft => "MSFT",
            };
            fields.push((tag::SYMBOL, symbol.into()));
        }

        fn instrument_of(&self, report: &FixMessage) -> Option<Stock> {
            match report.field(tag::SYMBOL)? {
                "AAPL" => Some(Stock::Aapl),
                "MSFT" => Some(Stock::Msft),
                _ => None,
            }
        }

        fn extra(&self, _: &Message<Stock>, fields: &mut Vec<(u32, String)>) {
            fields.push((1, "ACC-1".into()));
        }
    }

    fn codec() -> FixCodec<Exchange> {
        FixCodec::new(Exchange)
    }

    fn px(s: &str) -> Px {
        Px::parse(s).unwrap()
    }

    fn qty(s: &str) -> Qty {
        Qty::parse(s).unwrap()
    }

    fn fix(msg_type: &str, fields: &[(u32, &str)]) -> FixMessage {
        FixMessage {
            msg_type: msg_type.into(),
            seq_num: 1,
            sending_time: NanoTime::new(7),
            fields: fields.iter().map(|(t, v)| (*t, v.to_string())).collect(),
        }
    }

    /// 2024-06-27 11:17:25.223 UTC.
    const AT_NANOS: u64 = 1_719_487_045_223_000_000;

    #[test]
    fn a_new_order_states_the_instrument_in_the_dialect_and_the_rest_as_fix() {
        let msg = codec().encode(
            &Message::New(NewOrder {
                cl_ord_id: ClOrdId(12),
                instrument: Stock::Msft,
                side: Side::Ask,
                qty: qty("300"),
                price: Some(px("421.15")),
                kind: OrderKind::Limit,
                tif: TimeInForce::Day,
            }),
            NanoTime::new(AT_NANOS),
        );
        assert_eq!(msg.msg_type, "D");
        assert_eq!(
            msg.fields,
            [
                (tag::CL_ORD_ID, "12".to_string()),
                (tag::SYMBOL, "MSFT".into()),
                (tag::SIDE, "2".into()),
                (tag::ORDER_QTY, "300".into()),
                (tag::ORD_TYPE, "2".into()),
                (tag::PRICE, "421.15".into()),
                (tag::TIME_IN_FORCE, "0".into()),
                (tag::TRANSACT_TIME, "20240627-11:17:25.223".into()),
                (1, "ACC-1".into()),
            ]
        );
    }

    #[test]
    fn post_only_is_a_limit_with_exec_inst_6_and_a_market_order_has_no_price() {
        let new = |kind, price| {
            codec().encode(
                &Message::New(NewOrder {
                    cl_ord_id: ClOrdId(1),
                    instrument: Stock::Aapl,
                    side: Side::Bid,
                    qty: qty("1"),
                    price,
                    kind,
                    tif: TimeInForce::GoodTillCancel,
                }),
                NanoTime::new(AT_NANOS),
            )
        };
        let post = new(OrderKind::PostOnly, Some(px("190")));
        assert_eq!(post.field(tag::ORD_TYPE), Some("2"));
        assert_eq!(post.field(tag::EXEC_INST), Some("6"));
        let market = new(OrderKind::Market, None);
        assert_eq!(market.field(tag::ORD_TYPE), Some("1"));
        assert_eq!(market.field(tag::PRICE), None);
        assert_eq!(market.field(tag::EXEC_INST), None);
    }

    #[test]
    fn a_replace_and_a_cancel_name_the_order_they_act_on() {
        let replace = codec().encode(
            &Message::Replace {
                cl_ord_id: ClOrdId(5),
                orig: ClOrdId(4),
                instrument: Stock::Aapl,
                side: Side::Bid,
                kind: OrderKind::Limit,
                tif: TimeInForce::ImmediateOrCancel,
                price: px("189.9"),
                qty: qty("10"),
            },
            NanoTime::new(AT_NANOS),
        );
        assert_eq!(replace.msg_type, "G");
        assert_eq!(replace.field(tag::ORIG_CL_ORD_ID), Some("4"));
        assert_eq!(replace.field(tag::TIME_IN_FORCE), Some("3"));
        let cancel = codec().encode(
            &Message::Cancel {
                cl_ord_id: ClOrdId(6),
                orig: ClOrdId(5),
                instrument: Stock::Aapl,
                side: Side::Bid,
                qty: qty("10"),
            },
            NanoTime::new(AT_NANOS),
        );
        assert_eq!(cancel.msg_type, "F");
        assert_eq!(cancel.field(tag::ORDER_QTY), Some("10"));
        assert_eq!(cancel.field(tag::CL_ORD_ID), Some("6"));
        assert_eq!(cancel.field(tag::ORIG_CL_ORD_ID), Some("5"));
        assert_eq!(cancel.field(tag::SYMBOL), Some("AAPL"));
        let mass = codec().encode(
            &Message::MassCancel {
                cl_ord_id: ClOrdId(7),
            },
            NanoTime::new(AT_NANOS),
        );
        assert_eq!(mass.msg_type, "q");
        assert_eq!(mass.field(tag::MASS_CANCEL_REQUEST_TYPE), Some("7"));
    }

    #[test]
    fn a_trade_decodes_with_the_venues_cumulative_quantity() {
        let report = codec()
            .decode(&fix(
                "8",
                &[
                    (tag::CL_ORD_ID, "3"),
                    (tag::EXEC_TYPE, "F"),
                    (tag::EXEC_ID, "X1"),
                    (tag::SYMBOL, "AAPL"),
                    (tag::SIDE, "1"),
                    (tag::LAST_QTY, "2"),
                    (tag::CUM_QTY, "5"),
                    (tag::LEAVES_QTY, "1"),
                    (tag::LAST_PX, "190.01"),
                    (tag::LAST_LIQUIDITY_IND, "1"),
                ],
            ))
            .unwrap();
        assert_eq!(report.cl_ord_id, ClOrdId(3));
        assert_eq!(report.venue_time, NanoTime::new(7));
        assert_eq!(
            report.kind,
            ExecKind::Trade(Trade {
                exec_id: ExecId::new("X1").unwrap(),
                instrument: Stock::Aapl,
                side: Side::Bid,
                qty: qty("2"),
                filled: qty("5"),
                remaining: qty("1"),
                price: px("190.01"),
                liquidity: Liquidity::Maker,
                fee: Qty::ZERO,
            })
        );
    }

    #[test]
    fn a_trade_on_an_instrument_the_dialect_does_not_trade_is_dropped() {
        let msg = fix(
            "8",
            &[
                (tag::CL_ORD_ID, "3"),
                (tag::EXEC_TYPE, "F"),
                (tag::EXEC_ID, "X1"),
                (tag::SYMBOL, "TSLA"),
                (tag::SIDE, "1"),
                (tag::LAST_QTY, "2"),
                (tag::CUM_QTY, "2"),
                (tag::LAST_PX, "1"),
            ],
        );
        assert_eq!(codec().decode(&msg), None);
    }

    #[test]
    fn the_ends_of_an_order_decode_with_what_was_left() {
        let canceled = codec()
            .decode(&fix(
                "8",
                &[
                    (tag::CL_ORD_ID, "9"),
                    (tag::ORIG_CL_ORD_ID, "8"),
                    (tag::EXEC_TYPE, "4"),
                    (tag::ORDER_QTY, "10"),
                    (tag::CUM_QTY, "4"),
                    (tag::LEAVES_QTY, "0"),
                ],
            ))
            .unwrap();
        assert_eq!(canceled.orig, Some(ClOrdId(8)));
        assert_eq!(
            canceled.kind,
            ExecKind::Canceled {
                remaining: qty("6")
            }
        );
        let expired = codec()
            .decode(&fix(
                "8",
                &[
                    (tag::CL_ORD_ID, "3"),
                    (tag::EXEC_TYPE, "C"),
                    (tag::LEAVES_QTY, "2"),
                ],
            ))
            .unwrap();
        assert_eq!(
            expired.kind,
            ExecKind::Expired {
                remaining: qty("2")
            }
        );
    }

    #[test]
    fn rejects_map_through_the_dialect() {
        let order = |code| {
            codec()
                .decode(&fix(
                    "8",
                    &[
                        (tag::CL_ORD_ID, "3"),
                        (tag::EXEC_TYPE, "8"),
                        (tag::ORD_REJ_REASON, code),
                    ],
                ))
                .unwrap()
                .kind
        };
        assert_eq!(order("5"), ExecKind::Rejected(RejectReason::UnknownOrder));
        assert_eq!(order("3"), ExecKind::Rejected(RejectReason::RiskLimit));
        assert_eq!(order("99"), ExecKind::Rejected(RejectReason::Other));
        let cancel = codec()
            .decode(&fix(
                "9",
                &[
                    (tag::CL_ORD_ID, "9"),
                    (tag::ORIG_CL_ORD_ID, "8"),
                    (tag::CXL_REJ_REASON, "1"),
                ],
            ))
            .unwrap();
        assert_eq!(cancel.kind, ExecKind::Rejected(RejectReason::UnknownOrder));
    }

    #[test]
    fn admin_status_and_pending_traffic_are_not_execution_reports() {
        assert_eq!(codec().decode::<Stock>(&fix("0", &[])), None);
        for exec_type in ["I", "6", "E", "A"] {
            let msg = fix("8", &[(tag::CL_ORD_ID, "3"), (tag::EXEC_TYPE, exec_type)]);
            assert_eq!(codec().decode(&msg), None, "150={exec_type}");
        }
        let foreign = fix("8", &[(tag::CL_ORD_ID, "abc"), (tag::EXEC_TYPE, "4")]);
        assert_eq!(codec().decode(&foreign), None);
    }

    /// The fields of a fill, for the tests that vary one of them.
    fn fill(extra: &[(u32, &'static str)]) -> FixMessage {
        let mut fields = vec![
            (tag::CL_ORD_ID, "3"),
            (tag::EXEC_TYPE, "F"),
            (tag::EXEC_ID, "X1"),
            (tag::SYMBOL, "AAPL"),
            (tag::SIDE, "1"),
            (tag::LAST_QTY, "2"),
            (tag::CUM_QTY, "2"),
            (tag::LAST_PX, "190"),
        ];
        fields.extend_from_slice(extra);
        fix("8", &fields)
    }

    fn trade_of(report: Option<ExecReport<Stock>>) -> Trade<Stock> {
        match report.map(|r| r.kind) {
            Some(ExecKind::Trade(trade)) => trade,
            other => panic!("not a trade: {other:?}"),
        }
    }

    /// A partial fill without `LeavesQty` is never read as the order's end:
    /// what is left is `OrderQty − CumQty` where the report states the
    /// total, and with neither the report is not read at all. A 4.2 full
    /// fill (`150=2`) has nothing left whatever it omits.
    #[test]
    fn a_fill_without_leaves_qty_is_not_the_orders_end() {
        let partial = codec().decode(&fill(&[(tag::ORDER_QTY, "10")]));
        assert_eq!(trade_of(partial).remaining, qty("8"));
        assert_eq!(codec().decode::<Stock>(&fill(&[])), None);
        let mut full = fill(&[]);
        full.fields[1].1 = "2".into();
        assert_eq!(trade_of(codec().decode(&full)).remaining, Qty::ZERO);
    }

    /// An execution with a field it cannot be read without — or an
    /// `ExecID` longer than `ExecId` holds — is not decoded at all.
    #[test]
    fn an_execution_that_cannot_be_read_is_not_decoded() {
        let long = "X".repeat(ExecId::CAPACITY + 1);
        let mut msg = fill(&[(tag::LEAVES_QTY, "0")]);
        msg.fields[2].1 = long;
        assert_eq!(codec().decode::<Stock>(&msg), None);
        let mut msg = fill(&[(tag::LEAVES_QTY, "0")]);
        msg.fields.retain(|(t, _)| *t != tag::CUM_QTY);
        assert_eq!(codec().decode::<Stock>(&msg), None);
        let mut msg = fill(&[(tag::LEAVES_QTY, "0")]);
        msg.fields[0].1 = "not-ours".into();
        assert_eq!(codec().decode::<Stock>(&msg), None);
    }

    /// Buy-minus is a buy and the short sells are sells; a side that is
    /// neither is not read as one.
    #[test]
    fn a_trades_side_is_read_not_guessed() {
        let side = |code: &'static str| {
            let mut msg = fill(&[(tag::LEAVES_QTY, "0")]);
            msg.fields[4].1 = code.into();
            codec()
                .decode::<Stock>(&msg)
                .map(|r| trade_of(Some(r)).side)
        };
        assert_eq!(side("3"), Some(Side::Bid));
        assert_eq!(side("5"), Some(Side::Ask));
        assert_eq!(side("9"), None);
    }

    /// A bust or correction is never booked as a fill — a correction read
    /// as a new execution would count it twice — and a 4.2 status is not
    /// an execution at all.
    #[test]
    fn a_bust_or_correction_is_not_a_second_fill() {
        for (exec_type, trans) in [("H", "0"), ("G", "0"), ("2", "1"), ("1", "2"), ("2", "3")] {
            let mut msg = fill(&[(tag::LEAVES_QTY, "0"), (tag::EXEC_TRANS_TYPE, trans)]);
            msg.fields[1].1 = exec_type.into();
            assert_eq!(
                codec().decode::<Stock>(&msg),
                None,
                "150={exec_type} 20={trans}"
            );
        }
        let new = fill(&[(tag::LEAVES_QTY, "0"), (tag::EXEC_TRANS_TYPE, "0")]);
        assert!(codec().decode::<Stock>(&new).is_some());
    }

    /// The venue's time is the execution's `TransactTime`, and the
    /// header's `SendingTime` only where the report states none.
    #[test]
    fn venue_time_is_the_transact_time() {
        let msg = fill(&[
            (tag::LEAVES_QTY, "0"),
            (tag::TRANSACT_TIME, "20240627-11:17:25.223"),
        ]);
        let report = codec().decode::<Stock>(&msg).unwrap();
        assert_eq!(report.venue_time, NanoTime::new(AT_NANOS));
        let report = codec()
            .decode::<Stock>(&fill(&[(tag::LEAVES_QTY, "0")]))
            .unwrap();
        assert_eq!(report.venue_time, NanoTime::new(7));
    }

    /// A business reject names the refused message by `BusinessRejectRefID`
    /// and is its refusal; a refused mass cancel is the mass cancel's; an
    /// accepted one, and a session reject (which names no order), are not
    /// reports.
    #[test]
    fn business_and_mass_cancel_rejects_answer_the_request() {
        let business = codec()
            .decode::<Stock>(&fix(
                "j",
                &[(tag::REF_MSG_TYPE, "D"), (tag::BUSINESS_REJECT_REF_ID, "4")],
            ))
            .unwrap();
        assert_eq!(business.cl_ord_id, ClOrdId(4));
        assert_eq!(business.kind, ExecKind::Rejected(RejectReason::Other));
        assert_eq!(codec().decode::<Stock>(&fix("j", &[])), None);
        // A market-data request's id is a number too, and names no order.
        let market_data = fix(
            "j",
            &[(tag::REF_MSG_TYPE, "V"), (tag::BUSINESS_REJECT_REF_ID, "4")],
        );
        assert_eq!(codec().decode::<Stock>(&market_data), None);

        let refused = codec()
            .decode::<Stock>(&fix(
                "r",
                &[(tag::CL_ORD_ID, "7"), (tag::MASS_CANCEL_RESPONSE, "0")],
            ))
            .unwrap();
        assert_eq!(refused.cl_ord_id, ClOrdId(7));
        assert_eq!(refused.kind, ExecKind::Rejected(RejectReason::Other));
        let accepted = fix(
            "r",
            &[(tag::CL_ORD_ID, "7"), (tag::MASS_CANCEL_RESPONSE, "7")],
        );
        assert_eq!(codec().decode::<Stock>(&accepted), None);

        let session = fix("3", &[(tag::REF_SEQ_NUM, "12")]);
        assert_eq!(codec().decode::<Stock>(&session), None);
    }

    /// Done for day is not an end — a good-till-cancel order works on
    /// tomorrow — and a restatement or a stop has no kind on this edge; none
    /// is decoded.
    #[test]
    fn done_for_day_is_not_an_end_and_a_restatement_is_not_guessed_at() {
        let report = |exec_type: &'static str| {
            codec().decode::<Stock>(&fix(
                "8",
                &[
                    (tag::CL_ORD_ID, "3"),
                    (tag::EXEC_TYPE, exec_type),
                    (tag::ORDER_QTY, "10"),
                    (tag::CUM_QTY, "4"),
                ],
            ))
        };
        // Done for day leaves a good-till-cancel order working.
        assert_eq!(report("3"), None);
        assert_eq!(report("D"), None);
        assert_eq!(report("7"), None);
    }

    /// A refusal whose own `ClOrdID` cannot be read is attributed by its
    /// `OrigClOrdID`, under `ClOrdId::UNKNOWN`; one with neither readable
    /// is not decoded.
    #[test]
    fn a_refusal_on_an_unreadable_id_is_attributed_by_orig() {
        let refusal = codec()
            .decode::<Stock>(&fix(
                "9",
                &[(tag::CL_ORD_ID, "NONE"), (tag::ORIG_CL_ORD_ID, "8")],
            ))
            .unwrap();
        assert_eq!(refusal.cl_ord_id, ClOrdId::UNKNOWN);
        assert_eq!(refusal.orig, Some(ClOrdId(8)));
        let neither = fix("9", &[(tag::CL_ORD_ID, "NONE"), (tag::ORIG_CL_ORD_ID, "x")]);
        assert_eq!(codec().decode::<Stock>(&neither), None);
    }

    /// A place the session will not take — its queue closed — is answered
    /// as refused in the same instant, so the OMS's slot goes idle rather
    /// than waiting on a report that will never come.
    #[test]
    fn a_message_the_session_refuses_is_answered_as_a_reject() {
        let (tx, rx) = kanal::bounded::<FixMessage>(1);
        drop(rx);
        let g = GraphBuilder::new();
        let order = ClientOrderId(1);
        let place = Request::Place(Order::limit(
            order,
            Stock::Aapl,
            Side::Bid,
            qty("100"),
            px("189.95"),
        ));
        let requests = g.replay([(place, NanoTime::new(AT_NANOS))]);
        let venue = FixOrderVenue {
            inbound: silent(&g),
            status: silent(&g),
            sender: FixSender { sender: tx },
            epoch: Epoch::ZERO,
            mass_cancel: false,
            codec: codec(),
        };
        let reports = venue.wire(&requests).reports.with_time().accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();

        let reports = runner.value(&reports);
        assert_eq!(reports.len(), 1, "{reports:?}");
        let (at, burst) = &reports[0];
        assert_eq!(*at, NanoTime::new(AT_NANOS), "answered in the same instant");
        assert!(
            matches!(
                burst.as_slice(),
                [Report::Reject(reject)] if reject.order == Some(order)
            ),
            "{burst:?}"
        );
    }

    /// The venue on a graph: a place goes out on the sender as a
    /// NewOrderSingle stamped at its engine instant, and the venue's ack,
    /// arriving on the inbound stream later, comes back as the OMS's report
    /// on the OMS's own id.
    #[test]
    fn the_venue_sends_on_the_session_and_reports_what_comes_back() {
        let (tx, rx) = kanal::bounded::<FixMessage>(16);
        let g = GraphBuilder::new();
        let order = ClientOrderId(1);
        let place = Request::Place(Order::limit(
            order,
            Stock::Aapl,
            Side::Bid,
            qty("100"),
            px("189.95"),
        ));
        let requests = g.replay([(place, NanoTime::new(AT_NANOS))]);
        let ack = fix("8", &[(tag::CL_ORD_ID, "1"), (tag::EXEC_TYPE, "0")]);
        let later = NanoTime::new(u64::from(NanoTime::new(AT_NANOS)) + 1_000_000);
        let inbound = g.replay([(ack, later)]);
        let venue = FixOrderVenue {
            inbound,
            status: silent(&g),
            sender: FixSender { sender: tx },
            epoch: Epoch::ZERO,
            mass_cancel: false,
            codec: codec(),
        };
        let reports = venue.wire(&requests).reports.with_time().accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();

        let sent: Vec<FixMessage> = std::iter::from_fn(|| rx.try_recv().ok().flatten()).collect();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].msg_type, "D");
        assert_eq!(sent[0].field(tag::CL_ORD_ID), Some("1"));
        assert_eq!(
            sent[0].field(tag::TRANSACT_TIME),
            Some("20240627-11:17:25.223")
        );

        let reports = runner.value(&reports);
        assert_eq!(reports.len(), 1, "{reports:?}");
        let (at, burst) = &reports[0];
        assert_eq!(*at, later);
        assert!(
            matches!(burst.as_slice(), [Report::Ack(ack)] if ack.order == order),
            "{burst:?}"
        );
    }

    /// A burst stream that never ticks: a session that never speaks, an
    /// inbound side that never answers.
    fn silent<T: Clone + Default + 'static>(g: &GraphBuilder) -> Stream<Burst<T>> {
        g.never()
            .map(|_: &()| Burst::<T>::new())
            .filter_map(|_: &Burst<T>| None::<Burst<T>>)
    }

    /// A new order's report carries the venue's `OrderID` and what the
    /// venue rests — its price and `LeavesQty` — where it states them, and
    /// `None` where it does not. An `OrderID` longer than a `VenueId` holds
    /// is left off, never truncated.
    #[test]
    fn a_new_carries_the_venues_order_id_and_what_rests() {
        let stated = codec()
            .decode::<Stock>(&fix(
                "8",
                &[
                    (tag::CL_ORD_ID, "3"),
                    (tag::EXEC_TYPE, "0"),
                    (tag::ORDER_ID, "V-9"),
                    (tag::PRICE, "189.90"),
                    (tag::ORDER_QTY, "100"),
                    (tag::CUM_QTY, "0"),
                    (tag::LEAVES_QTY, "90"),
                ],
            ))
            .unwrap();
        assert_eq!(stated.venue_id, Some(VenueId::new("V-9").unwrap()));
        assert_eq!(
            stated.kind,
            ExecKind::New {
                price: Some(px("189.90")),
                remaining: Some(qty("90")),
            }
        );
        let bare = codec()
            .decode::<Stock>(&fix("8", &[(tag::CL_ORD_ID, "3"), (tag::EXEC_TYPE, "0")]))
            .unwrap();
        assert_eq!(bare.venue_id, None);
        assert_eq!(bare.kind, ExecKind::NEW);
        let from_total = codec()
            .decode::<Stock>(&fix(
                "8",
                &[
                    (tag::CL_ORD_ID, "3"),
                    (tag::EXEC_TYPE, "0"),
                    (tag::ORDER_QTY, "100"),
                ],
            ))
            .unwrap();
        assert_eq!(
            from_total.kind,
            ExecKind::New {
                price: None,
                remaining: Some(qty("100")),
            }
        );
        let market = codec()
            .decode::<Stock>(&fix(
                "8",
                &[
                    (tag::CL_ORD_ID, "3"),
                    (tag::EXEC_TYPE, "0"),
                    (tag::PRICE, "0"),
                ],
            ))
            .unwrap();
        assert_eq!(
            market.kind,
            ExecKind::NEW,
            "`44=0` on a market order is no price"
        );
        let long = "V".repeat(VenueId::CAPACITY + 1);
        let too_long = codec()
            .decode::<Stock>(&fix(
                "8",
                &[
                    (tag::CL_ORD_ID, "3"),
                    (tag::EXEC_TYPE, "0"),
                    (tag::ORDER_ID, &long),
                ],
            ))
            .unwrap();
        assert_eq!(too_long.venue_id, None);
    }

    /// A fill's fee is the report's `Commission`, read as `CommType` says:
    /// an amount, an amount per unit, or a percentage of the notional. A
    /// type with no reading here, or no type, is not read.
    #[test]
    fn a_fills_fee_is_the_commission_as_its_type_says() {
        let fee = |extra: &[(u32, &'static str)]| trade_of(codec().decode(&fill(extra))).fee;
        assert_eq!(fee(&[(tag::LEAVES_QTY, "0")]), Qty::ZERO, "none stated");
        assert_eq!(
            fee(&[
                (tag::LEAVES_QTY, "0"),
                (tag::COMMISSION, "0.5"),
                (tag::COMM_TYPE, "3")
            ]),
            qty("0.5"),
            "an amount"
        );
        assert_eq!(
            fee(&[
                (tag::LEAVES_QTY, "0"),
                (tag::COMMISSION, "0.01"),
                (tag::COMM_TYPE, "1")
            ]),
            qty("0.02"),
            "per unit, two units"
        );
        assert_eq!(
            fee(&[
                (tag::LEAVES_QTY, "0"),
                (tag::COMMISSION, "0.001"),
                (tag::COMM_TYPE, "2")
            ]),
            qty("0.38"),
            "a tenth of a percent of 2 × 190, stated as FIX states it: a fraction"
        );
        assert_eq!(
            fee(&[
                (tag::LEAVES_QTY, "0"),
                (tag::COMMISSION, "0.5"),
                (tag::COMM_TYPE, "3"),
                (tag::COMM_CURRENCY, "USD"),
            ]),
            Qty::ZERO,
            "a currency the dialect has not vouched for: not read"
        );
        assert_eq!(
            fee(&[
                (tag::LEAVES_QTY, "0"),
                (tag::COMMISSION, "-0.25"),
                (tag::COMM_TYPE, "3")
            ]),
            qty("-0.25"),
            "a rebate"
        );
        assert_eq!(
            fee(&[
                (tag::LEAVES_QTY, "0"),
                (tag::COMMISSION, "7"),
                (tag::COMM_TYPE, "6")
            ]),
            Qty::ZERO,
            "points per bond: no reading here"
        );
        assert_eq!(
            fee(&[(tag::LEAVES_QTY, "0"), (tag::COMMISSION, "7")]),
            Qty::ZERO,
            "no type"
        );
    }

    /// A cancel reject's `OrdStatus` outranks its reason code: an order the
    /// venue says is over retires whatever the code said, and one it says is
    /// live is not forgotten on a code that read as an unknown order.
    #[test]
    fn a_cancel_rejects_ord_status_decides_whether_the_order_is_over() {
        let cancel = |extra: &[(u32, &'static str)]| {
            let mut fields = vec![(tag::CL_ORD_ID, "9"), (tag::ORIG_CL_ORD_ID, "8")];
            fields.extend_from_slice(extra);
            codec().decode::<Stock>(&fix("9", &fields)).unwrap().kind
        };
        assert_eq!(
            cancel(&[(tag::CXL_REJ_REASON, "1"), (tag::ORD_STATUS, "1")]),
            ExecKind::Rejected(RejectReason::Other),
            "unknown order, but the order is partially filled and live"
        );
        assert_eq!(
            cancel(&[(tag::CXL_REJ_REASON, "0"), (tag::ORD_STATUS, "2")]),
            ExecKind::Rejected(RejectReason::Other),
            "too late to cancel, it filled: kept until the fill itself lands"
        );
        assert_eq!(
            cancel(&[(tag::CXL_REJ_REASON, "1"), (tag::ORD_STATUS, "2")]),
            ExecKind::Rejected(RejectReason::Other),
            "even on an unknown-order code: the fill retires it, not this"
        );
        assert_eq!(
            cancel(&[(tag::CXL_REJ_REASON, "0"), (tag::ORD_STATUS, "4")]),
            ExecKind::Rejected(RejectReason::UnknownOrder),
            "already cancelled"
        );
        assert_eq!(
            cancel(&[(tag::CXL_REJ_REASON, "0")]),
            ExecKind::Rejected(RejectReason::Other),
            "no status: the code stands"
        );
        assert_eq!(
            cancel(&[(tag::CXL_REJ_REASON, "1"), (tag::ORD_STATUS, "3")]),
            ExecKind::Rejected(RejectReason::UnknownOrder),
            "done for day says neither way: the code stands, and the order is forgotten"
        );
    }

    /// The session's status is the venue's trading state: halted while it
    /// is not logged in, open while it is, and unmoved by a sequence gap.
    #[test]
    fn the_sessions_status_is_the_venues_trading_state() {
        let (tx, _rx) = kanal::bounded::<FixMessage>(16);
        let g = GraphBuilder::new();
        let requests = g.replay(Vec::<(Request<Stock>, NanoTime)>::new());
        let at = |n: u64| NanoTime::new(AT_NANOS + n);
        let status = g.replay([
            (FixSessionStatus::Disconnected, at(0)),
            (FixSessionStatus::LoggingIn, at(1)),
            (FixSessionStatus::LoggedIn, at(2)),
            (
                FixSessionStatus::SequenceGap {
                    expected: 5,
                    received: 9,
                },
                at(3),
            ),
            (FixSessionStatus::LoggedOut(None), at(4)),
            (FixSessionStatus::LoggedIn, at(5)),
        ]);
        let venue = FixOrderVenue {
            inbound: silent(&g),
            status,
            sender: FixSender { sender: tx },
            epoch: Epoch::ZERO,
            mass_cancel: false,
            codec: codec(),
        };
        let trading = venue.wire(&requests).trading.with_time().accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        assert_eq!(
            runner.value(&trading),
            vec![
                (NanoTime::ZERO, TradingState::Halted),
                (at(0), TradingState::Halted),
                (at(1), TradingState::Halted),
                (at(2), TradingState::Open),
                (at(4), TradingState::Halted),
                (at(5), TradingState::Open),
            ]
        );
    }
}
