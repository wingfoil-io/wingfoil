//! LMAX's dialect of FIX order entry.
//!
//! Everything else about order entry is the library's: the replace chain,
//! the FIX 4.4 codec and the venue over a session
//! (`adapters::fix::order_entry::FixOrderVenue`). What LMAX says differently
//! is only how it names the one contract this gateway trades.

use wingfoil::adapters::fix::FixMessage;
use wingfoil::adapters::fix::order_entry::{Dialect, tag};

/// EUR/USD on LMAX — the one contract this gateway trades. A unit type: the
/// execution layer is generic over the instrument and reads nothing of it
/// but its identity, so the venue's own id ([`EUR_USD_ID`]) stays here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EurUsd;

/// LMAX's `SecurityID` (tag 48) for EUR/USD.
pub const EUR_USD_ID: &str = "4001";

/// LMAX names a contract by `SecurityID` with `SecurityIDSource` `8`
/// (exchange symbol). Its `ClOrdID` cap is 20 characters, which the
/// default decimal ids — at most 16 digits — sit inside, and its reject
/// codes are FIX 4.4's.
#[derive(Clone, Copy, Debug)]
pub struct Lmax;

impl Dialect<EurUsd> for Lmax {
    fn instrument(&self, _: &EurUsd, fields: &mut Vec<(u32, String)>) {
        fields.push((tag::SECURITY_ID_SOURCE, "8".into()));
        fields.push((tag::SECURITY_ID, EUR_USD_ID.into()));
    }

    /// A report on EUR/USD — or one that names no contract, since this
    /// session trades nothing else.
    fn instrument_of(&self, report: &FixMessage) -> Option<EurUsd> {
        match report.field(tag::SECURITY_ID) {
            None | Some(EUR_USD_ID) => Some(EurUsd),
            Some(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wingfoil::NanoTime;
    use wingfoil::adapters::execution::fix::{ClOrdId, ExecKind, Message, NewOrder};
    use wingfoil::adapters::execution::order::{OrderKind, TimeInForce};
    use wingfoil::adapters::fix::order_entry::FixCodec;
    use wingfoil::adapters::market::{Px, Qty, Side};

    #[test]
    fn an_order_names_eur_usd_by_security_id() {
        let msg = FixCodec::new(Lmax).encode(
            &Message::New(NewOrder {
                cl_ord_id: ClOrdId(12),
                instrument: EurUsd,
                side: Side::Ask,
                qty: Qty::parse("3").unwrap(),
                price: Some(Px::parse("1.08514").unwrap()),
                kind: OrderKind::Limit,
                tif: TimeInForce::ImmediateOrCancel,
            }),
            NanoTime::ZERO,
        );
        assert_eq!(msg.field(tag::SECURITY_ID), Some(EUR_USD_ID));
        assert_eq!(msg.field(tag::SECURITY_ID_SOURCE), Some("8"));
        assert_eq!(msg.field(tag::TIME_IN_FORCE), Some("3"));
    }

    #[test]
    fn a_fill_on_another_contract_is_not_ours() {
        let fill = |security: Option<&str>| {
            let mut fields = vec![
                (tag::CL_ORD_ID, "3".to_string()),
                (tag::EXEC_TYPE, "F".into()),
                (tag::EXEC_ID, "X1".into()),
                (tag::SIDE, "1".into()),
                (tag::LAST_QTY, "1".into()),
                (tag::CUM_QTY, "1".into()),
                (tag::LEAVES_QTY, "0".into()),
                (tag::LAST_PX, "1.085".into()),
            ];
            if let Some(security) = security {
                fields.push((tag::SECURITY_ID, security.into()));
            }
            FixCodec::new(Lmax).decode::<EurUsd>(&FixMessage {
                msg_type: "8".into(),
                seq_num: 1,
                sending_time: NanoTime::ZERO,
                fields,
            })
        };
        assert!(matches!(
            fill(Some(EUR_USD_ID)).map(|r| r.kind),
            Some(ExecKind::Trade(_))
        ));
        assert!(
            fill(None).is_some(),
            "a report naming no contract is EUR/USD's"
        );
        assert_eq!(fill(Some("4002")), None);
    }
}
