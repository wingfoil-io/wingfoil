// `not` is sugar over `map`, so it has a fluent method but deliberately no
// `nitro!` forwarders. Before the `fluent_only_advice` check, this produced
// three `cannot find value __WF_OP_NOT_*` errors — internal symbols, each with
// a nonsense "similarly named constant" suggestion — and, because `not` *does*
// exist on `Stream`, no `no method named` error to explain any of it. It must
// now be one message naming the primitive to spell instead.
#![allow(unused)]
use std::time::Duration;
use wingfoil::prelude::*;

wingfoil::nitro! {
    fn sugar(g: &GraphBuilder) -> Stream<bool> {
        let count = g.ticker(Duration::from_nanos(10)).count();
        let flag = count.map(|i| i.is_multiple_of(2));
        let out = flag.not();
        out
    }
}

fn main() {}
