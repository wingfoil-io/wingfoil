#![cfg(feature = "market")]
//! Market data adapter tests — the `order_book` op on a real graph.
//!
//! The book state machine itself is unit-tested in
//! `src/adapters/market.rs`; what these cover is the engine-facing half:
//! tick times, burst handling, and the gap contract as a downstream sees it.

use std::sync::Arc;
use std::time::Duration;

use wingfoil::adapters::market::{
    BookDelta, BookSnapshot, BookStatus, BookUpdate, GapCause, InstrumentId, Level, LevelChange,
    MarketBookOps, MarketEvent, MarketEventOps, OrderBook, Px, Qty, Sequencing, Side, Trade,
};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

fn inst() -> InstrumentId {
    InstrumentId::new("test", "BTC-USD")
}

fn snapshot(seq: u64, bid: (&str, &str), ask: (&str, &str)) -> BookUpdate {
    BookUpdate::Snapshot(BookSnapshot {
        instrument: inst(),
        bids: vec![Level::new(
            Px::parse(bid.0).unwrap(),
            Qty::parse(bid.1).unwrap(),
        )],
        asks: vec![Level::new(
            Px::parse(ask.0).unwrap(),
            Qty::parse(ask.1).unwrap(),
        )],
        sequencing: Sequencing::Single(seq),
        venue_time: None,
        recv_time: NanoTime::ZERO,
    })
}

fn delta(seq: u64, side: Side, price: &str, qty: &str) -> BookUpdate {
    BookUpdate::Delta(BookDelta {
        instrument: inst(),
        changes: vec![LevelChange::new(
            side,
            Px::parse(price).unwrap(),
            Qty::parse(qty).unwrap(),
        )],
        sequencing: Sequencing::Single(seq),
        venue_time: None,
        recv_time: NanoTime::ZERO,
    })
}

/// The op ticks once per advancing update, at the update's own time, and the
/// book carried by each tick reflects everything applied so far.
#[test]
fn order_book_ticks_at_update_times() {
    let g = GraphBuilder::new();
    let (updates, sender) = g.channel::<BookUpdate>();
    let mids = updates.order_book().map(|b| b.mid());
    let acc = mids.with_time().accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send_at(
            snapshot(1, ("100.0", "1"), ("102.0", "1")),
            NanoTime::new(100),
        );
        sender.send_at(delta(2, Side::Bid, "101.0", "1"), NanoTime::new(200));
        sender.send_at(delta(3, Side::Ask, "102.0", "0"), NanoTime::new(300));
        sender.close();
    });

    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    producer.join().expect("producer thread");

    assert_eq!(
        vec![
            // Snapshot: mid of 100 / 102.
            (NanoTime::new(100), Some(101.0)),
            // Better bid at 101 against the 102 ask.
            (NanoTime::new(200), Some(101.5)),
            // The only ask was removed, so there is no mid to compute.
            (NanoTime::new(300), None),
        ],
        r.value(&acc)
    );
}

/// Every update in a same-instant burst is applied in order. Collapsing the
/// burst latest-wins would drop the intervening level changes — the book would
/// be missing the 100.5 bid entirely and would show the wrong touch.
#[test]
fn burst_applies_every_update_not_just_the_last() {
    let g = GraphBuilder::new();
    let (updates, sender) = g.channel::<BookUpdate>();
    let books = updates.order_book();
    let acc = books
        .map(|b| {
            (
                b.best_bid().map(|l| l.price.to_string()),
                b.level_count(Side::Bid),
            )
        })
        .with_time()
        .accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send_at(
            snapshot(1, ("100.0", "1"), ("110.0", "1")),
            NanoTime::new(100),
        );
        // Three level insertions at one instant — one burst, one tick.
        sender.send_at(delta(2, Side::Bid, "100.5", "1"), NanoTime::new(200));
        sender.send_at(delta(3, Side::Bid, "100.75", "1"), NanoTime::new(200));
        sender.send_at(delta(4, Side::Bid, "100.25", "1"), NanoTime::new(200));
        sender.close();
    });

    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    producer.join().expect("producer thread");

    assert_eq!(
        vec![
            (NanoTime::new(100), (Some("100".to_string()), 1)),
            // All four levels present, and the touch is the best of them —
            // not the last one sent (100.25).
            (NanoTime::new(200), (Some("100.75".to_string()), 4)),
        ],
        r.value(&acc)
    );
}

/// A gap ticks rather than going quiet, and what downstream sees is a book
/// that refuses to quote. Silence here would leave a strategy trading off the
/// last good value indefinitely.
#[test]
fn gap_ticks_and_downstream_sees_an_unquotable_book() {
    let g = GraphBuilder::new();
    let (updates, sender) = g.channel::<BookUpdate>();
    let acc = updates
        .order_book()
        .map(|b: &Arc<OrderBook>| (b.status(), b.best_bid().is_some()))
        .with_time()
        .accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send_at(
            snapshot(10, ("100.0", "1"), ("102.0", "1")),
            NanoTime::new(100),
        );
        // 11 expected, 13 delivered: 11 and 12 were lost.
        sender.send_at(delta(13, Side::Bid, "101.0", "1"), NanoTime::new(200));
        // A gapped book refuses further deltas — no tick at 300.
        sender.send_at(delta(14, Side::Bid, "101.5", "1"), NanoTime::new(300));
        // A fresh snapshot recovers it.
        sender.send_at(
            snapshot(20, ("103.0", "1"), ("104.0", "1")),
            NanoTime::new(400),
        );
        sender.close();
    });

    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    producer.join().expect("producer thread");

    assert_eq!(
        vec![
            (NanoTime::new(100), (BookStatus::Live, true)),
            (NanoTime::new(200), (BookStatus::Gapped, false)),
            (NanoTime::new(400), (BookStatus::Live, true)),
        ],
        r.value(&acc)
    );
}

/// Deltas arriving before the snapshot stay quiet (they are buffered), then
/// the snapshot ticks once with them replayed on top.
#[test]
fn pre_snapshot_deltas_are_quiet_then_replayed() {
    let g = GraphBuilder::new();
    let (updates, sender) = g.channel::<BookUpdate>();
    let acc = updates
        .order_book()
        .map(|b| b.best_bid().map(|l| l.qty.to_string()))
        .with_time()
        .accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        // Stream first, snapshot second — the universal REST+WS race.
        sender.send_at(delta(5, Side::Bid, "100.0", "5"), NanoTime::new(100));
        sender.send_at(delta(6, Side::Bid, "100.0", "6"), NanoTime::new(200));
        sender.send_at(delta(7, Side::Bid, "100.0", "7"), NanoTime::new(300));
        // Snapshot current as of 6: delta 5 and 6 are dropped, 7 replays.
        sender.send_at(
            snapshot(6, ("99.0", "1"), ("101.0", "1")),
            NanoTime::new(400),
        );
        sender.close();
    });

    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    producer.join().expect("producer thread");

    // Nothing at 100/200/300 — those were buffered, not applied.
    assert_eq!(
        vec![(NanoTime::new(400), Some("7".to_string()))],
        r.value(&acc)
    );
}

/// A stream carrying two instruments is a wiring bug, and it aborts the run
/// with a message naming both rather than interleaving them into one book.
#[test]
fn mixed_instruments_abort_the_run() {
    let g = GraphBuilder::new();
    let (updates, sender) = g.channel::<BookUpdate>();
    let _books = updates.order_book();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send_at(
            snapshot(1, ("100.0", "1"), ("102.0", "1")),
            NanoTime::new(100),
        );
        let other = BookUpdate::Snapshot(BookSnapshot {
            instrument: InstrumentId::new("test", "ETH-USD"),
            bids: vec![],
            asks: vec![],
            sequencing: Sequencing::Single(2),
            venue_time: None,
            recv_time: NanoTime::ZERO,
        });
        sender.send_at(other, NanoTime::new(200));
        sender.close();
    });

    let err = r
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .expect_err("a second instrument must abort the run");
    producer.join().expect("producer thread");

    let msg = format!("{err:#}");
    assert!(msg.contains("test:ETH-USD"), "error was: {msg}");
    assert!(msg.contains("test:BTC-USD"), "error was: {msg}");
}

/// Book maintenance is deterministic under replay: the same timestamped
/// updates produce the same values at the same times on every run. This is the
/// property that makes a recorded venue session a usable backtest.
#[test]
fn replay_is_deterministic() {
    let run_once = || {
        let g = GraphBuilder::new();
        let (updates, sender) = g.channel::<BookUpdate>();
        let acc = updates
            .order_book()
            .map(|b| b.mid())
            .with_time()
            .accumulate();
        let mut r = g.build();

        let producer = std::thread::spawn(move || {
            sender.send_at(
                snapshot(1, ("100.0", "1"), ("102.0", "1")),
                NanoTime::new(100),
            );
            // A wall-clock delay must not perturb the replayed tick times.
            std::thread::sleep(Duration::from_millis(3));
            sender.send_at(delta(2, Side::Bid, "101.0", "1"), NanoTime::new(200));
            sender.send_at(delta(3, Side::Ask, "103.0", "1"), NanoTime::new(300));
            sender.close();
        });

        r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        producer.join().expect("producer thread");
        r.value(&acc)
    };

    let first = run_once();
    assert_eq!(first, run_once());
    assert_eq!(
        vec![
            (NanoTime::new(100), Some(101.0)),
            (NanoTime::new(200), Some(101.5)),
            (NanoTime::new(300), Some(101.5)),
        ],
        first
    );
}

/// The shape a real venue adapter actually produces: one multiplexed
/// `Stream<Burst<MarketEvent>>`, demultiplexed into trades and book messages
/// with the burst preserved on both branches.
///
/// This is the wiring the module docs show. It had no `MarketEventOps` impl at
/// all until review caught it, so the documented example could not compile
/// against the stream a `channel` source hands you.
#[test]
fn market_events_demultiplex_preserving_bursts() {
    let g = GraphBuilder::new();
    let (events, sender) = g.channel::<MarketEvent>();

    let books = events.book_updates().order_book();
    let mids = books.map(|b| b.mid()).with_time().accumulate();
    // Count per burst as well as the values, so a collapsed burst would show up
    // as a shorter group rather than silently matching.
    let trades = events
        .trades()
        .map(|b| {
            b.iter()
                .map(|t| t.price.to_string())
                .collect::<Vec<_>>()
                .join(",")
        })
        .with_time()
        .accumulate();

    let mut r = g.build();

    let feed = inst();
    let producer = std::thread::spawn(move || {
        // A book snapshot and two trades, all at the same instant: one burst
        // carrying all three, which must split into one book tick and one
        // two-element trade burst.
        sender.send_at(
            MarketEvent::Book(snapshot(1, ("100.0", "1"), ("102.0", "1"))),
            NanoTime::new(100),
        );
        sender.send_at(
            MarketEvent::Trade(Trade {
                instrument: feed.clone(),
                price: Px::parse("101.0").unwrap(),
                qty: Qty::parse("1").unwrap(),
                aggressor: Some(Side::Bid),
                trade_id: None,
                venue_time: None,
                recv_time: NanoTime::new(100),
            }),
            NanoTime::new(100),
        );
        sender.send_at(
            MarketEvent::Trade(Trade {
                instrument: feed.clone(),
                price: Px::parse("101.5").unwrap(),
                qty: Qty::parse("2").unwrap(),
                aggressor: Some(Side::Ask),
                trade_id: None,
                venue_time: None,
                recv_time: NanoTime::new(100),
            }),
            NanoTime::new(100),
        );
        // A book-only instant: the trade branch must stay quiet rather than
        // ticking an empty burst.
        sender.send_at(
            MarketEvent::Book(delta(2, Side::Bid, "101.0", "1")),
            NanoTime::new(200),
        );
        sender.close();
    });

    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    producer.join().expect("producer thread");

    assert_eq!(
        vec![
            (NanoTime::new(100), Some(101.0)),
            (NanoTime::new(200), Some(101.5)),
        ],
        r.value(&mids)
    );
    // Both trades rode one burst, and the book-only instant produced no tick.
    assert_eq!(
        vec![(NanoTime::new(100), "101,101.5".to_string())],
        r.value(&trades)
    );
}

/// A gap's sequence ids survive the trip through the graph on the book itself,
/// so a downstream monitor can log which updates were lost — the `BookApply`
/// the op matched on does not reach anyone.
#[test]
fn gap_cause_reaches_downstream() {
    let g = GraphBuilder::new();
    let (updates, sender) = g.channel::<BookUpdate>();
    let acc = updates
        .order_book()
        .map(|b| b.gap_cause())
        .with_time()
        .accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send_at(
            snapshot(10, ("100.0", "1"), ("102.0", "1")),
            NanoTime::new(100),
        );
        // 11 is expected, 13 arrives: 11 and 12 were lost.
        sender.send_at(delta(13, Side::Bid, "101.0", "1"), NanoTime::new(200));
        sender.close();
    });

    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    producer.join().expect("producer thread");

    assert_eq!(
        vec![
            (NanoTime::new(100), None),
            (
                NanoTime::new(200),
                Some(GapCause::Sequence {
                    expected: 11,
                    got: 13
                })
            ),
        ],
        r.value(&acc)
    );
}
