// A feedback loop closes a cycle, which straight-line compiled emission cannot
// express, so `nitro!` refuses both spellings of one with that advice. Both
// are rooted at the builder — `g.feedback::<T>()` opens a cut and
// `g.feedback_loop(..)` wires a whole loop — and a builder-rooted call goes
// straight to forwarder lookup, so without the check there they would die on
// an unresolved `__WF_OP_*` forwarder with nothing to explain it. The
// stream-side `.feedback(&sink)` is refused the same way in `apply_call`.
#![allow(unused)]
use wingfoil::prelude::*;

wingfoil::nitro! {
    fn opening_a_cut(g: &GraphBuilder) -> Stream<u64> {
        let fed_back = g.feedback::<u64>();
        fed_back
    }
}

wingfoil::nitro! {
    fn wiring_a_loop(g: &GraphBuilder, counts: &Stream<u64>) -> Stream<u64> {
        let sent = g.feedback_loop(|was| was.map(|v: &u64| v + 1));
        sent
    }
}

fn main() {}
