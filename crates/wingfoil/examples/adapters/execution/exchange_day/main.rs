//! A trading day at an exchange, in a graph: no post-only, day orders, a
//! halt and a close.
//!
//! `cargo run -p wingfoil --example execution_exchange_day --features execution-testing`
//!
//! `Config::passive = Limit` rests quotes as plain limits where the venue
//! has no post-only; `Config::lifetime = Day` makes them day orders the
//! close expires; and the venue's `TradingState`, which reaches the OMS
//! node through the session, holds all but cancels while the book is not
//! open. The day's states and the desk's decisions are replayed at their
//! instants; the test venue behind the feedback cut answers, and expires
//! what rests when it closes.

use std::time::Duration;

use wingfoil::adapters::execution::edge::{Report, Request, TradingState};
use wingfoil::adapters::execution::oms::{Config, Desired, Lifetime, OmsOps, Passive};
use wingfoil::adapters::execution::order::Epoch;
use wingfoil::adapters::execution::rate_limit::{OrderRate, Terms};
use wingfoil::adapters::execution::testing::{Profile, SimVenue, Touch};
use wingfoil::adapters::execution::venue::Venue;
use wingfoil::adapters::market::{Level, Px, Side, px, qty};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Stock {
    #[default]
    Aapl,
    Msft,
}

/// The 09:30 open, day one.
const OPEN: NanoTime = NanoTime::ZERO;
const LATENCY: Duration = Duration::from_millis(1);

const RATE: OrderRate = OrderRate::stated(Terms::per_second(50, 50), Terms::per_second(1, 1), 1.0);

/// Plain limits, day orders, a decision believed for a day.
const CONFIG: Config = Config::unmetered()
    .with_rate(RATE)
    .with_passive(Passive::Limit)
    .with_lifetime(Lifetime::Day)
    .with_max_desired_age(Duration::from_secs(24 * 3600));

/// Minutes after the open.
fn at(minutes: u64) -> NanoTime {
    OPEN + Duration::from_secs(minutes * 60)
}

/// `at` inverted, as a clock on the wall.
fn clock(t: NanoTime) -> String {
    let wall = 9 * 60 + 30 + u64::from(t - OPEN) / 60_000_000_000;
    let (day, minute) = (wall / 1_440 + 1, wall % 1_440);
    let (hour, minute) = (minute / 60, minute % 60);
    format!("day {day} {hour:02}:{minute:02}")
}

fn touch(stock: Stock, bid: Px, ask: Px) -> Touch<Stock> {
    Touch {
        instrument: stock,
        bid: Some(bid),
        ask: Some(ask),
    }
}

fn level(price: Px) -> Level {
    Level::new(price, qty!(100))
}

fn two_way(stock: Stock, bid: Px, ask: Px, as_of: NanoTime) -> Desired<Stock> {
    Desired::two_way(stock, as_of, level(bid), level(ask))
}

fn replay<T: Clone + Default + 'static>(
    g: &GraphBuilder,
    rows: Vec<(T, NanoTime)>,
) -> Stream<Burst<T>> {
    g.replay_results(rows.into_iter().map(Ok))
}

fn describe(request: &Request<Stock>) -> String {
    match request {
        Request::Place(o) => format!(
            "place  {} {:?} {:?} {} @ {} ({:?}, {:?})",
            o.id.sequence(),
            o.instrument,
            o.side,
            o.qty,
            o.price.map_or("market".to_string(), |p| p.to_string()),
            o.kind,
            o.tif
        ),
        Request::Amend(a) => format!("amend  {} → {} @ {}", a.order.sequence(), a.qty, a.price),
        Request::Cancel(id) => format!("cancel {}", id.sequence()),
        Request::CancelAll => "cancel all".to_string(),
    }
}

fn heard(report: &Report<Stock>) -> String {
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

    let touches = replay(
        &g,
        vec![
            (touch(Stock::Aapl, px!(190.00), px!(190.05)), at(0)),
            (touch(Stock::Msft, px!(421.00), px!(421.10)), at(0)),
        ],
    );

    // The venue's day: a volatility halt at 10:00, open again at 10:05,
    // the close at 16:00, and the next day's open and close.
    let venue_states = replay(
        &g,
        vec![
            (TradingState::Halted, at(30)),
            (TradingState::Open, at(35)),
            (TradingState::Closed, at(390)),
            (TradingState::Open, at(1_440)),
            (TradingState::Closed, at(1_830)),
        ],
    )
    .filter_map(|states: &Burst<TradingState>| states.last().copied());

    // The desk's decisions.
    let desired = replay(
        &g,
        vec![
            // 09:31: quote both. This exchange has no post-only; the quotes
            // rest as day limits.
            (two_way(Stock::Aapl, px!(189.95), px!(190.10), at(1)), at(1)),
            (two_way(Stock::Msft, px!(420.95), px!(421.15), at(1)), at(1)),
            // 10:01, halted: re-price AAPL and pull MSFT. The withdrawal
            // goes; the re-price waits for the open.
            (
                two_way(Stock::Aapl, px!(189.90), px!(190.15), at(31)),
                at(31),
            ),
            (Desired::nothing(Stock::Msft, at(31)), at(31)),
            // 16:01, closed: still wanted, and nothing is sent into the
            // closed book. The next open quotes it.
            (
                two_way(Stock::Aapl, px!(189.90), px!(190.15), at(391)),
                at(391),
            ),
            // Day two, 09:31: take the MSFT offer. A cross is a limit,
            // immediate-or-cancel, capped at its price — and one-shot: it
            // fills as a taker and is not placed again, where a resting
            // bid priced through the ask would be wanted again the instant
            // it filled.
            (
                Desired::cross(Stock::Msft, at(1_441), Side::Bid, level(px!(421.20))),
                at(1_441),
            ),
        ],
    );

    let FeedbackLoop {
        feedback: requests,
        out: (reports, trading),
    } = g.feedback_loop(|sent_requests| {
        let session = SimVenue::new(Profile::EXCHANGE, Epoch::ZERO, &touches)
            .with_trading(&venue_states)
            .wire(&sent_requests.delay(LATENCY));
        let reports = session.reports.delay(LATENCY);
        // The venue's day as the OMS hears it: stated back by the session, a
        // wire later.
        let trading = session.trading.delay(LATENCY);
        let (requests, _pacing) = desired
            .wire_oms(CONFIG, Epoch::ZERO, &reports)
            .trading(&trading)
            .build();
        FeedbackLoop {
            feedback: requests,
            out: (reports, trading),
        }
    });

    // Printed as the OMS hears it, so each state lands beside what it
    // holds or lets go.
    let _states = trading.with_time().for_each(|(t, state)| {
        println!("{} [{state:?}]", clock(*t));
        Ok(())
    });
    let _sent = requests.with_time().for_each(|(t, burst)| {
        for request in burst {
            println!("{} sent  {}", clock(*t), describe(request));
        }
        Ok(())
    });
    let _heard = reports.with_time().for_each(|(t, burst)| {
        for report in burst {
            println!("{} heard {}", clock(*t), heard(report));
        }
        Ok(())
    });

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(OPEN),
        RunFor::Duration(Duration::from_secs(1_832 * 60)),
    )?;
    Ok(())
}
