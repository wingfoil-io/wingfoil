//! Position reconciliation: the position fold read against what the venue
//! says it holds.
//!
//! The order half of reconciliation belongs to a venue's integration,
//! because its rules are about what one venue's session does on connect.
//! This half is not: it compares two numbers per instrument — ours and the
//! venue's — and every venue that can say what it holds can be reconciled
//! the same way. The venue's side arrives as [`Holdings`]; the fold it
//! corrects is whatever implements [`Fold`] — a [`Book`] and a mark to
//! re-base at.
//!
//! # The venue is the truth
//!
//! It holds of orders and it holds of positions more strongly: the fold is our account of what the venue did, assembled from
//! the reports and settlements we were sent, and it is right only while
//! every one of them reached us. It stops being right when one does not — a
//! fill lost with the session it happened on (cancel-on-disconnect pulls the
//! orders, not what they had already done), a position the account held
//! before this process started (a restart restores the fold's own account
//! of it, and the venue is still the truth about it), a trade
//! somebody placed by hand. None of those can reach the fold by any
//! path it is fed, so when the two disagree it is the fold that moves.
//!
//! # A difference counts only past a grace
//!
//! The two are not stated at the same instant, and a difference that is only
//! the order two messages arrived in is not a disagreement. A fill and the
//! position it left usually land together — but a delivery may not: the
//! venue closes the position at the cut, and the settlement that tells the
//! fold can arrive on a later poll of the history. So a difference is held,
//! from the instant it is first seen, for [`Config::grace`], and only one
//! that outlives it is a [`Mismatch`]. The grace wants to sit comfortably
//! past the slowest of those lags and short of
//! how long we are willing to hedge a book we have miscounted.
//!
//! **Sizes compare exactly.** Both are the venue's own decimals — the size a
//! fill printed and the size a position is stated at — in the same unit, so
//! there is no rounding for a tolerance to absorb. A tolerance in size would
//! be a position allowed to be wrong by that much for ever.
//!
//! # What a mismatch does
//!
//! Two things, and neither is enough alone:
//!
//! - **The fold is re-based on the venue** ([`Fold::rebase`]), the
//!   difference booked at the book's own mark. Everything downstream — the
//!   delta a hedger works, the inventory a quoter skews on, what the limits
//!   cap — is read off the fold, and while it is wrong they are numbers
//!   about a book nobody holds. A reconciliation that only reported would
//!   leave a hedger flattening a fiction.
//! - **The caller's risk limits latch** on every [`Mismatch`] returned, so
//!   a kill switch trips — cancel-all, then a hedge of what the *re-based*
//!   book holds — and nothing is quoted until an operator clears it. The
//!   fold agreeing again is not the question answered; *why* it did not is,
//!   and a re-base that carried on quoting would be a gap papered over.
//!
//! A fact that arrives after the re-base — a delivery booked by a poll the
//! grace did not outlast, say — makes the fold disagree again, and it is
//! re-based again after the grace. So the fold follows the venue, late and
//! loudly, and never the other way round; [`Reconciler::rebased`] counts how
//! often.
//!
//! # The venue's side
//!
//! What the venue has stated is kept per instrument, and only while it is
//! not flat. A [`Holdings`] that is [`whole`](Holdings::whole) replaces all
//! of it — anything a snapshot does not name is flat — and one that is not
//! replaces only what it names. A settlement ([`Reconciler::settled`]) is
//! the venue's own statement that a position is over, so it records the
//! instrument flat on the venue's side too: not every venue restates a
//! delivered position, and one that does not would otherwise leave the
//! venue's side holding a contract that no longer exists.
//!
//! A venue that has said nothing *yet* is not judged against. Until the
//! first [`Holdings`] the fold is left alone — no difference is seen, no
//! grace starts — because a fold restored across a restart read against an empty venue would re-base every position it carried to
//! zero at the mark, latch, and re-base again when the snapshot lands,
//! losing the carries it was restored to keep. Once the venue has spoken,
//! what it has not named is flat, and a fold that disagrees is held to the
//! grace as ever. A live session asks for a snapshot on every connect; a
//! venue that never answered one would reconcile nothing, and quietly.
//!
//! # What a backtest proves
//!
//! Only that it is quiet. The sim states its own book, which is fed the same
//! reports as the strategy's, so the two agree by construction; a
//! disagreement there is a bug in the sim, not a case of this. The cases
//! this exists for are tested with scripted holdings.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::time::Duration;

use crate::NanoTime;
use crate::adapters::market::Qty;

use crate::adapters::execution::edge::Holdings;
use crate::adapters::execution::order::{Fill, Instrument};
use crate::adapters::execution::position::Book;

/// The fold reconciliation reads and corrects: a position [`Book`], and a
/// way to move one of its positions to the venue's net at the fold's own
/// mark.
///
/// A trait rather than a `Book` and a mark function, because whatever owns
/// the book usually owns the marks too — the exposure fold, say — and could
/// not lend both at once.
pub trait Fold<I> {
    /// The positions.
    fn positions(&self) -> &Book<I>;

    /// Move `instrument`'s position to `net` as though the difference had
    /// traded at the fold's own mark at `now` — [`Book::rebase`].
    fn rebase(&mut self, now: NanoTime, instrument: I, net: Qty);
}

/// How reconciliation is run.
///
/// No `Default`, like the limits': the grace is a threshold the venue's own
/// lags set, and an invented one reads as a decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// How long the fold may disagree with the venue about one instrument
    /// before that is a [`Mismatch`]. See the module docs for what it has to
    /// outlast.
    ///
    /// Off is `Duration::MAX`: no difference is ever a mismatch, and the
    /// fold is never re-based.
    pub grace: Duration,
}

/// One instrument the fold disagreed with the venue on for longer than the
/// grace — and has now been re-based on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mismatch<I> {
    /// The contract.
    pub instrument: I,
    /// What the fold held.
    pub ours: Qty,
    /// What the venue says it holds, and what the fold now holds.
    pub venue: Qty,
    /// The engine time the difference was first seen.
    pub since: NanoTime,
}

/// The venue's side of the book, and how long each difference has stood.
#[derive(Clone, Debug)]
pub struct Reconciler<I> {
    config: Config,
    /// What the venue has said it holds, non-zero only: flat and unstated
    /// are the same fact. Ordered, as `since` is, so [`reconcile`]'s walk
    /// is in instrument order without a sort.
    ///
    /// [`reconcile`]: Reconciler::reconcile
    venue: BTreeMap<I, Qty>,
    /// When each standing difference was first seen.
    since: BTreeMap<I, NanoTime>,
    /// Whether the venue has stated anything at all. See the module docs.
    heard: bool,
    rebased: u64,
}

impl<I: Instrument> Reconciler<I> {
    /// Nothing heard from the venue, and nothing disagreeing.
    pub fn new(config: Config) -> Reconciler<I> {
        Reconciler {
            config,
            venue: BTreeMap::new(),
            since: BTreeMap::new(),
            heard: false,
            rebased: 0,
        }
    }

    /// What it runs on.
    pub const fn config(&self) -> &Config {
        &self.config
    }

    /// Fold in what the venue says it holds.
    pub fn heard(&mut self, holdings: &Holdings<I>) {
        self.heard = true;
        if holdings.whole {
            self.venue.clear();
        }
        for held in &holdings.held {
            if held.net == Qty::ZERO {
                self.venue.remove(&held.instrument);
            } else {
                self.venue.insert(held.instrument, held.net);
            }
        }
    }

    /// The venue closed these at their cut: its side is flat in each.
    pub fn settled(&mut self, fills: &[Fill<I>]) {
        for fill in fills {
            self.venue.remove(&fill.instrument);
        }
    }

    /// Whether the venue has stated anything yet. Until it has, nothing is
    /// reconciled.
    pub const fn has_heard(&self) -> bool {
        self.heard
    }

    /// What the venue last said it holds in `instrument` — zero where it has
    /// said nothing, or said flat.
    pub fn venue(&self, instrument: &I) -> Qty {
        self.venue.get(instrument).copied().unwrap_or(Qty::ZERO)
    }

    /// Differences seen and still inside their grace, as of the last
    /// [`reconcile`](Self::reconcile).
    pub fn disagreeing(&self) -> usize {
        self.since.len()
    }

    /// How many positions have been re-based on the venue, ever.
    pub const fn rebased(&self) -> u64 {
        self.rebased
    }

    /// Read the fold against the venue at `now`, re-base every position that
    /// has disagreed for longer than the grace, and say which.
    ///
    /// In instrument order, so a replay re-bases in the order the run did. A
    /// difference first seen now starts its grace now; one that has closed
    /// forgets it, so a difference that comes back is a new one.
    pub fn reconcile(&mut self, now: NanoTime, fold: &mut impl Fold<I>) -> Vec<Mismatch<I>> {
        if !self.heard {
            return Vec::new();
        }
        let book = fold.positions();
        // Every instrument either side holds, or that disagreed last time:
        // three walks already in instrument order, merged rather than
        // sorted. Collected because the walk below changes `since`.
        let instruments: Vec<I> = union(
            union(self.venue.keys().copied(), self.since.keys().copied()),
            book.iter()
                .filter(|position| !position.is_flat())
                .map(|position| position.instrument),
        )
        .collect();

        let mut found = Vec::new();
        for instrument in instruments {
            let ours = book.net(&instrument);
            let venue = self.venue(&instrument);
            if ours == venue {
                self.since.remove(&instrument);
                continue;
            }
            let since = *self.since.entry(instrument).or_insert(now);
            // The time elapsed, not `since + grace`: that add is an
            // unchecked `u64` one, and `Duration::MAX` would overflow it.
            if now >= since && Duration::from(now - since) >= self.config.grace {
                self.since.remove(&instrument);
                found.push(Mismatch {
                    instrument,
                    ours,
                    venue,
                    since,
                });
            }
        }
        for mismatch in &found {
            log::error!(
                "reconcile: the fold held {} of {:?} and the venue {} since {} — re-based on the venue",
                mismatch.ours,
                mismatch.instrument,
                mismatch.venue,
                mismatch.since,
            );
            fold.rebase(now, mismatch.instrument, mismatch.venue);
            self.rebased += 1;
        }
        found
    }
}

/// The union of two ascending walks with no repeats, ascending with no
/// repeats: a merge, which is what three ordered maps need where a sort
/// and a dedup would be paying to rediscover an order they already have.
fn union<I: Ord>(
    a: impl Iterator<Item = I>,
    b: impl Iterator<Item = I>,
) -> impl Iterator<Item = I> {
    let (mut a, mut b) = (a.peekable(), b.peekable());
    std::iter::from_fn(move || match (a.peek(), b.peek()) {
        (Some(x), Some(y)) => match x.cmp(y) {
            Ordering::Less => a.next(),
            Ordering::Greater => b.next(),
            Ordering::Equal => {
                b.next();
                a.next()
            }
        },
        (Some(_), None) => a.next(),
        (None, _) => b.next(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Burst;
    use crate::adapters::execution::edge::Held;
    use crate::adapters::execution::exec_id::ExecId;
    use crate::adapters::execution::order::{ClientOrderId, Liquidity};
    use crate::adapters::execution::position::Measure;
    use crate::adapters::market::{Px, Side};

    const SEC: u64 = 1_000_000_000;

    /// A call, linear in its premium, and a perp, inverse.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
    enum Inst {
        Call,
        #[default]
        Perp,
    }

    type Reconciler = super::Reconciler<Inst>;
    type Holdings = crate::adapters::execution::edge::Holdings<Inst>;
    type Fill = crate::adapters::execution::order::Fill<Inst>;
    type Book = crate::adapters::execution::position::Book<Inst>;

    fn measure(instrument: &Inst) -> Option<Measure> {
        Some(match instrument {
            Inst::Call => Measure::Linear,
            Inst::Perp => Measure::Inverse,
        })
    }

    /// A book and the one mark it has: the perp's.
    struct Marked {
        book: Book,
        perp: Px,
    }

    impl Marked {
        fn apply(&mut self, fills: &[Fill]) {
            self.book.apply(fills);
        }
    }

    impl super::Fold<Inst> for Marked {
        fn positions(&self) -> &Book {
            &self.book
        }

        fn rebase(&mut self, _now: NanoTime, instrument: Inst, net: Qty) {
            let mark = (instrument == Inst::Perp).then_some(self.perp);
            self.book.rebase(instrument, net, mark);
        }
    }

    fn qty(s: &str) -> Qty {
        Qty::parse(s).unwrap()
    }

    fn at(secs: u64) -> NanoTime {
        NanoTime::from(1_000 * SEC + secs * SEC)
    }

    fn perp() -> Inst {
        Inst::Perp
    }

    fn call() -> Inst {
        Inst::Call
    }

    fn reconciler() -> Reconciler {
        Reconciler::new(Config {
            grace: std::time::Duration::from_secs(5),
        })
    }

    /// A book with the perp marked, so a re-base on it is valued.
    fn book() -> Marked {
        Marked {
            book: Book::new(measure),
            perp: Px::parse("60000").unwrap(),
        }
    }

    fn fill(instrument: Inst, side: Side, size: &str) -> Fill {
        Fill {
            order: ClientOrderId(1),
            exec_id: ExecId::new("e1").unwrap(),
            instrument,
            side,
            qty: qty(size),
            filled: qty(size),
            remaining: Qty::ZERO,
            price: Px::parse("60000").unwrap(),
            fee: Qty::ZERO,
            liquidity: Liquidity::Maker,
            venue_time: None,
            recv_time: at(0),
        }
    }

    fn holdings(whole: bool, held: &[(Inst, &str)], as_of: NanoTime) -> Holdings {
        Holdings {
            whole,
            held: held
                .iter()
                .map(|(instrument, net)| Held {
                    instrument: *instrument,
                    net: qty(net),
                })
                .collect::<Burst<Held<Inst>>>(),
            as_of,
        }
    }

    /// A fold the venue has said nothing about yet — one restored across a
    /// restart, before the connect's snapshot — is not judged: no
    /// difference is seen and no grace starts, however long it waits. The
    /// first holdings are what it is held to, and a snapshot that agrees
    /// leaves the cost alone.
    #[test]
    fn a_fold_is_not_judged_before_the_venue_has_spoken() {
        let mut reconciler = reconciler();
        let mut greeks = book();
        greeks.apply(&[fill(perp(), Side::Bid, "1000")]);
        assert!(!reconciler.has_heard());
        assert!(reconciler.reconcile(at(0), &mut greeks).is_empty());
        assert!(reconciler.reconcile(at(60), &mut greeks).is_empty());
        assert_eq!((reconciler.disagreeing(), reconciler.rebased()), (0, 0));
        assert_eq!(greeks.positions().net(&perp()), qty("1000"), "left alone");

        reconciler.heard(&holdings(true, &[(perp(), "1000")], at(61)));
        assert!(reconciler.has_heard());
        assert!(reconciler.reconcile(at(61), &mut greeks).is_empty());
        assert_eq!(reconciler.disagreeing(), 0, "the snapshot agrees");

        // A snapshot that does not is a difference from *its* arrival.
        reconciler.heard(&holdings(true, &[], at(62)));
        assert!(reconciler.reconcile(at(62), &mut greeks).is_empty());
        assert_eq!(reconciler.disagreeing(), 1);
        let found = reconciler.reconcile(at(67), &mut greeks);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].since, at(62));
    }

    /// The ordinary case: the venue states the position the fill left, and
    /// the fold agrees. Nothing to do, and nothing held.
    #[test]
    fn a_fold_that_agrees_is_left_alone() {
        let mut reconciler = reconciler();
        let mut greeks = book();
        greeks.apply(&[fill(perp(), Side::Bid, "1000")]);
        reconciler.heard(&holdings(false, &[(perp(), "1000")], at(0)));
        assert!(reconciler.reconcile(at(0), &mut greeks).is_empty());
        assert_eq!(reconciler.disagreeing(), 0);
        assert!(reconciler.reconcile(at(60), &mut greeks).is_empty());
    }

    /// A difference inside the grace is the order two messages arrived in,
    /// not a disagreement: it is held, and forgotten when the fold catches
    /// up.
    #[test]
    fn a_difference_inside_the_grace_is_not_a_mismatch() {
        let mut reconciler = reconciler();
        let mut greeks = book();
        reconciler.heard(&holdings(false, &[(perp(), "1000")], at(0)));
        assert!(reconciler.reconcile(at(0), &mut greeks).is_empty());
        assert_eq!(reconciler.disagreeing(), 1);
        assert!(reconciler.reconcile(at(4), &mut greeks).is_empty());

        greeks.apply(&[fill(perp(), Side::Bid, "1000")]);
        assert!(reconciler.reconcile(at(4), &mut greeks).is_empty());
        assert_eq!(reconciler.disagreeing(), 0, "caught up");
        assert_eq!(reconciler.rebased(), 0);
    }

    /// Past the grace the fold is re-based on the venue, whichever side was
    /// bigger — a fill the fold never heard, and one it heard that the venue
    /// does not hold.
    #[test]
    fn past_the_grace_the_fold_is_re_based_on_the_venue() {
        let mut reconciler = reconciler();
        let mut greeks = book();
        // A position the account held before this process started.
        reconciler.heard(&holdings(true, &[(perp(), "-3000")], at(0)));
        assert!(reconciler.reconcile(at(0), &mut greeks).is_empty());
        let found = reconciler.reconcile(at(5), &mut greeks);
        assert_eq!(
            found,
            [Mismatch {
                instrument: perp(),
                ours: Qty::ZERO,
                venue: qty("-3000"),
                since: at(0),
            }]
        );
        assert_eq!(greeks.positions().net(&perp()), qty("-3000"));
        assert_eq!(greeks.positions().unvalued(), 0, "booked at the mark");
        assert!(
            reconciler.reconcile(at(6), &mut greeks).is_empty(),
            "and it agrees now"
        );
        assert_eq!(reconciler.rebased(), 1);
    }

    /// Mismatches come out in instrument order, each once, whichever side
    /// holds the instrument — so a replay re-bases in the order the run did.
    /// The call is only the fold's and the perp only the venue's, and the
    /// perp was seen disagreeing first.
    #[test]
    fn mismatches_come_out_in_instrument_order_once_each() {
        let mut reconciler = reconciler();
        let mut greeks = book();
        reconciler.heard(&holdings(true, &[(perp(), "-3000")], at(0)));
        assert!(reconciler.reconcile(at(0), &mut greeks).is_empty());
        greeks.apply(&[fill(call(), Side::Bid, "10")]);
        assert!(reconciler.reconcile(at(1), &mut greeks).is_empty());
        let found: Vec<Inst> = reconciler
            .reconcile(at(6), &mut greeks)
            .iter()
            .map(|mismatch| mismatch.instrument)
            .collect();
        assert_eq!(found, [call(), perp()]);
    }

    /// The merge the walk is built on: two ordered walks with no repeats,
    /// overlapping, give one ordered walk with no repeats.
    #[test]
    fn the_union_of_two_ordered_walks_is_ordered_without_repeats() {
        let merged: Vec<u32> = union([1, 3, 5].into_iter(), [2, 3, 6, 7].into_iter()).collect();
        assert_eq!(merged, [1, 2, 3, 5, 6, 7]);
        assert_eq!(
            union([4].into_iter(), [].into_iter()).collect::<Vec<u32>>(),
            [4]
        );
        assert_eq!(
            union([].into_iter(), [4].into_iter()).collect::<Vec<u32>>(),
            [4]
        );
    }

    /// `Duration::MAX` is "never a mismatch", and means it: the grace is
    /// judged as time elapsed, so it does not overflow `since + grace`.
    #[test]
    fn a_grace_of_duration_max_is_never_a_mismatch() {
        let mut reconciler = Reconciler::new(Config {
            grace: std::time::Duration::MAX,
        });
        let mut greeks = book();
        reconciler.heard(&holdings(true, &[(perp(), "-3000")], at(0)));
        assert!(reconciler.reconcile(at(0), &mut greeks).is_empty());
        assert!(reconciler.reconcile(NanoTime::MAX, &mut greeks).is_empty());
        assert_eq!((reconciler.disagreeing(), reconciler.rebased()), (1, 0));
        assert_eq!(greeks.positions().net(&perp()), Qty::ZERO, "left alone");
    }

    /// A snapshot is the whole book: what it does not name is flat, so a
    /// position the fold holds and the venue no longer does is re-based to
    /// nothing.
    #[test]
    fn a_snapshot_flattens_what_it_does_not_name() {
        let mut reconciler = reconciler();
        let mut greeks = book();
        greeks.apply(&[fill(perp(), Side::Bid, "1000")]);
        reconciler.heard(&holdings(false, &[(perp(), "1000")], at(0)));
        // The session drops; the new one's snapshot lists nothing.
        reconciler.heard(&holdings(true, &[], at(1)));
        assert_eq!(reconciler.venue(&perp()), Qty::ZERO);
        reconciler.reconcile(at(1), &mut greeks);
        let found = reconciler.reconcile(at(6), &mut greeks);
        assert_eq!(found.len(), 1);
        assert!(greeks.positions().position(&perp()).unwrap().is_flat());

        // An update names only what changed.
        reconciler.heard(&holdings(false, &[(perp(), "10")], at(7)));
        reconciler.heard(&holdings(false, &[(call(), "-1")], at(7)));
        assert_eq!(reconciler.venue(&perp()), qty("10"));
        assert_eq!(reconciler.venue(&call()), qty("-1"));
    }

    /// A settlement is the venue saying the position is over, whether or not
    /// it restates the contract: the venue's side is flat in it.
    #[test]
    fn a_settlement_ends_the_venues_position_too() {
        let mut reconciler = reconciler();
        let mut greeks = book();
        let short = fill(call(), Side::Ask, "1");
        greeks.apply(&[short]);
        reconciler.heard(&holdings(false, &[(call(), "-1")], at(0)));
        let delivery = fill(call(), Side::Bid, "1");
        greeks.apply(&[delivery]);
        reconciler.settled(&[delivery]);
        assert!(reconciler.reconcile(at(0), &mut greeks).is_empty());
        assert!(reconciler.reconcile(at(60), &mut greeks).is_empty());
        assert_eq!(reconciler.disagreeing(), 0);
    }

    /// A difference that closes and comes back is a new one, with a new
    /// grace — not the old one's clock.
    #[test]
    fn a_difference_that_comes_back_starts_again() {
        let mut reconciler = reconciler();
        let mut greeks = book();
        reconciler.heard(&holdings(false, &[(perp(), "10")], at(0)));
        reconciler.reconcile(at(0), &mut greeks);
        reconciler.heard(&holdings(false, &[(perp(), "0")], at(3)));
        reconciler.reconcile(at(3), &mut greeks);
        reconciler.heard(&holdings(false, &[(perp(), "10")], at(4)));
        assert!(reconciler.reconcile(at(4), &mut greeks).is_empty());
        assert!(
            reconciler.reconcile(at(6), &mut greeks).is_empty(),
            "five seconds from the first sighting, two from this one"
        );
        assert_eq!(reconciler.reconcile(at(9), &mut greeks).len(), 1);
    }
}
