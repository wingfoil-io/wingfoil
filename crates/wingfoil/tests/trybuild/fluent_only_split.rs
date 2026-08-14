// `split` cannot become an op: it yields two output streams where an `Op` has
// one `Out`, and `nitro!`'s grammar binds one name to one node. Without the
// `non_op_method_advice` check it produced two or three `cannot find value
// __WF_OP_SPLIT_*` errors — internal symbols, each with a nonsense "similarly
// named constant" suggestion — and, because `split` *does* exist on `Stream`,
// no `no method named` error to explain them. It must be one message naming
// what to write instead.
//
// Contrast `not` and `collapse`, which were rejected here until they became
// real ops (`ops::Not` / `ops::Collapse`); they now work in `nitro!` outright
// and are exercised in `op_completeness.rs`.
#![allow(unused)]
use std::time::Duration;
use wingfoil::prelude::*;

wingfoil::nitro! {
    fn splitting(g: &GraphBuilder) -> Stream<u64> {
        let count = g.ticker(Duration::from_nanos(10)).count();
        let pairs = count.map(|i| (*i, *i * 2));
        let halves = pairs.split();
        count
    }
}

fn main() {}
