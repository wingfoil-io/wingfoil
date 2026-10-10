//! The LMAX order session as an execution-layer [`Venue`].
//!
//! Everything venue-specific about order entry lives here and nowhere else:
//! the instrument's `SecurityID`, the FIX tags LMAX reads and writes, and the
//! `ClOrdID` format. What is *not* here is the FIX replace chain — the map
//! from the OMS's one [`ClientOrderId`] per order to the venue's
//! one-`ClOrdID`-per-message — because that is the same for every FIX venue
//! and is [`ReplaceChain`]'s job. This file is only the tag=value codec
//! around it, plus the graph node that owns it.
//!
//! [`ClientOrderId`]: wingfoil::adapters::execution::order::ClientOrderId

use std::cell::RefCell;

use wingfoil::NanoTime;
use wingfoil::adapters::execution::edge::{RejectReason, Report, Request};
use wingfoil::adapters::execution::exec_id::ExecId;
use wingfoil::adapters::execution::fix::{
    ClOrdId, ExecKind, ExecReport, Message, NewOrder, ReplaceChain, Trade,
};
use wingfoil::adapters::execution::order::{Epoch, Liquidity, OrderKind, TimeInForce};
use wingfoil::adapters::execution::venue::{Session, Venue};
use wingfoil::adapters::fix::{FixMessage, FixSender};
use wingfoil::adapters::market::{Px, Qty, Side};
use wingfoil::prelude::*;

/// EUR/USD on LMAX — the one contract this gateway trades. A unit type: the
/// execution layer is generic over the instrument and reads nothing of it
/// but its identity, so the venue's own id ([`EUR_USD_ID`]) stays here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EurUsd;

/// LMAX's `SecurityID` (tag 48) for EUR/USD, `SecurityIDSource` 8.
pub const EUR_USD_ID: &str = "4001";

// ── FIX tags ──────────────────────────────────────────────────────────────
const TAG_CL_ORD_ID: u32 = 11;
const TAG_CUM_QTY: u32 = 14;
const TAG_EXEC_ID: u32 = 17;
const TAG_LAST_PX: u32 = 31;
const TAG_LAST_QTY: u32 = 32;
const TAG_ORDER_QTY: u32 = 38;
const TAG_ORD_TYPE: u32 = 40;
const TAG_ORIG_CL_ORD_ID: u32 = 41;
const TAG_PRICE: u32 = 44;
const TAG_SECURITY_ID: u32 = 48;
const TAG_SIDE: u32 = 54;
const TAG_TEXT: u32 = 58;
const TAG_TIME_IN_FORCE: u32 = 59;
const TAG_TRANSACT_TIME: u32 = 60;
const TAG_CXL_REJ_REASON: u32 = 102;
const TAG_ORD_REJ_REASON: u32 = 103;
const TAG_EXEC_TYPE: u32 = 150;
const TAG_LEAVES_QTY: u32 = 151;
const TAG_SECURITY_ID_SOURCE: u32 = 22;
const TAG_LAST_LIQUIDITY_IND: u32 = 851;

/// The LMAX order session, seen from the execution layer.
///
/// `wire` builds one node over the requests and the session's inbound
/// messages, holding the [`ReplaceChain`]: a request burst is rendered and
/// injected through the session's [`FixSender`], and every execution report
/// comes back as the OMS's [`Report`]s. The session states reports and
/// nothing else, so the other six [`Session`] streams are silent.
pub struct Lmax {
    /// The order session's inbound messages.
    pub inbound: Stream<Burst<FixMessage>>,
    /// The order session's outbound queue.
    pub sender: FixSender,
    /// The process's epoch, which the replace chain mints its `ClOrdID`s
    /// under — the same one the OMS's ids carry. LMAX remembers `ClOrdID`s
    /// across logons, so a restarted gateway must never send the last one's.
    pub epoch: Epoch,
}

/// One instant's input to the venue node: what the OMS sent, or what LMAX
/// said. `Default` only for the burst placeholder.
#[derive(Clone, Debug, Default)]
enum Event {
    #[default]
    None,
    Requests(Burst<Request<EurUsd>>),
    Inbound(Burst<FixMessage>),
}

impl Venue<EurUsd> for Lmax {
    fn wire(&self, requests: &Stream<Burst<Request<EurUsd>>>) -> Session<EurUsd> {
        let chain = RefCell::new(ReplaceChain::<EurUsd>::new(false, self.epoch));
        let sender = self.sender.clone();
        let inbound = self
            .inbound
            .map(|ms: &Burst<FixMessage>| Event::Inbound(ms.clone()));
        let outbound = requests.map(|rs: &Burst<Request<EurUsd>>| Event::Requests(rs.clone()));
        // LMAX's reports are applied before this instant's requests are
        // rendered, so a request for an order whose terminal report landed
        // in the same cycle is answered `UnknownOrder` by the chain rather
        // than sent to die at the venue.
        let reports = requests
            .graph()
            .combine(&[inbound, outbound])
            .with_time()
            .map(move |(now, events): &(NanoTime, Burst<Event>)| {
                let mut chain = chain.borrow_mut();
                let mut out: Burst<Report<EurUsd>> = Burst::new();
                for event in events {
                    match event {
                        Event::Inbound(ms) => {
                            let execs: Vec<ExecReport<EurUsd>> =
                                ms.iter().filter_map(decode).collect();
                            out.extend(chain.receive(*now, &execs));
                        }
                        Event::Requests(rs) => {
                            let (messages, refused) = chain.send(*now, rs);
                            for message in &messages {
                                let fix = encode(message);
                                log::info!(
                                    "lmax: → 35={} ClOrdID={}",
                                    fix.msg_type,
                                    fix.field(TAG_CL_ORD_ID).unwrap_or("")
                                );
                                if let Err(e) = sender.send(fix) {
                                    // The OMS's slot waits on a report that
                                    // will now never come; the order-rate
                                    // budget makes this unreachable short
                                    // of a dead session, and a dead session
                                    // reports its orders cancelled.
                                    log::error!("lmax: FixSender dropped a message: {e}");
                                }
                            }
                            out.extend(refused);
                        }
                        Event::None => {}
                    }
                }
                out
            })
            .filter_map(|out: &Burst<Report<EurUsd>>| (!out.is_empty()).then(|| out.clone()));
        Session::quiet(reports)
    }
}

/// A `ClOrdID` on the wire: the chain's id in decimal. It carries the
/// process's epoch in its high bits, so it is at most 16 digits — inside
/// LMAX's 20-character cap.
fn format(id: ClOrdId) -> String {
    id.0.to_string()
}

fn parse(text: &str) -> Option<ClOrdId> {
    text.parse().ok().map(ClOrdId)
}

/// Render one order-entry message as LMAX's FIX.
pub fn encode(message: &Message<EurUsd>) -> FixMessage {
    let transact_time = chrono::Utc::now().format("%Y%m%d-%H:%M:%S%.3f").to_string();
    let (msg_type, fields) = match *message {
        Message::New(NewOrder {
            cl_ord_id,
            side,
            qty,
            price,
            kind,
            tif,
            ..
        }) => {
            let mut fields = vec![
                (TAG_CL_ORD_ID, format(cl_ord_id)),
                (TAG_SECURITY_ID_SOURCE, "8".into()),
                (TAG_SECURITY_ID, EUR_USD_ID.into()),
                (TAG_SIDE, side_tag(side).into()),
                (TAG_ORDER_QTY, qty.to_string()),
                (TAG_ORD_TYPE, ord_type(kind).into()),
            ];
            if let Some(price) = price {
                fields.push((TAG_PRICE, price.to_string()));
            }
            fields.push((TAG_TIME_IN_FORCE, tif_tag(tif).into()));
            fields.push((TAG_TRANSACT_TIME, transact_time));
            ("D", fields)
        }
        Message::Replace {
            cl_ord_id,
            orig,
            side,
            kind,
            tif,
            price,
            qty,
            ..
        } => (
            "G",
            vec![
                (TAG_CL_ORD_ID, format(cl_ord_id)),
                (TAG_ORIG_CL_ORD_ID, format(orig)),
                (TAG_SECURITY_ID_SOURCE, "8".into()),
                (TAG_SECURITY_ID, EUR_USD_ID.into()),
                (TAG_SIDE, side_tag(side).into()),
                (TAG_ORDER_QTY, qty.to_string()),
                (TAG_ORD_TYPE, ord_type(kind).into()),
                (TAG_PRICE, price.to_string()),
                (TAG_TIME_IN_FORCE, tif_tag(tif).into()),
                (TAG_TRANSACT_TIME, transact_time),
            ],
        ),
        Message::Cancel {
            cl_ord_id,
            orig,
            side,
            ..
        } => (
            "F",
            vec![
                (TAG_CL_ORD_ID, format(cl_ord_id)),
                (TAG_ORIG_CL_ORD_ID, format(orig)),
                (TAG_SECURITY_ID_SOURCE, "8".into()),
                (TAG_SECURITY_ID, EUR_USD_ID.into()),
                (TAG_SIDE, side_tag(side).into()),
                (TAG_TRANSACT_TIME, transact_time),
            ],
        ),
        // The chain is built without a mass cancel, so a cancel-all
        // reaches here as one `Cancel` per live order.
        Message::MassCancel { cl_ord_id } => ("q", vec![(TAG_CL_ORD_ID, format(cl_ord_id))]),
    };
    FixMessage {
        msg_type: msg_type.into(),
        seq_num: 0,
        sending_time: NanoTime::ZERO,
        fields,
    }
}

/// Read one inbound message as an execution report, or `None` for anything
/// that is not one: admin traffic, a status report (`150=I`), a
/// pending-state report. An id the chain never minted — the last process's,
/// under its own epoch — is the chain's to pass over.
pub fn decode(msg: &FixMessage) -> Option<ExecReport<EurUsd>> {
    let report = |kind| {
        let cl_ord_id = parse(msg.field(TAG_CL_ORD_ID)?)?;
        let orig = msg.field(TAG_ORIG_CL_ORD_ID).and_then(parse);
        Some(ExecReport {
            cl_ord_id,
            orig,
            kind,
            venue_time: msg.sending_time,
        })
    };
    match msg.msg_type.as_str() {
        // OrderCancelReject: 102=1 is "unknown order", which the chain
        // and the OMS both treat as the order being gone.
        "9" => {
            let reason = match msg.field(TAG_CXL_REJ_REASON) {
                Some("1") => RejectReason::UnknownOrder,
                _ => RejectReason::Other,
            };
            log::info!(
                "lmax: ← cancel reject ClOrdID={} reason={reason:?} text={}",
                msg.field(TAG_CL_ORD_ID).unwrap_or(""),
                msg.field(TAG_TEXT).unwrap_or(""),
            );
            report(ExecKind::Rejected(reason))
        }
        "8" => {
            let exec_type = msg.field(TAG_EXEC_TYPE)?;
            log::info!(
                "lmax: ← 150={exec_type} ClOrdID={} text={}",
                msg.field(TAG_CL_ORD_ID).unwrap_or(""),
                msg.field(TAG_TEXT).unwrap_or(""),
            );
            let kind = match exec_type {
                "0" => ExecKind::New,
                "5" => ExecKind::Replaced {
                    price: px(msg, TAG_PRICE)?,
                    qty: qty(msg, TAG_ORDER_QTY)?,
                },
                "4" => ExecKind::Canceled {
                    remaining: unfilled(msg),
                },
                "C" => ExecKind::Expired {
                    remaining: unfilled(msg),
                },
                "8" => ExecKind::Rejected(match msg.field(TAG_ORD_REJ_REASON) {
                    Some("5") => RejectReason::UnknownOrder,
                    _ => RejectReason::Other,
                }),
                // `F` is FIX 4.4's trade; `1` / `2` are 4.2's partial /
                // full fill, which some sessions still send.
                "F" | "1" | "2" => ExecKind::Trade(Trade {
                    exec_id: ExecId::new(msg.field(TAG_EXEC_ID)?).ok()?,
                    instrument: EurUsd,
                    side: match msg.field(TAG_SIDE)? {
                        "1" => Side::Bid,
                        _ => Side::Ask,
                    },
                    qty: qty(msg, TAG_LAST_QTY)?,
                    filled: qty(msg, TAG_CUM_QTY)?,
                    remaining: qty(msg, TAG_LEAVES_QTY).unwrap_or(Qty::ZERO),
                    price: px(msg, TAG_LAST_PX)?,
                    liquidity: match msg.field(TAG_LAST_LIQUIDITY_IND) {
                        Some("1") => Liquidity::Maker,
                        _ => Liquidity::Taker,
                    },
                }),
                _ => return None,
            };
            report(kind)
        }
        _ => None,
    }
}

/// What was left showing when an order ended: `OrderQty − CumQty` where
/// the report states both, else `LeavesQty`, else nothing.
fn unfilled(msg: &FixMessage) -> Qty {
    match (qty(msg, TAG_ORDER_QTY), qty(msg, TAG_CUM_QTY)) {
        (Some(total), Some(filled)) if total >= filled => total - filled,
        _ => qty(msg, TAG_LEAVES_QTY).unwrap_or(Qty::ZERO),
    }
}

fn qty(msg: &FixMessage, tag: u32) -> Option<Qty> {
    Qty::parse(msg.field(tag)?).ok()
}

fn px(msg: &FixMessage, tag: u32) -> Option<Px> {
    Px::parse(msg.field(tag)?).ok()
}

fn side_tag(side: Side) -> &'static str {
    match side {
        Side::Bid => "1",
        Side::Ask => "2",
    }
}

/// LMAX has no post-only order type, so the OMS is configured with
/// `Passive::Limit` and never asks for one; were it to, a limit is the
/// nearest thing and the strategy owns the price guard.
fn ord_type(kind: OrderKind) -> &'static str {
    match kind {
        OrderKind::Market => "1",
        OrderKind::Limit | OrderKind::PostOnly => "2",
    }
}

fn tif_tag(tif: TimeInForce) -> &'static str {
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

    fn fix(msg_type: &str, fields: &[(u32, &str)]) -> FixMessage {
        FixMessage {
            msg_type: msg_type.into(),
            seq_num: 1,
            sending_time: NanoTime::new(7),
            fields: fields.iter().map(|(t, v)| (*t, v.to_string())).collect(),
        }
    }

    #[test]
    fn a_new_order_is_an_ioc_limit_on_eur_usd() {
        let msg = encode(&Message::New(NewOrder {
            cl_ord_id: ClOrdId(12),
            instrument: EurUsd,
            side: Side::Ask,
            qty: Qty::parse("3").unwrap(),
            price: Some(Px::parse("1.08514").unwrap()),
            kind: OrderKind::Limit,
            tif: TimeInForce::ImmediateOrCancel,
        }));
        assert_eq!(msg.msg_type, "D");
        assert_eq!(msg.field(TAG_CL_ORD_ID), Some("12"));
        assert_eq!(msg.field(TAG_SECURITY_ID), Some(EUR_USD_ID));
        assert_eq!(msg.field(TAG_SIDE), Some("2"));
        assert_eq!(msg.field(TAG_ORDER_QTY), Some("3"));
        assert_eq!(msg.field(TAG_ORD_TYPE), Some("2"));
        assert_eq!(msg.field(TAG_PRICE), Some("1.08514"));
        assert_eq!(msg.field(TAG_TIME_IN_FORCE), Some("3"));
    }

    #[test]
    fn a_cancel_names_the_order_it_cancels() {
        let msg = encode(&Message::Cancel {
            cl_ord_id: ClOrdId(5),
            orig: ClOrdId(4),
            instrument: EurUsd,
            side: Side::Bid,
        });
        assert_eq!(msg.msg_type, "F");
        assert_eq!(msg.field(TAG_CL_ORD_ID), Some("5"));
        assert_eq!(msg.field(TAG_ORIG_CL_ORD_ID), Some("4"));
    }

    #[test]
    fn a_trade_decodes_with_the_venues_cumulative_quantity() {
        let report = decode(&fix(
            "8",
            &[
                (TAG_CL_ORD_ID, "3"),
                (TAG_EXEC_TYPE, "F"),
                (TAG_EXEC_ID, "X1"),
                (TAG_SIDE, "1"),
                (TAG_LAST_QTY, "2"),
                (TAG_CUM_QTY, "2"),
                (TAG_LEAVES_QTY, "1"),
                (TAG_LAST_PX, "1.0852"),
            ],
        ))
        .unwrap();
        assert_eq!(report.cl_ord_id, ClOrdId(3));
        assert_eq!(report.venue_time, NanoTime::new(7));
        let ExecKind::Trade(trade) = report.kind else {
            panic!("{report:?}")
        };
        assert_eq!(trade.side, Side::Bid);
        assert_eq!(trade.qty, Qty::parse("2").unwrap());
        assert_eq!(trade.remaining, Qty::parse("1").unwrap());
        assert_eq!(trade.price, Px::parse("1.0852").unwrap());
        assert_eq!(trade.liquidity, Liquidity::Taker);
    }

    #[test]
    fn an_iocs_unfilled_rest_is_cancelled_with_what_was_left() {
        let report = decode(&fix(
            "8",
            &[
                (TAG_CL_ORD_ID, "3"),
                (TAG_EXEC_TYPE, "4"),
                (TAG_ORDER_QTY, "3"),
                (TAG_CUM_QTY, "2"),
                (TAG_LEAVES_QTY, "0"),
            ],
        ))
        .unwrap();
        assert_eq!(
            report.kind,
            ExecKind::Canceled {
                remaining: Qty::parse("1").unwrap()
            }
        );
    }

    #[test]
    fn admin_and_status_traffic_are_not_execution_reports() {
        assert_eq!(decode(&fix("0", &[])), None);
        let status = fix("8", &[(TAG_CL_ORD_ID, "3"), (TAG_EXEC_TYPE, "I")]);
        assert_eq!(decode(&status), None);
    }

    #[test]
    fn an_unknown_order_cancel_reject_says_so() {
        let report = decode(&fix(
            "9",
            &[
                (TAG_CL_ORD_ID, "9"),
                (TAG_ORIG_CL_ORD_ID, "8"),
                (TAG_CXL_REJ_REASON, "1"),
            ],
        ))
        .unwrap();
        assert_eq!(report.orig, Some(ClOrdId(8)));
        assert_eq!(report.kind, ExecKind::Rejected(RejectReason::UnknownOrder));
    }
}
