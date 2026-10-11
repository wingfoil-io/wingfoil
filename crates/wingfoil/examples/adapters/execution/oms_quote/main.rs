//! The OMS on an instrument of your own, in a graph: desireds in, the
//! requests that close the gap out, the venue's answers back in.
//!
//! `cargo run -p wingfoil --example execution_oms_quote --features execution-testing`
//!
//! A market maker quoting equities. The strategy's decisions are replayed
//! at engine instants into the OMS node (`OmsOps`); what it sends goes
//! down a millisecond of wire and through the feedback cut to the test
//! venue, whose reports come back on the session. Nothing here advances a
//! clock by hand: the graph is run historically, and every line is printed
//! at the instant it happened.

use std::time::Duration;

use wingfoil::adapters::execution::edge::{Report, Request};
use wingfoil::adapters::execution::oms::{Config, Desired, OmsOps, Pacing};
use wingfoil::adapters::execution::order::Epoch;
use wingfoil::adapters::execution::rate_limit::{OrderRate, Terms};
use wingfoil::adapters::execution::testing::{Profile, SimVenue, Touch};
use wingfoil::adapters::execution::venue::Venue;
use wingfoil::adapters::market::{Level, Px, Qty};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Ticker {
    #[default]
    Aapl,
    Msft,
    Nvda,
}

/// The run starts here; every instant below is milliseconds after it.
const START: NanoTime = NanoTime::ZERO;

/// The wire to the venue, each way.
const WIRE: Duration = Duration::from_millis(1);

/// A venue's order-entry limits: 10 a second, burst 4, spent whole.
const RATE: OrderRate = OrderRate::stated(Terms::per_second(10, 4), Terms::per_second(1, 1), 1.0);

/// Post-only, good till cancelled, a decision believed for five seconds.
const CONFIG: Config = Config::unmetered()
    .with_rate(RATE)
    .with_max_desired_age(Duration::from_secs(5));

/// A venue with post-only and a mass cancel.
const VENUE: Profile = Profile {
    post_only: true,
    mass_cancel: true,
};

fn at(ms: u64) -> NanoTime {
    START + Duration::from_millis(ms)
}

fn ms(t: NanoTime) -> u64 {
    u64::from(t - START) / 1_000_000
}

fn px(s: &str) -> Px {
    Px::parse(s).unwrap()
}

fn level(price: &str, size: &str) -> Level {
    Level::new(px(price), Qty::parse(size).unwrap())
}

fn touch(ticker: Ticker, bid: &str, ask: &str) -> Touch<Ticker> {
    Touch {
        instrument: ticker,
        bid: Some(px(bid)),
        ask: Some(px(ask)),
    }
}

fn two_way(ticker: Ticker, bid: Level, ask: Level, as_of: NanoTime) -> Desired<Ticker> {
    Desired::two_way(ticker, as_of, bid, ask)
}

/// Replay `rows`, each at its instant, as a burst stream.
fn replay<T: Clone + Default + 'static>(
    g: &GraphBuilder,
    rows: Vec<(T, NanoTime)>,
) -> Stream<Burst<T>> {
    g.replay_results(rows.into_iter().map(Ok))
}

fn describe(request: &Request<Ticker>) -> String {
    match request {
        Request::Place(o) => format!(
            "place  {} {:?} {:?} {} @ {}",
            o.id.sequence(),
            o.instrument,
            o.side,
            o.qty,
            o.price.map_or("market".to_string(), |p| p.to_string())
        ),
        Request::Amend(a) => format!("amend  {} → {} @ {}", a.order.sequence(), a.qty, a.price),
        Request::Cancel(id) => format!("cancel {}", id.sequence()),
        Request::CancelAll => "cancel all".to_string(),
    }
}

fn heard(report: &Report<Ticker>) -> String {
    match report {
        Report::Ack(a) => format!("ack       {}", a.order.sequence()),
        Report::Reject(r) => format!(
            "reject    {} ({:?})",
            r.order
                .map_or("-".to_string(), |o| o.sequence().to_string()),
            r.reason
        ),
        Report::Fill(f) => format!(
            "fill      {} {} @ {} ({:?})",
            f.order.sequence(),
            f.qty,
            f.price,
            f.liquidity
        ),
        Report::Cancelled(r) => format!("cancelled {}", r.order.sequence()),
        Report::Expired(r) => format!("expired   {}", r.order.sequence()),
        Report::Triggered(f) => format!("triggered {}", f.order.sequence()),
    }
}

fn main() -> anyhow::Result<()> {
    let g = GraphBuilder::new();

    // The market: one touch per name, stated before anything is quoted.
    let touches = replay(
        &g,
        vec![
            (touch(Ticker::Aapl, "189.54", "189.56"), at(0)),
            (touch(Ticker::Msft, "421.00", "421.02"), at(0)),
            (touch(Ticker::Nvda, "120.02", "120.03"), at(0)),
        ],
    );

    // The strategy's decisions, replayed at the instants it made them.
    let aapl = |bid, ask, as_of| two_way(Ticker::Aapl, level(bid, "100"), level(ask, "100"), as_of);
    let msft = |bid, ask, as_of| two_way(Ticker::Msft, level(bid, "50"), level(ask, "50"), as_of);
    let nvda = |bid, ask, as_of| two_way(Ticker::Nvda, level(bid, "200"), level(ask, "200"), as_of);
    let desired = replay(
        &g,
        vec![
            // Nothing working: two two-ways are four post-only places —
            // exactly the burst the venue allows.
            (aapl("189.50", "189.60", at(0)), at(0)),
            (msft("420.90", "421.10", at(0)), at(0)),
            // The touch moves on AAPL only: one amend per side that changed,
            // and MSFT, which wants what is working, is left alone.
            (aapl("189.55", "189.65", at(300)), at(300)),
            (msft("420.90", "421.10", at(300)), at(300)),
            // Withdrawing is a decision: an empty desired cancels at once.
            (Desired::nothing(Ticker::Msft, at(600)), at(600)),
            // AAPL is then left alone past `max_desired_age`, and the sweep
            // withdraws it without a decision.
            //
            // Three two-ways at once are six places; four go now, two wait —
            // not queued, re-planned on the next sweep once a token has
            // refilled (a tenth of a second at 10 a second).
            (aapl("189.50", "189.60", at(10_000)), at(10_000)),
            (msft("420.90", "421.10", at(10_000)), at(10_000)),
            (nvda("120.00", "120.05", at(10_000)), at(10_000)),
        ],
    );

    // The sweep: the clock the OMS re-plans on with nothing new arriving.
    let sweep = g.ticker(Duration::from_millis(250));

    // The cut: what the OMS sends at one instant lands at the venue a wire
    // later. The venue answers in the instant it is sent to, and its
    // reports come back up the same wire.
    let (landed, cut) = g.feedback::<Burst<Request<Ticker>>>();
    let session = SimVenue::new(VENUE, Epoch::ZERO, &touches).wire(&landed.delay(WIRE));
    let reports = session.reports.delay(WIRE);

    let (requests, pacing) = desired
        .wire_oms(CONFIG, Epoch::ZERO, &reports)
        .sweep(&sweep)
        .build();
    let requests = requests.feedback(&cut);

    let _sent = requests.with_time().for_each(|(t, burst)| {
        println!("t={}ms sent:", ms(*t));
        for request in burst {
            println!("  {}", describe(request));
        }
        Ok(())
    });
    let _heard = reports.with_time().for_each(|(t, burst)| {
        println!("t={}ms heard:", ms(*t));
        for report in burst {
            println!("  {}", heard(report));
        }
        Ok(())
    });
    // The pacing, whenever it changes: what the budget held back.
    let _pacing = pacing
        .distinct()
        .with_time()
        .for_each(|(t, p): &(NanoTime, Pacing)| {
            println!("t={}ms pacing: {p:?}", ms(*t));
            Ok(())
        });

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(START),
        RunFor::Duration(Duration::from_millis(10_500)),
    )?;
    Ok(())
}
