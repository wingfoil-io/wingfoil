// Benchmark graphs, shared by `build.rs` (via `include!`, to generate the
// static runners for each engine tier at build time) and `benches/tiers.rs`
// (as a module, to wire the same graphs at runtime).
//
// Note: plain `//` comments only — `build.rs` includes this file at its top
// level, where `//!` inner doc comments would not parse.

use std::rc::Rc;
use std::time::Duration;
use wingfoil::*;

/// The mixed odds/evens graph from the wingfoil docs (12 nodes: ticker,
/// count, maps, filters, merge, accumulate) — a realistic small pipeline
/// with String payloads.
pub fn wire_odds_evens() -> (Vec<Rc<dyn Node>>, Rc<dyn Stream<Vec<String>>>) {
    let period = Duration::from_millis(10);
    let source = ticker(period).count();
    let is_even = source.map(|i| i.is_multiple_of(2));
    let odds = source.filter(is_even.not()).map(|i| format!("{i} is odd"));
    let evens = source.filter(is_even).map(|i| format!("{i} is even"));
    let acc = merge(vec![odds, evens]).accumulate();
    (vec![acc.clone().as_node()], acc)
}

/// A dense chain: `len` sample nodes all triggered by one ticker, each
/// passively reading its predecessor (u64 payload, no closures). Every node
/// fires on every cycle — a pure measure of per-node dispatch overhead.
/// Total nodes: `len + 2` (ticker + constant + samples).
pub fn wire_chain(len: usize) -> (Vec<Rc<dyn Node>>, Rc<dyn Stream<u64>>) {
    let t = ticker(Duration::from_nanos(100));
    let mut s = constant(1u64).sample(t.clone());
    for _ in 1..len {
        s = s.sample(t.clone());
    }
    (vec![s.clone().as_node()], s)
}

/// A sparse graph: the same `len`-sample chain on a slow ticker, plus an
/// independent fast counter. In historical mode the fast source fires 1000
/// cycles for every chain tick, so almost every cycle leaves the chain
/// quiet — a measure of the quiet-node floor.
pub fn wire_sparse(len: usize) -> Vec<Rc<dyn Node>> {
    let slow = ticker(Duration::from_micros(100));
    let mut s = constant(1u64).sample(slow.clone());
    for _ in 1..len {
        s = s.sample(slow.clone());
    }
    let fast = ticker(Duration::from_nanos(100)).count();
    vec![s.as_node(), fast.as_node()]
}
