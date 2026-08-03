#![allow(unused)]
use std::time::Duration;
use wingfoil::prelude::*;

wingfoil::nitro! {
    fn bad(g: &GraphBuilder, src: Stream<u64>) -> Stream<u64> {
        let out = src.map(|i| i + 1);
        out
    }
}

fn main() {}
