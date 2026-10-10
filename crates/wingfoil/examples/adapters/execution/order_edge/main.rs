//! The execution vocabulary on an instrument of your own: `Order`, `Fill`,
//! `Request` and `Report`, generic over what they trade.
//!
//! `cargo run -p wingfoil --example execution_order_edge --features execution`
//!
//! Nothing here knows what an option or a perpetual is. The instrument is any
//! `Copy + Default` value — here an equity ticker — and the constructors,
//! `validate`, the inert defaults and the epoch-carrying ids work the same
//! for it as for anything else.

use wingfoil::NanoTime;
use wingfoil::adapters::execution::edge::{
    Ack, Amend, Reject, RejectReason, Report, Request, RequestError,
};
use wingfoil::adapters::execution::exec_id::ExecId;
use wingfoil::adapters::execution::order::{
    ClientOrderId, Epoch, Fill, Liquidity, Order, OrderError, OrderKind, TimeInForce,
};
use wingfoil::adapters::market::{Px, Qty, Side};

/// An equity, by ticker: the whole identity, as a `Copy` value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Ticker {
    #[default]
    Aapl,
    Msft,
}

fn px(s: &str) -> Px {
    Px::parse(s).unwrap()
}

fn qty(s: &str) -> Qty {
    Qty::parse(s).unwrap()
}

fn main() {
    // Ids carry the process's epoch above a sequence, so a restarted process
    // never reuses the last one's numbers.
    let epoch = Epoch::new(7).unwrap();
    let id = |n| ClientOrderId::new(epoch, n);
    println!(
        "id 1 of epoch 7 is {} (epoch {}, seq {})",
        id(1).0,
        id(1).epoch().get(),
        id(1).sequence()
    );

    // Each constructor can only build an order whose kind, price and
    // lifetime agree.
    let bid = Order::post_only(id(1), Ticker::Aapl, Side::Bid, qty("100"), px("189.50"));
    let lift = Order::limit(id(2), Ticker::Msft, Side::Bid, qty("50"), px("421.00"));
    let dump = Order::market(id(3), Ticker::Msft, Side::Ask, qty("50"));
    for order in [bid, lift, dump] {
        println!(
            "{:?} {:?} {} {:?} @ {} {:?} → {:?}",
            order.instrument,
            order.side,
            order.qty,
            order.kind,
            order.price.map_or("market".to_string(), |p| p.to_string()),
            order.tif,
            order.validate()
        );
        order.validate().unwrap();
    }

    // What a struct literal or an edited copy can get wrong, `validate` says.
    let crossing = Order {
        tif: TimeInForce::ImmediateOrCancel,
        ..bid
    };
    assert_eq!(
        crossing.validate(),
        Err(OrderError::PostOnlyCannotCross(
            TimeInForce::ImmediateOrCancel
        ))
    );
    // A defaulted order exists only to ride a `Burst`, and is inert.
    let inert: Order<Ticker> = Order::default();
    assert_eq!(inert.kind, OrderKind::PostOnly);
    assert_eq!(
        inert.validate(),
        Err(OrderError::NonPositiveQuantity(Qty::ZERO))
    );
    println!("a post-only IOC and a defaulted order are both refused");

    // The edge out: place, amend (which carries its instrument), cancel.
    let requests = [
        Request::Place(bid),
        Request::Amend(Amend {
            order: id(1),
            instrument: Ticker::Aapl,
            price: px("189.55"),
            qty: qty("100"),
            trigger: None,
        }),
        Request::Cancel(id(1)),
        Request::CancelAll,
    ];
    for request in &requests {
        let name = match request {
            Request::Place(_) => "place",
            Request::Amend(_) => "amend",
            Request::Cancel(_) => "cancel",
            Request::CancelAll => "cancel-all",
        };
        println!(
            "request {name} → order {:?}",
            request.order().map(|id| id.sequence())
        );
        request.validate().unwrap();
    }
    let to_nothing = Request::Amend(Amend {
        order: id(1),
        instrument: Ticker::Aapl,
        price: px("189.55"),
        qty: Qty::ZERO,
        trigger: None,
    });
    assert_eq!(
        to_nothing.validate(),
        Err(RequestError::AmendToNothing(Qty::ZERO))
    );

    // The edge back: one report stream, so an ack cannot overtake its fill.
    let now = NanoTime::from(1_000_000_000u64);
    let reports: [Report<Ticker>; 3] = [
        Report::Ack(Ack {
            order: id(1),
            recv_time: now,
            ..Ack::default()
        }),
        Report::Fill(Fill {
            order: id(1),
            exec_id: ExecId::new("XNAS-000123").unwrap(),
            instrument: Ticker::Aapl,
            side: Side::Bid,
            qty: qty("40"),
            // What has filled on the order so far, `CumQty`: the first
            // execution, so the same 40.
            filled: qty("40"),
            remaining: qty("60"),
            price: px("189.50"),
            fee: qty("-0.08"),
            liquidity: Liquidity::Maker,
            venue_time: None,
            recv_time: now,
        }),
        Report::Reject(Reject {
            order: Some(id(2)),
            reason: RejectReason::RiskLimit,
            recv_time: now,
            ..Reject::default()
        }),
    ];
    for report in &reports {
        println!(
            "report about {:?} at {}",
            report.order(),
            report.recv_time()
        );
    }
    // A venue id that does not fit is refused, never truncated.
    assert!(ExecId::new(&"9".repeat(200)).is_err());
    println!("a 200-character exec id is refused");
}
