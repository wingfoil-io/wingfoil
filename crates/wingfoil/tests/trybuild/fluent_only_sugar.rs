// `collapse_accumulate` and `filter_none` are inherent one-liners over `fold`
// and `map_filter` — both already ops — so neither has forwarders of its own.
// Like `split`, they resolve fine fluently, which is what makes the untreated
// failure so poor: the expansion dies on `__WF_OP_*` internals with no `no
// method named` error to explain them. Each must name the primitive to spell
// instead. See `fluent_only_split.rs` for the same shape on `split`.
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

fn main() {}
