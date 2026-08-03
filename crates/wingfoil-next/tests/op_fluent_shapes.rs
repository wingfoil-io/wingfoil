//! `#[op(fluent)]`-generated fluent methods, one test per **receiver shape**.
//!
//! `#[op(build = name, fluent)]` emits `__wf_fluent_<name>!`, a `macro_rules!`
//! that writes the op's fluent method inside whatever extension trait the
//! author picks. Which receiver the method takes is decided by the op's own
//! `In`, not by the author, and there are three:
//!
//! | Receiver | `In` edge 0 | Invocation | In-tree user |
//! |---|---|---|---|
//! | generic | a bare type parameter | `__wf_fluent_map!(T);` | `StreamOps` |
//! | concrete | a fixed type (`&f64`) | `__wf_fluent_rolling_mean!();` | `StatisticsOps` |
//! | source | no edges at all | `__wf_fluent_ticker!();` | `SourceOps` |
//!
//! `crates/wingfoil-next/src/{fluent,stats}.rs` already invoke all three, and
//! the behavioural suites (`catalog*.rs`, `statistics*.rs`) are the oracle for
//! what the methods compute. What *this* file pins is the property those
//! suites cannot see: that each macro is **reusable from an unrelated trait**
//! — the whole reason the method is emitted as a `macro_rules!` rather than as
//! an inherent method on `Stream`. Every trait below is defined here, in an
//! integration test outside the crate, and its bodies come from the generated
//! macro alone; a receiver shape losing its generator is a compile error here.

use std::time::Duration;

use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const P: Duration = Duration::from_nanos(100);

// --- generic receiver -------------------------------------------------------

/// Edge 0 is one of the op's type parameters, so the macro takes that type and
/// the invoking impl binds it — the shape `StreamOps` uses for most of the
/// catalog. `map`'s later parameter (its closure `Cfg`) and its output type
/// come across as the op declares them.
trait MyMapOps<T> {
    fn map<B, F>(&self, f: F) -> Stream<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> B + 'static,
        T: Clone + Default + 'static;
}

impl<T> MyMapOps<T> for Stream<T> {
    wingfoil_next::__wf_fluent_map!(T);
}

#[test]
fn generic_receiver_binds_the_element_type_of_the_invoking_trait() {
    let g = GraphBuilder::new();
    let ticks = SourceOps::ticker(&g, P);
    let doubled = MyMapOps::map(&ticks.count(), |c: &u64| c * 2);
    let out = doubled.with_time().accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(3)).unwrap();

    assert_eq!(
        vec![
            (NanoTime::ZERO, 2),
            (NanoTime::new(100), 4),
            (NanoTime::new(200), 6),
        ],
        runner.value(&out)
    );
}

// --- concrete receiver ------------------------------------------------------

/// Edge 0 is `&f64`, so the receiver is fixed at `Stream<f64>` and the macro
/// takes no argument — the shape `StatisticsOps` uses. Both a `Cfg`-carrying op
/// (`rolling_mean`, `usize`) and a `Cfg = ()` op (`cumulative_sum`) are pinned:
/// the config parameter disappears entirely for the latter.
trait MyStatOps {
    fn rolling_mean(&self, window: usize) -> Stream<f64>;
    fn cumulative_sum(&self) -> Stream<f64>;
}

impl MyStatOps for Stream<f64> {
    wingfoil_next::__wf_fluent_rolling_mean!();
    wingfoil_next::__wf_fluent_cumulative_sum!();
}

#[test]
fn concrete_receiver_needs_no_type_argument() {
    let g = GraphBuilder::new();
    let counts = SourceOps::ticker(&g, P).count();
    let values = StreamOps::map(&counts, |c: &u64| *c as f64);
    let mean = MyStatOps::rolling_mean(&values, 2).accumulate();
    let total = MyStatOps::cumulative_sum(&values).accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(4)).unwrap();

    assert_eq!(vec![1.0, 1.5, 2.5, 3.5], runner.value(&mean));
    assert_eq!(vec![1.0, 3.0, 6.0, 10.0], runner.value(&total));
}

// --- source receiver --------------------------------------------------------

/// A source op has no edges, so there is no receiver stream: the method lands
/// on a `GraphBuilder` extension trait and its body wires through
/// `GraphBuilder::source` instead of `Stream::wire`. `constant` additionally
/// keeps a type parameter of its own (its payload), which is *not* the
/// receiver — it stays a method generic.
trait MySourceOps {
    fn ticker(&self, period: Duration) -> Stream<()>;
    fn constant<T: Clone + Default + 'static>(&self, value: T) -> Stream<T>;
}

impl MySourceOps for GraphBuilder {
    wingfoil_next::__wf_fluent_ticker!();
    wingfoil_next::__wf_fluent_constant!();
}

#[test]
fn source_receiver_wires_through_the_graph_builder() {
    let g = GraphBuilder::new();
    let counted = MySourceOps::ticker(&g, P).count().with_time().accumulate();
    let seven = MySourceOps::constant(&g, 7u64).with_time().accumulate();

    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(3)).unwrap();

    assert_eq!(
        vec![
            (NanoTime::ZERO, 1),
            (NanoTime::new(100), 2),
            (NanoTime::new(200), 3),
        ],
        runner.value(&counted)
    );
    // `constant` ticks once, at the start instant, and stays quiet after.
    assert_eq!(vec![(NanoTime::ZERO, 7)], runner.value(&seven));
}
