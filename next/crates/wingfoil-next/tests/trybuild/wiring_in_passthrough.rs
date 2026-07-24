#![allow(unused)]
use std::time::Duration;
use wingfoil_next::prelude::*;

wingfoil_next::graph! {
    fn bad(g: &GraphBuilder) -> Stream<u64> {
        let count = g.ticker(Duration::from_nanos(10)).count();
        // A non-wiring statement that mentions a stream: `count` may only
        // appear in straight-line `let name = <chain>;` wiring.
        let _x = std::convert::identity(&count);
        count
    }
}

fn main() {}
