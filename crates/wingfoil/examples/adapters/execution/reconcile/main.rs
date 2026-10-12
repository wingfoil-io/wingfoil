//! Position reconciliation in a graph: a book read against what the venue
//! says it holds, and moved to the venue past a grace.
//!
//! `cargo run -p wingfoil --example execution_reconcile --features execution`
//!
//! The venue is the truth. A fill lost with a session, a position held
//! before the process started, a trade placed by hand — none reach the
//! fold, so when the two disagree for longer than the grace, the fold is
//! re-based on the venue at its own mark and the caller is told, to latch
//! its risk.
//!
//! The book and the `Reconciler` are node state in the library's
//! reconciliation node (`ReconcileOps`) — one node, because the
//! reconciler's fold *is* the book: fills, the venue's `Holdings` and the
//! marks a re-base is booked at are replayed at engine instants into it,
//! and a one-second sweep is what judges the grace at engine time with
//! nothing new arriving — the same clock a stalled session would be judged
//! by.

use std::time::Duration;

use anyhow::Result;
use wingfoil::adapters::execution::edge::{Held, Holdings};
use wingfoil::adapters::execution::exec_id::ExecId;
use wingfoil::adapters::execution::order::{ClientOrderId, Fill, Liquidity};
use wingfoil::adapters::execution::position::Measure;
use wingfoil::adapters::execution::reconcile::node::Standing;
use wingfoil::adapters::execution::reconcile::{Config, Mismatch, ReconcileOps};
use wingfoil::adapters::market::{Qty, Side, px, qty};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Ticker {
    #[default]
    Aapl,
    Msft,
}

const START: NanoTime = NanoTime::ZERO;

fn at(secs: u64) -> NanoTime {
    START + Duration::from_secs(secs)
}

fn secs(t: NanoTime) -> u64 {
    u64::from(t - START) / 1_000_000_000
}

fn holdings(whole: bool, held: &[(Ticker, Qty)], at: NanoTime) -> (Holdings<Ticker>, NanoTime) {
    let holdings = Holdings {
        whole,
        held: held
            .iter()
            .map(|(instrument, net)| Held {
                instrument: *instrument,
                net: *net,
            })
            .collect::<Burst<Held<Ticker>>>(),
        as_of: at,
    };
    (holdings, at)
}

fn print_mismatch(t: NanoTime, m: &Mismatch<Ticker>, standing: &Standing<Ticker>) {
    println!(
        "t={} mismatch on {:?}: ours {}, venue {}, since t={} → re-based",
        secs(t),
        m.instrument,
        m.ours,
        m.venue,
        secs(m.since)
    );
    let position = standing
        .positions
        .iter()
        .find(|p| p.instrument == m.instrument)
        .expect("a re-based position is in the book");
    println!(
        "     {:?} now net {}, entry {} — the book's own mark",
        m.instrument,
        position.net(),
        position.entry().map_or("-".to_string(), |p| p.to_string())
    );
}

fn replay<T: Clone + Default + 'static>(
    g: &GraphBuilder,
    rows: Vec<(T, NanoTime)>,
) -> Stream<Burst<T>> {
    g.replay_results(rows.into_iter().map(Ok))
}

fn main() -> Result<()> {
    let g = GraphBuilder::new();

    // The one fill the fold saw: 100 AAPL.
    let fills = replay(
        &g,
        vec![(
            Fill {
                order: ClientOrderId(1),
                exec_id: ExecId::new("e1").unwrap(),
                instrument: Ticker::Aapl,
                side: Side::Bid,
                qty: qty!(100),
                filled: qty!(100),
                remaining: Qty::ZERO,
                price: px!(189),
                fee: Qty::ZERO,
                liquidity: Liquidity::Maker,
                venue_time: None,
                recv_time: at(0),
            },
            at(0),
        )],
    );
    // The venue's snapshot, two seconds in: it agrees on AAPL but holds 30
    // MSFT the fold never saw — a fill lost with a dropped session, say.
    let holdings = replay(
        &g,
        vec![holdings(
            true,
            &[(Ticker::Aapl, qty!(100)), (Ticker::Msft, qty!(30))],
            at(2),
        )],
    );
    let sweep = g.ticker(Duration::from_secs(1));
    // The marks a re-base is booked at, as the venue publishes them.
    let marks = replay(
        &g,
        vec![
            ((Ticker::Aapl, px!(190)), at(0)),
            ((Ticker::Msft, px!(420)), at(0)),
        ],
    );

    let (mismatches, standing) = fills
        .wire_reconcile(
            Config {
                grace: Duration::from_secs(5),
            },
            |_: &Ticker| Some(Measure::Linear),
            &holdings,
        )
        .marks(&marks)
        .sweep(&sweep)
        .build();

    // The standing, whenever it changes.
    let _standing = standing
        .map(|s: &Standing<Ticker>| (s.heard, s.disagreeing, s.rebased))
        .distinct()
        .with_time()
        .for_each(|(t, (heard, disagreeing, rebased))| {
            let line = match (heard, disagreeing, rebased) {
                (false, _, _) => "venue silent: nothing judged".to_string(),
                (true, 0, 0) => "agreed".to_string(),
                (true, 0, n) => format!("agreed; re-based {n} position(s) in all"),
                (true, n, _) => format!("{n} position(s) disagree, inside the grace"),
            };
            println!("t={} {line}", secs(*t));
            Ok(())
        });
    // Each mismatch, as it is found and re-based, read against the fold the
    // re-base left.
    let _mismatches = mismatches
        .join_passive(&standing, |mismatches, standing| {
            (mismatches.clone(), standing.clone())
        })
        .with_time()
        .for_each(|(t, (mismatches, standing))| {
            for m in mismatches {
                print_mismatch(*t, m, standing);
            }
            Ok(())
        });

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(START),
        RunFor::Duration(Duration::from_secs(10)),
    )?;
    Ok(())
}
