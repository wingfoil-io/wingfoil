// `collapse_accumulate`, `filter_none` and `filter_map` are one-liners over
// `fold` and `map_filter` — both already ops — so none has forwarders of its
// own. Like `split`, they resolve fine fluently, which is what makes the
// untreated failure so poor: the expansion dies on `__WF_OP_*` internals with
// no `no method named` error to explain them. Each must name the primitive to
// spell instead. See `fluent_only_split.rs` for the same shape on `split`.
//
// `filter_map` is the one of the three that is *not* on a specialised
// `Stream<..>` — it is a `StreamOps` method, so it is in reach of every
// `nitro!` block, which is exactly why it needs its own arm here.
#![allow(unused)]
use std::time::Duration;
use wingfoil::prelude::*;

wingfoil::nitro! {
    fn collapsing(g: &GraphBuilder, bursts: &Stream<Burst<u64>>) -> Stream<Vec<u64>> {
        let out = bursts.collapse_accumulate();
        out
    }
}

wingfoil::nitro! {
    fn dropping_none(g: &GraphBuilder, opts: &Stream<Option<u64>>) -> Stream<u64> {
        let out = opts.filter_none();
        out
    }
}

wingfoil::nitro! {
    fn mapping_and_filtering(g: &GraphBuilder, counts: &Stream<u64>) -> Stream<u64> {
        let out = counts.filter_map(|n: &u64| (n % 2 == 1).then(|| n * n));
        out
    }
}

fn main() {}
