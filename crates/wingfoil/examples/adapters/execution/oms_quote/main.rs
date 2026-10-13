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
use wingfoil::adapters::market::{Level, Px, Qty, px, qty};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Ticker {
    #[default]
    Aapl,
    Msft,
    Nvda,
}

impl Ticker {
    /// What each side of a quote shows.
    fn lot(self) -> Qty {
        match self {
            Ticker::Aapl => qty!(100),
            Ticker::Msft => qty!(50),
            Ticker::Nvda => qty!(200),
        }
    }
}

/// The run starts here; every instant below is milliseconds after it.
const START: NanoTime = NanoTime::ZERO;

/// The latency to the venue, each way.
const LATENCY: Duration = Duration::from_millis(1);

/// Mints the OMS's order ids. A backtest has no earlier process whose ids
/// could collide, so the epoch is zero.
const EPOCH: Epoch = Epoch::ZERO;

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

/// The market on `ticker` at `t`, as a replay row.
fn touch(t: NanoTime, ticker: Ticker, bid: Px, ask: Px) -> (Touch<Ticker>, NanoTime) {
    let touch = Touch {
        instrument: ticker,
        bid: Some(bid),
        ask: Some(ask),
    };
    (touch, t)
}

/// A two-way quote on `ticker` decided at `t`, as a replay row.
fn quote(t: NanoTime, ticker: Ticker, bid: Px, ask: Px) -> (Desired<Ticker>, NanoTime) {
    let lot = ticker.lot();
    let desired = Desired::two_way(ticker, t, Level::new(bid, lot), Level::new(ask, lot));
    (desired, t)
}

/// Nothing wanted on `ticker` from `t`, as a replay row.
fn withdraw(t: NanoTime, ticker: Ticker) -> (Desired<Ticker>, NanoTime) {
    (Desired::nothing(ticker, t), t)
}

/// Replay `rows`, each at its instant, as a burst stream.
fn replay<T: Clone + Default + 'static>(
    g: &GraphBuilder,
    rows: Vec<(T, NanoTime)>,
) -> Stream<Burst<T>> {
    g.replay_results(rows.into_iter().map(Ok))
}

/// Print each burst as it lands: the instant and `what`, then one line per
/// item.
fn print_bursts<T: Clone + Default + 'static>(
    stream: &Stream<Burst<T>>,
    what: &'static str,
    line: fn(&T) -> String,
) {
    stream.with_time().for_each(move |(t, burst)| {
        println!("t={}ms {what}:", ms(*t));
        for item in burst {
            println!("  {}", line(item));
        }
        Ok(())
    });
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
            touch(at(0), Ticker::Aapl, px!(189.54), px!(189.56)),
            touch(at(0), Ticker::Msft, px!(421.00), px!(421.02)),
            touch(at(0), Ticker::Nvda, px!(120.02), px!(120.03)),
        ],
    );

    // The strategy's decisions, replayed at the instants it made them.
    let desired = replay(
        &g,
        vec![
            // Nothing working: two two-ways are four post-only places —
            // exactly the burst the venue allows.
            quote(at(0), Ticker::Aapl, px!(189.50), px!(189.60)),
            quote(at(0), Ticker::Msft, px!(420.90), px!(421.10)),
            // The touch moves on AAPL only: one amend per side that changed,
            // and MSFT, which wants what is working, is left alone.
            quote(at(300), Ticker::Aapl, px!(189.55), px!(189.65)),
            quote(at(300), Ticker::Msft, px!(420.90), px!(421.10)),
            // Withdrawing is a decision: an empty desired cancels at once.
            withdraw(at(600), Ticker::Msft),
            // AAPL is then left alone past `max_desired_age`, and the OMS
            // withdraws it the instant it stales, without a decision — it
            // wakes itself for that; nothing here ticks a clock.
            //
            // Three two-ways at once are six places; four go now, two wait —
            // not queued, re-planned at the instant each token has refilled
            // (a tenth of a second apart at 10 a second).
            quote(at(10_000), Ticker::Aapl, px!(189.50), px!(189.60)),
            quote(at(10_000), Ticker::Msft, px!(420.90), px!(421.10)),
            quote(at(10_000), Ticker::Nvda, px!(120.00), px!(120.05)),
            // Then the strategy falls silent: five seconds on, all six
            // quotes stale at once, and their cancels are paced by the
            // same budget — four on the burst, the last two a token apart.
        ],
    );

    // The loop: requests reach the venue `LATENCY` later; reports reach the
    // OMS `LATENCY` after that. `sent_requests` is the `requests` returned
    // below, one instant late.
    let (requests, (reports, pacing)) = g.feedback_loop(|sent_requests| {
        let session = SimVenue::new(VENUE, EPOCH, &touches).wire(&sent_requests.delay(LATENCY));
        let reports = session.reports.delay(LATENCY);
        let (requests, pacing) = desired.wire_oms(CONFIG, EPOCH, &reports).build();
        FeedbackLoop {
            feedback: requests,
            out: (reports, pacing),
        }
    });

    print_bursts(&requests, "sent", describe);
    print_bursts(&reports, "heard", heard);
    // The pacing, whenever it changes: what the budget held back.
    pacing
        .distinct()
        .with_time()
        .for_each(|(t, p): &(NanoTime, Pacing)| {
            println!("t={}ms pacing: {p:?}", ms(*t));
            Ok(())
        });

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(START),
        RunFor::Duration(Duration::from_millis(15_500)),
    )?;
    Ok(())
}
