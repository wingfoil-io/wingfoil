// `Stream`'s inherent method names cannot also identify `nitro!` ops. The
// ordinary fluent `wire()` function would resolve the inherent method while
// compiled emission resolved a same-named forwarder, giving the two tiers
// different meanings. Each collision must produce one curated diagnostic at
// the call site rather than leaking generated `__WF_OP_*` errors.
//
// One block per public method of `impl<T> Stream<T>` in `fluent.rs`, plus
// `clone`. The set shipped as `clone` / `handle` / `wire` alone, which left the
// other four leaking `__WF_OP_*`; these blocks are what keeps the two in step.
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
    fn reaching_the_builder(g: &GraphBuilder) -> Stream<u64> {
        let out = g.ticker(Duration::from_nanos(10)).graph();
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

wingfoil::nitro! {
    fn taking_slot(g: &GraphBuilder) -> Stream<u64> {
        let out = g.ticker(Duration::from_nanos(10)).value_slot();
        out
    }
}

// The likeliest of the seven to be typed by habit: `Stream::build` is the
// documented way to close a *fluent* chain (`.print().build().run(..)`).
wingfoil::nitro! {
    fn closing_the_chain(g: &GraphBuilder) -> Stream<u64> {
        let out = g.ticker(Duration::from_nanos(10)).build();
        out
    }
}

wingfoil::nitro! {
    fn erasing_to_an_edge(g: &GraphBuilder) -> Stream<u64> {
        let out = g.ticker(Duration::from_nanos(10)).upstream();
        out
    }
}

fn main() {}
