// Shared `graph!` wiring for the 10x10 fan-out demo/benchmark, `include!`d by
// `examples/fanout_10x10.rs` and `benches/fanout.rs`.
//
// One `count` source is fanned out into 10 parallel 10-deep identity-`map`
// chains and merged back into one stream. The `fan` / `map_n` combinators are
// bounded compile-time repetition — the macro unrolls them into a static DAG
// (100 map nodes + a 10-way merge), so no `for` loop is needed and nothing is
// spelled out by hand.
//
// Plain `//` comments only — this file is `include!`d at module scope.

const PERIOD: std::time::Duration = std::time::Duration::from_nanos(100);

wingfoil_next::graph! {
    fn fanout(g: &GraphBuilder) -> Stream<u64> {
        let src = g.ticker(PERIOD).count();
        let out = src.fan(10, |s| s.map_n(10, |i| std::hint::black_box(*i)));
        out
    }
}
