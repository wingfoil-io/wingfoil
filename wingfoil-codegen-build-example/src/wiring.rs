// The single source of truth for the graph. Shared by `build.rs` (via
// `include!`, to generate the static runner at build time) and `main.rs` (as
// a module, to wire the same nodes at runtime). Editing this file triggers
// regeneration on the next build.
//
// Note: plain `//` comments only — `build.rs` includes this file at its top
// level, where `//!` inner doc comments would not parse.

use std::rc::Rc;
use std::time::Duration;
use wingfoil::*;

/// The odds/evens pipeline from the wingfoil docs, with an accumulator so
/// results can be asserted.
pub fn wire() -> (Vec<Rc<dyn Node>>, Rc<dyn Stream<Vec<String>>>) {
    let period = Duration::from_millis(10);
    let source = ticker(period).count();
    let is_even = source.map(|i| i.is_multiple_of(2));
    let odds = source.filter(is_even.not()).map(|i| format!("{i} is odd"));
    let evens = source.filter(is_even).map(|i| format!("{i} is even"));
    let acc = merge(vec![odds, evens]).accumulate();
    (vec![acc.clone().as_node()], acc)
}
