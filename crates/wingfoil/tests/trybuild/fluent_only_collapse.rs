// The same rule for `collapse` (sugar over `map_filter`): one message naming
// the primitive, not a wall of unresolved `__WF_OP_COLLAPSE_*` internals.
#![allow(unused)]
use std::time::Duration;
use wingfoil::prelude::*;

wingfoil::nitro! {
    fn collapsing(g: &GraphBuilder) -> Stream<u64> {
        let count = g.ticker(Duration::from_nanos(10)).count();
        let listed = count.map(|i| vec![*i, *i + 1]);
        let out = listed.collapse();
        out
    }
}

fn main() {}
