#![allow(unused)]
use std::time::Duration;
use wingfoil::prelude::*;

wingfoil::nitro! {
    fn bad(g: &GraphBuilder) {
        let out = g.ticker(Duration::from_nanos(10)).count();
    }
}

fn main() {}
