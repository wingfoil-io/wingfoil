//! The OMS in front of a FIX-shaped venue, unchanged, in a graph.
//!
//! `cargo run -p wingfoil --example execution_replace_chain --features execution-testing`
//!
//! A FIX venue renames an order on every replace (`OrigClOrdID` →
//! `ClOrdID`) and may have no mass cancel. The OMS keeps one
//! `ClientOrderId` per order and says `CancelAll`; `fix::ReplaceChain` maps
//! between the two, so the diff never learns a venue id exists. The test
//! venue, `testing::SimVenue`, is a `FixVenue` behind that chain as one
//! node: what comes back up the wire is the OMS's own ids, with the venue's
//! current `ClOrdID` carried on each ack as its `VenueId`.

use std::time::Duration;

use wingfoil::adapters::execution::edge::{Report, Request};
use wingfoil::adapters::execution::oms::{Config, Desired, OmsOps};
use wingfoil::adapters::execution::order::Epoch;
use wingfoil::adapters::execution::rate_limit::{OrderRate, Terms};
use wingfoil::adapters::execution::testing::{Profile, SimVenue, Touch};
use wingfoil::adapters::execution::venue::Venue;
use wingfoil::adapters::market::{Level, Px, px, qty};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// The E-mini S&P future.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Es;

const START: NanoTime = NanoTime::ZERO;
const WIRE: Duration = Duration::from_millis(1);

const RATE: OrderRate = OrderRate::stated(Terms::per_second(50, 50), Terms::per_second(1, 1), 1.0);

const CONFIG: Config = Config::unmetered()
    .with_rate(RATE)
    .with_max_desired_age(Duration::from_secs(60));

/// Post-only, so the OMS's default passive kind is taken; no mass cancel.
const VENUE: Profile = Profile {
    post_only: true,
    mass_cancel: false,
};

fn at(ms: u64) -> NanoTime {
    START + Duration::from_millis(ms)
}

fn ms(t: NanoTime) -> u64 {
    u64::from(t - START) / 1_000_000
}

fn level(price: Px) -> Level {
    Level::new(price, qty!(5))
}

fn touch(bid: Px, ask: Px) -> Touch<Es> {
    Touch {
        instrument: Es,
        bid: Some(bid),
        ask: Some(ask),
    }
}

fn two_way(bid: Px, ask: Px, as_of: NanoTime) -> Desired<Es> {
    Desired::two_way(Es, as_of, level(bid), level(ask))
}

fn replay<T: Clone + Default + 'static>(
    g: &GraphBuilder,
    rows: Vec<(T, NanoTime)>,
) -> Stream<Burst<T>> {
    g.replay_results(rows.into_iter().map(Ok))
}

fn describe(request: &Request<Es>) -> String {
    match request {
        Request::Place(o) => format!(
            "place  order {} {:?} {} @ {}",
            o.id.sequence(),
            o.side,
            o.qty,
            o.price.map_or("market".to_string(), |p| p.to_string())
        ),
        Request::Amend(a) => format!(
            "amend  order {} → {} @ {}",
            a.order.sequence(),
            a.qty,
            a.price
        ),
        Request::Cancel(id) => format!("cancel order {}", id.sequence()),
        Request::CancelAll => "cancel all".to_string(),
    }
}

fn heard(report: &Report<Es>) -> String {
    match report {
        Report::Ack(a) => format!(
            "ack       order {}  resting as ClOrdID {}",
            a.order.sequence(),
            a.venue_id
        ),
        Report::Reject(r) => format!(
            "reject    order {}  {:?}",
            r.order
                .map_or("-".to_string(), |o| o.sequence().to_string()),
            r.reason
        ),
        Report::Fill(f) => format!(
            "fill      order {}  {} @ {} ({:?}), {} left",
            f.order.sequence(),
            f.qty,
            f.price,
            f.liquidity,
            f.remaining
        ),
        Report::Cancelled(r) => format!(
            "cancelled order {}  {} left",
            r.order.sequence(),
            r.remaining
        ),
        Report::Expired(r) => format!("expired   order {}", r.order.sequence()),
        Report::Triggered(f) => format!("triggered order {}", f.order.sequence()),
    }
}

fn main() -> anyhow::Result<()> {
    let g = GraphBuilder::new();

    // The market; at 6ms it trades down through the bid and is back a
    // millisecond later.
    let touches = replay(
        &g,
        vec![
            (touch(px!(5000.00), px!(5000.25)), at(0)),
            (touch(px!(4999.25), px!(4999.50)), at(6)),
            (touch(px!(5000.00), px!(5000.25)), at(7)),
        ],
    );

    // Quote; re-price once the acks are back, which the venue answers by
    // renaming both orders. The fill lands on the renamed order, and the
    // OMS, still wanting the bid, places it again as a new order. Then
    // cancel all, on a venue with no mass cancel.
    let desired = replay(
        &g,
        vec![
            (two_way(px!(4999.75), px!(5000.50), at(1)), at(1)),
            (two_way(px!(4999.50), px!(5000.75), at(4)), at(4)),
        ],
    );
    let cancel_all = replay(&g, vec![((), at(10))]).map(|_: &Burst<()>| ());

    let (requests, reports) = g.feedback_loop(|landed| {
        let session = SimVenue::new(VENUE, Epoch::ZERO, &touches).wire(&landed.delay(WIRE));
        let reports = session.reports.delay(WIRE);
        let (requests, _pacing) = desired
            .wire_oms(CONFIG, Epoch::ZERO, &reports)
            .cancel_all(&cancel_all)
            .build();
        (requests, reports)
    });

    let _sent = requests.with_time().for_each(|(t, burst)| {
        for request in burst {
            println!("t={}ms sent  {}", ms(*t), describe(request));
        }
        Ok(())
    });
    let _heard = reports.with_time().for_each(|(t, burst)| {
        for report in burst {
            println!("t={}ms heard {}", ms(*t), heard(report));
        }
        Ok(())
    });

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(START),
        RunFor::Duration(Duration::from_millis(15)),
    )?;
    Ok(())
}
