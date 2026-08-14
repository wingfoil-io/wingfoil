// `Stream`'s inherent method names cannot also identify `nitro!` ops. The
// ordinary fluent `wire()` function would resolve the inherent method while
// compiled emission resolved a same-named forwarder, giving the two tiers
// different meanings. Each collision must produce one curated diagnostic at
// the call site rather than leaking generated `__WF_OP_*` errors.
#![allow(unused)]
use std::time::Duration;
use wingfoil::prelude::*;

wingfoil::nitro! {
    fn cloning(g: &GraphBuilder) -> Stream<u64> {
        let out = g.ticker(Duration::from_nanos(10)).clone();
        out
    }
}

wingfoil::nitro! {
    fn taking_handle(g: &GraphBuilder) -> Stream<u64> {
        let out = g.ticker(Duration::from_nanos(10)).handle();
        out
    }
}

wingfoil::nitro! {
    fn raw_wiring(g: &GraphBuilder) -> Stream<u64> {
        let out = g
            .ticker(Duration::from_nanos(10))
            .wire(|b, h| b.map(h, |value| *value));
        out
    }
}

fn main() {}
