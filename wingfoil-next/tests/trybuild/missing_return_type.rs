#![allow(unused)]
use std::time::Duration;
use wingfoil_next::prelude::*;

wingfoil_next::graph! {
    fn bad(g: &GraphBuilder) {
        let out = g.ticker(Duration::from_nanos(10)).count();
    }
}

fn main() {}
