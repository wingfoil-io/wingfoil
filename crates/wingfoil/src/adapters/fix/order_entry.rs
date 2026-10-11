//! FIX order entry for the execution layer: the tag=value either side of
//! [`ReplaceChain`], and a [`Venue`] over a FIX session.
//!
//! [`ReplaceChain`]: crate::adapters::execution::fix::ReplaceChain
//!
//! Gated on `fix` **and** `execution`. The execution layer knows no wire
//! protocol — its [`fix`](crate::adapters::execution::fix) module is the
//! *shape* FIX imposes on order entry, as values — and this adapter knew no
//! orders. This module is the bridge, and the only place the two meet:
//!
//! ```text
//!   requests ─► ReplaceChain ─► Message ─► FixCodec::encode ─► FixSender ─► venue
//!   reports  ◄─ ReplaceChain ◄─ ExecReport ◄─ FixCodec::decode ◄─ inbound ◄─┘
//! ```
//!
//! # What is generic, and what a venue states
//!
//! [`FixCodec`] writes and reads standard FIX 4.4 order entry:
//! NewOrderSingle (`35=D`), OrderCancelReplaceRequest (`G`),
//! OrderCancelRequest (`F`) and OrderMassCancelRequest (`q`) out;
//! ExecutionReport (`8`) and OrderCancelReject (`9`) in. Order type, time in
//! force, post-only (`ExecInst` `6`) and the execution fields (`LastQty`,
//! `CumQty`, `LeavesQty`, `LastPx`, `ExecID`, `LastLiquidityInd`) are the
//! standard's and the same everywhere.
//!
//! What differs between venues is a [`Dialect`]: how an instrument is named
//! (`Symbol`, or `SecurityID` with its source), how a `ClOrdID` is written
//! (the venue's length cap), which reject codes mean what, and any field a
//! venue requires that the standard leaves optional (an account, a
//! `HandlInst`). A venue integration is a `Dialect` and a [`FixOrderVenue`]
//! built on it — no codec of its own.
//!
//! # What it does not do
//!
//! - **No session.** Logon, sequence numbers, heartbeats and reconnect are
//!   the session's ([`fix_connect_tls`](super::fix_connect_tls) and friends);
//!   this module writes to its [`FixSender`] and reads its inbound stream.
//! - **No triggers.** [`ReplaceChain`] refuses a triggered order rather
//!   than send it as the plain order it names.
//! - **No status or pending reports.** `150=I` (status), `6`/`E`/`A`
//!   (pending cancel / replace / new) and anything else this edge has no
//!   [`ExecKind`] for decode to `None`: they answer no request.

use crate::NanoTime;
use crate::adapters::execution::edge::{RejectReason, Request};
use crate::adapters::execution::exec_id::ExecId;
use crate::adapters::execution::fix::{
    ClOrdId, ExecKind, ExecReport, FixOps, Message, NewOrder, Trade,
};
use crate::adapters::execution::order::{Epoch, Instrument, Liquidity, OrderKind, TimeInForce};
use crate::adapters::execution::venue::{Session, Venue};
use crate::adapters::market::{Px, Qty, Side};
use crate::prelude::*;

use super::{FixMessage, FixSender};

/// FIX tags this module reads or writes.
pub mod tag {
    /// `ClOrdID`.
    pub const CL_ORD_ID: u32 = 11;
    /// `CumQty`.
    pub const CUM_QTY: u32 = 14;
    /// `ExecID`.
    pub const EXEC_ID: u32 = 17;
    /// `ExecInst`.
    pub const EXEC_INST: u32 = 18;
    /// `SecurityIDSource`.
    pub const SECURITY_ID_SOURCE: u32 = 22;
    /// `LastPx`.
    pub const LAST_PX: u32 = 31;
    /// `LastQty`.
    pub const LAST_QTY: u32 = 32;
    /// `OrderQty`.
    pub const ORDER_QTY: u32 = 38;
    /// `OrdType`.
    pub const ORD_TYPE: u32 = 40;
    /// `OrigClOrdID`.
    pub const ORIG_CL_ORD_ID: u32 = 41;
    /// `Price`.
    pub const PRICE: u32 = 44;
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
    /// `CxlRejReason`.
    pub const CXL_REJ_REASON: u32 = 102;
    /// `OrdRejReason`.
    pub const ORD_REJ_REASON: u32 = 103;
    /// `ExecType`.
    pub const EXEC_TYPE: u32 = 150;
    /// `LeavesQty`.
    pub const LEAVES_QTY: u32 = 151;
    /// `MassCancelRequestType`.
    pub const MASS_CANCEL_REQUEST_TYPE: u32 = 530;
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
    /// The fields naming `instrument` on an outbound message — `Symbol`
    /// (`55`), or `SecurityID` (`48`) with its `SecurityIDSource` (`22`),
    /// or whatever the venue keys on.
    fn instrument(&self, instrument: &I) -> Vec<(u32, String)>;

    /// The instrument an execution report is for, or `None` for one this
    /// dialect does not trade. A trade whose instrument is `None` is
    /// dropped with an error logged — it cannot be booked against nothing
    /// — so a venue that omits the field on some reports answers from what
    /// it trades.
    fn instrument_of(&self, report: &FixMessage) -> Option<I>;

    /// A `ClOrdID` as it goes on the wire. Decimal by default: the chain's
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

    /// Why a cancel or replace was refused (`35=9`). By default
    /// `CxlRejReason` (`102`) `1` is an unknown order.
    fn cancel_reject(&self, report: &FixMessage) -> RejectReason {
        match report.field(tag::CXL_REJ_REASON) {
            Some("1") => RejectReason::UnknownOrder,
            _ => RejectReason::Other,
        }
    }

    /// Fields the venue requires that the standard leaves optional — an
    /// account, a `HandlInst` — appended to every outbound `message`.
    /// None by default.
    fn extra(&self, message: &Message<I>, fields: &mut Vec<(u32, String)>) {
        let _ = (message, fields);
    }
}

/// FIX 4.4 order entry, in a venue's [`Dialect`].
#[derive(Clone, Debug)]
pub struct FixCodec<D> {
    dialect: D,
}

impl<D> FixCodec<D> {
    /// A codec speaking `dialect`.
    pub const fn new(dialect: D) -> FixCodec<D> {
        FixCodec { dialect }
    }

    /// The dialect.
    pub const fn dialect(&self) -> &D {
        &self.dialect
    }
}

impl<D> FixCodec<D> {
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
                fields.extend(d.instrument(&instrument));
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
                fields.extend(d.instrument(&instrument));
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
            } => {
                fields.push((tag::CL_ORD_ID, id(cl_ord_id)));
                fields.push((tag::ORIG_CL_ORD_ID, id(orig)));
                fields.extend(d.instrument(&instrument));
                fields.push((tag::SIDE, side_tag(side).into()));
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

    /// Read one inbound message as an execution report, or `None` for
    /// anything that is not one this edge has a kind for — admin traffic, a
    /// status or pending report, an id this process did not mint (see the
    /// module docs).
    pub fn decode<I: Instrument>(&self, msg: &FixMessage) -> Option<ExecReport<I>>
    where
        D: Dialect<I>,
    {
        let d = &self.dialect;
        let kind = match msg.msg_type.as_str() {
            "9" => ExecKind::Rejected(d.cancel_reject(msg)),
            "8" => match msg.field(tag::EXEC_TYPE)? {
                "0" => ExecKind::New,
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
                "8" => ExecKind::Rejected(d.order_reject(msg)),
                // `F` is FIX 4.4's trade; `1` / `2` are 4.2's partial and
                // full fill, which some sessions still send.
                "F" | "1" | "2" => {
                    let Some(instrument) = d.instrument_of(msg) else {
                        log::error!(
                            "fix order entry: dropped an execution on an instrument the \
                             dialect does not trade: {msg:?}"
                        );
                        return None;
                    };
                    ExecKind::Trade(Trade {
                        exec_id: ExecId::new(msg.field(tag::EXEC_ID)?).ok()?,
                        instrument,
                        side: match msg.field(tag::SIDE)? {
                            "1" => Side::Bid,
                            _ => Side::Ask,
                        },
                        qty: qty(msg, tag::LAST_QTY)?,
                        filled: qty(msg, tag::CUM_QTY)?,
                        remaining: qty(msg, tag::LEAVES_QTY).unwrap_or(Qty::ZERO),
                        price: px(msg, tag::LAST_PX)?,
                        liquidity: match msg.field(tag::LAST_LIQUIDITY_IND) {
                            Some("1") => Liquidity::Maker,
                            _ => Liquidity::Taker,
                        },
                    })
                }
                _ => return None,
            },
            _ => return None,
        };
        Some(ExecReport {
            cl_ord_id: d.parse_cl_ord_id(msg.field(tag::CL_ORD_ID)?)?,
            orig: msg
                .field(tag::ORIG_CL_ORD_ID)
                .and_then(|text| d.parse_cl_ord_id(text)),
            kind,
            venue_time: msg.sending_time,
        })
    }
}

/// A FIX session as an execution-layer [`Venue`].
///
/// [`wire`](Venue::wire) puts [`FixOps::replace_chain`] on the requests,
/// decodes `inbound` into its executions, and encodes each message it sends
/// onto `sender` — a non-blocking push onto the session's outbound queue,
/// stamped `TransactTime` at the engine time. The venue's answers arrive
/// on `inbound`, cycles later; nothing waits. The session states reports
/// and nothing else, so the other six [`Session`] streams are silent.
pub struct FixOrderVenue<D> {
    /// The order session's inbound messages.
    pub inbound: Stream<Burst<FixMessage>>,
    /// The order session's outbound queue.
    pub sender: FixSender,
    /// The process's epoch, which the chain mints its `ClOrdID`s under.
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
        let (messages, reports) = requests.replace_chain(self.mass_cancel, self.epoch, &executions);
        let codec = self.codec.clone();
        let sender = self.sender.clone();
        let _sent = messages.with_time().for_each(
            move |(now, messages): &(NanoTime, Burst<Message<I>>)| {
                for message in messages {
                    if let Err(e) = sender.send(codec.encode(message, *now)) {
                        // The OMS's slot waits on a report that will now
                        // never come. A full queue is a stalled session
                        // and a closed one a dead session, which reports
                        // its orders cancelled when it returns.
                        log::error!("fix order entry: dropped {message:?}: {e}");
                    }
                }
                Ok(())
            },
        );
        Session::quiet(reports)
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

/// What was left showing when an order ended: `OrderQty − CumQty` where the
/// report states both, else `LeavesQty`, else nothing.
fn unfilled(msg: &FixMessage) -> Qty {
    match (qty(msg, tag::ORDER_QTY), qty(msg, tag::CUM_QTY)) {
        (Some(total), Some(filled)) if total >= filled => total - filled,
        _ => qty(msg, tag::LEAVES_QTY).unwrap_or(Qty::ZERO),
    }
}

fn qty(msg: &FixMessage, tag: u32) -> Option<Qty> {
    Qty::parse(msg.field(tag)?).ok()
}

fn px(msg: &FixMessage, tag: u32) -> Option<Px> {
    Px::parse(msg.field(tag)?).ok()
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

/// A FIX `UTCTimestamp` to the millisecond.
fn utc_timestamp(at: NanoTime) -> String {
    let nanos = i64::try_from(u64::from(at)).unwrap_or(i64::MAX);
    chrono::DateTime::from_timestamp_nanos(nanos)
        .format("%Y%m%d-%H:%M:%S%.3f")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::execution::edge::Report;
    use crate::adapters::execution::order::{ClientOrderId, Order};
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
        fn instrument(&self, instrument: &Stock) -> Vec<(u32, String)> {
            let symbol = match instrument {
                Stock::Aapl => "AAPL",
                Stock::Msft => "MSFT",
            };
            vec![(tag::SYMBOL, symbol.into())]
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
            },
            NanoTime::new(AT_NANOS),
        );
        assert_eq!(cancel.msg_type, "F");
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
        assert_eq!(codec().decode(&fix("0", &[])), None);
        for exec_type in ["I", "6", "E", "A"] {
            let msg = fix("8", &[(tag::CL_ORD_ID, "3"), (tag::EXEC_TYPE, exec_type)]);
            assert_eq!(codec().decode(&msg), None, "150={exec_type}");
        }
        let foreign = fix("8", &[(tag::CL_ORD_ID, "abc"), (tag::EXEC_TYPE, "4")]);
        assert_eq!(codec().decode(&foreign), None);
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
        let requests = g.replay_results([Ok((place, NanoTime::new(AT_NANOS)))]);
        let ack = fix("8", &[(tag::CL_ORD_ID, "1"), (tag::EXEC_TYPE, "0")]);
        let later = NanoTime::new(u64::from(NanoTime::new(AT_NANOS)) + 1_000_000);
        let inbound = g.replay_results([Ok((ack, later))]);
        let venue = FixOrderVenue {
            inbound,
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
}
