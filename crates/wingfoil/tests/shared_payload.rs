//! `Stream::shared` / `unshared` — the `Rc` payload idiom.
//!
//! Pass-through ops republish their input by clone (`Op::Out` is an owned value
//! and the engine owns the slot it lands in), so a heap-carrying payload pays a
//! full copy per hop per tick. `shared()` makes each hop a refcount bump
//! instead. These tests pin that the wrapping is *transparent* — same values,
//! same tick times as the unwrapped graph — and that the copy really is being
//! avoided rather than merely moved.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const STEP: Duration = Duration::from_nanos(100);

/// A payload that counts its own clones, so "did the hop copy?" is observable
/// rather than inferred from a timing.
#[derive(Default, Debug)]
struct Counted {
    id: u64,
    clones: Rc<RefCell<usize>>,
}

impl Clone for Counted {
    fn clone(&self) -> Self {
        *self.clones.borrow_mut() += 1;
        Self {
            id: self.id,
            clones: self.clones.clone(),
        }
    }
}

/// The headline: a chain of pass-through hops over a `shared()` payload deep-
/// copies the value **once**, not once per hop.
#[test]
fn sharing_collapses_the_per_hop_clone_to_one() {
    fn deep_clones(share: bool) -> usize {
        let clones = Rc::new(RefCell::new(0usize));
        let counter = clones.clone();

        let g = GraphBuilder::new();
        let keep = g.ticker(STEP).count().map(|_: &u64| true);
        let src = g.ticker(STEP).count().map(move |n: &u64| Counted {
            id: *n,
            clones: counter.clone(),
        });

        // Six forwarding hops, the shape a real graph reaches quickly.
        let last_id = if share {
            let mut cur = src.shared();
            for _ in 0..6 {
                cur = cur.filter(&keep);
            }
            cur.map(|c: &Rc<Counted>| c.id).accumulate()
        } else {
            let mut cur = src;
            for _ in 0..6 {
                cur = cur.filter(&keep);
            }
            cur.map(|c: &Counted| c.id).accumulate()
        };

        let mut r = g.build();
        r.run(HISTORICAL, RunFor::Cycles(1)).expect("run completes");
        // Both shapes must agree on what came out.
        assert_eq!(vec![1u64], r.value(&last_id));
        *clones.borrow()
    }

    let owned = deep_clones(false);
    let shared = deep_clones(true);

    assert!(
        owned >= 6,
        "expected at least one deep clone per hop, got {owned}"
    );
    assert_eq!(
        1, shared,
        "sharing should deep-copy once at the boundary and never again \
         (got {shared} against {owned} unshared)"
    );
}

/// Wrapping is transparent: identical values at identical tick times.
#[test]
fn a_shared_graph_produces_the_same_values_and_tick_times() {
    fn run(share: bool) -> Vec<(NanoTime, usize)> {
        let g = GraphBuilder::new();
        let even = g.ticker(STEP).count().map(|n: &u64| n.is_multiple_of(2));
        let src = g.ticker(STEP).count().map(|n: &u64| vec![*n; 4]);

        let out = if share {
            src.shared()
                .filter(&even)
                .delay(STEP)
                .map(|v: &Rc<Vec<u64>>| v.len() + v[0] as usize)
                .with_time()
                .accumulate()
        } else {
            src.filter(&even)
                .delay(STEP)
                .map(|v: &Vec<u64>| v.len() + v[0] as usize)
                .with_time()
                .accumulate()
        };

        let mut r = g.build();
        r.run(HISTORICAL, RunFor::Cycles(8)).expect("run completes");
        r.value(&out)
    }

    let owned = run(false);
    let shared = run(true);
    assert!(!owned.is_empty(), "the fixture produced nothing to compare");
    assert_eq!(
        owned, shared,
        "sharing changed the values or their tick times"
    );
}

/// `unshared` is the inverse and costs exactly one copy.
#[test]
fn unshared_recovers_the_owned_value() {
    let clones = Rc::new(RefCell::new(0usize));
    let counter = clones.clone();

    let g = GraphBuilder::new();
    let keep = g.ticker(STEP).count().map(|_: &u64| true);
    let out = g
        .ticker(STEP)
        .count()
        .map(move |n: &u64| Counted {
            id: *n,
            clones: counter.clone(),
        })
        .shared()
        .filter(&keep)
        .filter(&keep)
        .unshared()
        .map(|c: &Counted| c.id)
        .accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(1)).expect("run completes");

    assert_eq!(vec![1u64], r.value(&out));
    // One at `shared`, one at `unshared`; the two `filter` hops in between are
    // refcount bumps. (The trailing `map` reads by reference.)
    assert_eq!(2, *clones.borrow());
}

/// A shared value fans out to several branches, each of which is still just a
/// refcount bump — the case that motivates it most, since fan-out is where a
/// clone-per-hop cost multiplies.
#[test]
fn fan_out_over_a_shared_payload_stays_copy_free() {
    let clones = Rc::new(RefCell::new(0usize));
    let counter = clones.clone();

    let g = GraphBuilder::new();
    let keep = g.ticker(STEP).count().map(|_: &u64| true);
    let shared = g
        .ticker(STEP)
        .count()
        .map(move |n: &u64| Counted {
            id: *n,
            clones: counter.clone(),
        })
        .shared();

    let a = shared
        .filter(&keep)
        .map(|c: &Rc<Counted>| c.id)
        .accumulate();
    let b = shared
        .filter(&keep)
        .map(|c: &Rc<Counted>| c.id)
        .accumulate();
    let c = shared
        .filter(&keep)
        .map(|c: &Rc<Counted>| c.id)
        .accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(2)).expect("run completes");

    assert_eq!(vec![1u64, 2], r.value(&a));
    assert_eq!(vec![1u64, 2], r.value(&b));
    assert_eq!(vec![1u64, 2], r.value(&c));
    // Two ticks, one deep copy each at the `shared` boundary — the three
    // branches added none.
    assert_eq!(2, *clones.borrow());
}
