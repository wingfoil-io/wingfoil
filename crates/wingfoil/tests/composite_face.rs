//! **Composite faces from user position**: fluent methods that wire several
//! nodes, written *in this test crate* and called verbatim inside `nitro!`.
//!
//! A composite is a definition in `nitro!`'s grammar, carried by a
//! hand-written `macro_rules!` shell that `nitro!` calls back for the body
//! (see `wingfoil_derive`'s `composite` module). The execution layer's
//! faces — `algo`, `oms`, `fix_bridge`, `reconcile` — are pinned in
//! `op_completeness.rs`; this file pins the inlining rules on small graphs
//! whose values are easy to read off:
//!
//! - [`halves`] — two outputs, and a **plain** argument (`k`) that a closure
//!   in the body captures: it is evaluated **once** per call, where the call
//!   stands, as the fluent call evaluates it;
//! - [`scaled`] — a **literal-closure** argument, substituted where the body
//!   uses it so the op still sees a literal closure; one output, bound to a
//!   one-tuple;
//! - [`quarters`] — a composite whose body calls another composite;
//! - [`counted`] — a face on the **builder**, its body rooted at it;
//!
//! and, in the graphs, the same composite called twice (each call's nodes
//! named apart) and an output bound to `_`.
//!
//! An integration test is its own crate, so every shell here is a genuine
//! out-of-crate one: `::wingfoil::composite!` rather than `$crate::`, and the
//! `pub use crate::__wf_composite_..` re-export that lets a glob import of
//! [`faces`] bring the shells with the trait.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const P: Duration = Duration::from_millis(10);

/// How many times a composite's plain argument has been evaluated.
static EVALS: AtomicUsize = AtomicUsize::new(0);

/// A threshold, counting each evaluation.
fn threshold(k: u64) -> u64 {
    EVALS.fetch_add(1, Ordering::SeqCst);
    k
}

pub mod faces {
    use std::time::Duration;

    use wingfoil::prelude::*;

    /// The faces, as a library would ship them: a trait whose methods call
    /// the composites' `define`d functions.
    pub trait Faces {
        fn halves(&self, k: u64) -> (Stream<u64>, Stream<u64>);
        fn scaled<F: Fn(&u64) -> u64 + 'static>(&self, f: F) -> (Stream<u64>,);
        fn quarters(&self, k: u64) -> (Stream<u64>, Stream<u64>, Stream<u64>);
    }

    impl Faces for Stream<u64> {
        fn halves(&self, k: u64) -> (Stream<u64>, Stream<u64>) {
            halves(self, k)
        }
        fn scaled<F: Fn(&u64) -> u64 + 'static>(&self, f: F) -> (Stream<u64>,) {
            scaled(self, f)
        }
        fn quarters(&self, k: u64) -> (Stream<u64>, Stream<u64>, Stream<u64>) {
            quarters(self, k)
        }
    }

    pub trait BuilderFaces {
        fn counted(&self, period: Duration) -> (Stream<u64>, Stream<u64>);
    }

    impl BuilderFaces for GraphBuilder {
        fn counted(&self, period: Duration) -> (Stream<u64>, Stream<u64>) {
            counted(self, period)
        }
    }

    #[macro_export]
    macro_rules! __wf_composite_halves {
        ($($cb:tt)*) => {
            ::wingfoil::composite! { [$($cb)*]
                /// The values below `k`, and the rest.
                pub fn halves(src: &Stream<u64>, k: u64) -> (Stream<u64>, Stream<u64>) {
                    let low = src.map_filter(move |v: &u64| (*v, *v < k));
                    let high = src.map_filter(move |v: &u64| (*v, *v >= k));
                    (low, high)
                }
            }
        };
    }
    pub use crate::__wf_composite_halves;
    __wf_composite_halves!(define);

    #[macro_export]
    macro_rules! __wf_composite_scaled {
        ($($cb:tt)*) => {
            ::wingfoil::composite! { [$($cb)*]
                /// `f` of every value.
                pub fn scaled<F: Fn(&u64) -> u64 + 'static>(src: &Stream<u64>, f: F) -> (Stream<u64>,) {
                    let out = src.map(f);
                    (out,)
                }
            }
        };
    }
    pub use crate::__wf_composite_scaled;
    __wf_composite_scaled!(define);

    #[macro_export]
    macro_rules! __wf_composite_quarters {
        ($($cb:tt)*) => {
            ::wingfoil::composite! { [$($cb)*]
                /// [`halves`], the low half doubled, and the two summed.
                pub fn quarters(src: &Stream<u64>, k: u64) -> (Stream<u64>, Stream<u64>, Stream<u64>) {
                    let (low, high) = src.halves(k);
                    let doubled = low.map(|v: &u64| v * 2);
                    let sum = doubled.join(&high, |a: &u64, b: &u64| a + b);
                    (doubled, high, sum)
                }
            }
        };
    }
    pub use crate::__wf_composite_quarters;
    __wf_composite_quarters!(define);

    #[macro_export]
    macro_rules! __wf_composite_counted {
        ($($cb:tt)*) => {
            ::wingfoil::composite! { [$($cb)*]
                /// A count on `period`, and its even values.
                pub fn counted(g: &GraphBuilder, period: Duration) -> (Stream<u64>, Stream<u64>) {
                    let count = g.ticker(period).count();
                    let even = count.map_filter(|c: &u64| (*c, c % 2 == 0));
                    (count, even)
                }
            }
        };
    }
    pub use crate::__wf_composite_counted;
    __wf_composite_counted!(define);
}

use faces::*;

// `halves` twice on one stream, with a different `k` each time, so the two
// calls' nodes must be named apart; `scaled` with a literal closure, bound to
// a one-tuple; the builder face's even half bound to `_`.
wingfoil::nitro! {
    fn split_twice(g: &GraphBuilder) -> Stream<Vec<(NanoTime, (u64, u64, u64))>> {
        let (count, _) = g.counted(P);
        let (low, _) = count.halves(threshold(3));
        let (_, high) = count.halves(threshold(5));
        let (tripled,) = count.scaled(|v: &u64| v * 3);
        let all = low.join3(&high, &tripled, |l: &u64, h: &u64, t: &u64| (*l, *h, *t));
        let out = all.with_time().accumulate();
        out
    }
}

/// The count runs 1..=6. `low` holds below 3 (1, 2, then held at 2),
/// `high` is 0 until the count reaches 5, and `tripled` follows every tick.
#[test]
fn a_composite_called_twice_is_two_sets_of_nodes() {
    let at = |n: u64| NanoTime::new(n * 10_000_000);
    let expected = vec![
        (at(0), (1, 0, 3)),
        (at(1), (2, 0, 6)),
        (at(2), (2, 0, 9)),
        (at(3), (2, 0, 12)),
        (at(4), (2, 5, 15)),
        (at(5), (2, 6, 18)),
    ];
    let run = RunFor::Cycles(6);

    let before = EVALS.load(Ordering::SeqCst);
    let (mut runner, out) = split_twice::interpreted();
    runner.run(HISTORICAL, run).unwrap();
    assert_eq!(runner.value(out), expected);
    // The fluent call evaluates each argument once.
    assert_eq!(EVALS.load(Ordering::SeqCst) - before, 2);

    let before = EVALS.load(Ordering::SeqCst);
    let (compiled,) = split_twice::compiled(HISTORICAL, run).unwrap();
    assert_eq!(compiled, expected);
    // So does the inlined one: the argument is bound once, where the call
    // stands, never re-evaluated per use or per cycle.
    assert_eq!(EVALS.load(Ordering::SeqCst) - before, 2);

    let g = GraphBuilder::new();
    let island = split_twice::nested(&g);
    let mut r = g.build();
    r.run(HISTORICAL, run).unwrap();
    assert_eq!(r.value(&island), expected);
}

// A composite whose body calls another: `quarters` inlines `halves`.
wingfoil::nitro! {
    fn nested_faces(g: &GraphBuilder) -> Stream<Vec<(NanoTime, (u64, u64, u64))>> {
        let (count, even) = g.counted(P);
        let (doubled, high, sum) = even.quarters(4);
        let all = doubled.join3(&high, &sum, |d: &u64, h: &u64, s: &u64| (*d, *h, *s));
        let out = all.with_time().accumulate();
        out
    }
}

/// The evens are 2, 4, 6 at the 2nd, 4th and 6th ticks. Below 4 only 2,
/// doubled to 4 and held; 4 and 6 are high; the sum adds the two as held.
#[test]
fn a_composite_may_call_a_composite() {
    let at = |n: u64| NanoTime::new(n * 10_000_000);
    let expected = vec![(at(1), (4, 0, 4)), (at(3), (4, 4, 8)), (at(5), (4, 6, 10))];
    let run = RunFor::Cycles(6);

    let (mut runner, out) = nested_faces::interpreted();
    runner.run(HISTORICAL, run).unwrap();
    assert_eq!(runner.value(out), expected);

    let (compiled,) = nested_faces::compiled(HISTORICAL, run).unwrap();
    assert_eq!(compiled, expected);

    let g = GraphBuilder::new();
    let island = nested_faces::nested(&g);
    let mut r = g.build();
    r.run(HISTORICAL, run).unwrap();
    assert_eq!(r.value(&island), expected);
}
