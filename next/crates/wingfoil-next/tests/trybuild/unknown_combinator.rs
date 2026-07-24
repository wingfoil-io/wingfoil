// An unknown combinator now falls through to the generic custom-op path: the
// expansion calls the naming-convention forwarders (`__wf_op_<name>_cycle` /
// `__wf_op_<name>_start`), so a typo — or an op that never generated them —
// fails as an unresolved function *spanned at the method call*, in both the
// `wire()` (no fluent method) and `compiled()` (no forwarder) expansions.
#![allow(unused)]
use std::time::Duration;
use wingfoil_next::prelude::*;

wingfoil_next::graph! {
    fn bad(g: &GraphBuilder) -> Stream<u64> {
        let out = g.ticker(Duration::from_nanos(10)).count().frobnicate();
        out
    }
}

fn main() {}
