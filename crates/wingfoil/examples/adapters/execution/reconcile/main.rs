//! Position reconciliation: a book read against what the venue says it
//! holds, and moved to the venue past a grace.
//!
//! `cargo run -p wingfoil --example execution_reconcile --features execution`
//!
//! The venue is the truth. A fill lost with a session, a position held
//! before the process started, a trade placed by hand — none reach the fold,
//! so when the two disagree for longer than the grace, the fold is re-based
//! on the venue at its own mark and the caller is told, to latch its risk.

use std::collections::HashMap;
use std::time::Duration;

use wingfoil::adapters::execution::edge::{Held, Holdings};
use wingfoil::adapters::execution::exec_id::ExecId;
use wingfoil::adapters::execution::order::{ClientOrderId, Fill, Liquidity};
use wingfoil::adapters::execution::position::{Book, Measure};
use wingfoil::adapters::execution::reconcile::{Config, Fold, Reconciler};
use wingfoil::adapters::market::{Px, Qty, Side};
use wingfoil::{Burst, NanoTime};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Ticker {
    #[default]
    Aapl,
    Msft,
}

/// A book and the marks a re-base is booked at: the caller's fold.
struct Desk {
    book: Book<Ticker>,
    marks: HashMap<Ticker, Px>,
}

impl Fold<Ticker> for Desk {
    fn positions(&self) -> &Book<Ticker> {
        &self.book
    }

    fn rebase(&mut self, _now: NanoTime, instrument: Ticker, net: Qty) {
        let mark = self.marks.get(&instrument).copied();
        self.book.rebase(instrument, net, mark);
    }
}

fn secs(s: u64) -> NanoTime {
    NanoTime::from(1_000_000 * 1_000_000_000 + s * 1_000_000_000)
}

fn q(s: &str) -> Qty {
    Qty::parse(s).unwrap()
}

fn holdings(whole: bool, held: &[(Ticker, &str)], at: NanoTime) -> Holdings<Ticker> {
    Holdings {
        whole,
        held: held
            .iter()
            .map(|(instrument, net)| Held {
                instrument: *instrument,
                net: q(net),
            })
            .collect::<Burst<Held<Ticker>>>(),
        as_of: at,
    }
}

fn main() {
    let mut desk = Desk {
        book: Book::new(|_| Some(Measure::Linear)),
        marks: [
            (Ticker::Aapl, Px::parse("190").unwrap()),
            (Ticker::Msft, Px::parse("420").unwrap()),
        ]
        .into(),
    };
    desk.book.apply(&[Fill {
        order: ClientOrderId(1),
        exec_id: ExecId::new("e1").unwrap(),
        instrument: Ticker::Aapl,
        side: Side::Bid,
        qty: q("100"),
        filled: q("100"),
        remaining: Qty::ZERO,
        price: Px::parse("189").unwrap(),
        fee: Qty::ZERO,
        liquidity: Liquidity::Maker,
        venue_time: None,
        recv_time: secs(0),
    }]);

    let mut reconciler = Reconciler::new(Config {
        grace: Duration::from_secs(5),
    });

    // Before the venue has spoken, nothing is judged.
    assert!(reconciler.reconcile(secs(1), &mut desk).is_empty());
    println!("t=1  venue silent: nothing judged");

    // The venue's snapshot agrees on AAPL but holds 30 MSFT the fold never
    // saw — a fill lost with a dropped session, say.
    reconciler.heard(&holdings(
        true,
        &[(Ticker::Aapl, "100"), (Ticker::Msft, "30")],
        secs(2),
    ));
    assert!(reconciler.reconcile(secs(2), &mut desk).is_empty());
    println!(
        "t=2  MSFT disagrees: {} inside its grace",
        reconciler.disagreeing()
    );

    // Past the grace it is a mismatch: the fold moves to the venue.
    let found = reconciler.reconcile(secs(8), &mut desk);
    for m in &found {
        println!(
            "t=8  mismatch on {:?}: ours {}, venue {} since {} → re-based",
            m.instrument, m.ours, m.venue, m.since
        );
    }
    assert_eq!(found.len(), 1);
    assert_eq!(desk.book.net(&Ticker::Msft), q("30"));
    println!(
        "     MSFT entry now {:?}, the book's own mark",
        desk.book.position(&Ticker::Msft).unwrap().entry()
    );

    // Agreeing again is quiet.
    assert!(reconciler.reconcile(secs(9), &mut desk).is_empty());
    println!(
        "t=9  agreed; re-based {} position(s) in all",
        reconciler.rebased()
    );
}
