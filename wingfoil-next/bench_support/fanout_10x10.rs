// Shared `graph!` wiring for the NxN fan-out demo/benchmark, `include!`d
// by `examples/fanout_10x10.rs` and `benches/fanout.rs`.
//
// `graph!` v1 forbids wiring inside loops (the DAG must be static), so all
// 100 `.map()` nodes and the 10-way merge are written out literally — the
// price of a macro that reads source tokens rather than a built graph.
//
// Plain `//` comments only — this file is `include!`d at module scope.

const PERIOD: std::time::Duration = std::time::Duration::from_nanos(100);

wingfoil_next::graph! {
    fn fanout(g: &GraphBuilder) -> Stream<u64> {
        let src = g.ticker(PERIOD).count();
        let c0_0 = src.map(|i| std::hint::black_box(*i));
        let c0_1 = c0_0.map(|i| std::hint::black_box(*i));
        let c0_2 = c0_1.map(|i| std::hint::black_box(*i));
        let c0_3 = c0_2.map(|i| std::hint::black_box(*i));
        let c0_4 = c0_3.map(|i| std::hint::black_box(*i));
        let c0_5 = c0_4.map(|i| std::hint::black_box(*i));
        let c0_6 = c0_5.map(|i| std::hint::black_box(*i));
        let c0_7 = c0_6.map(|i| std::hint::black_box(*i));
        let c0_8 = c0_7.map(|i| std::hint::black_box(*i));
        let c0_9 = c0_8.map(|i| std::hint::black_box(*i));
        let c1_0 = src.map(|i| std::hint::black_box(*i));
        let c1_1 = c1_0.map(|i| std::hint::black_box(*i));
        let c1_2 = c1_1.map(|i| std::hint::black_box(*i));
        let c1_3 = c1_2.map(|i| std::hint::black_box(*i));
        let c1_4 = c1_3.map(|i| std::hint::black_box(*i));
        let c1_5 = c1_4.map(|i| std::hint::black_box(*i));
        let c1_6 = c1_5.map(|i| std::hint::black_box(*i));
        let c1_7 = c1_6.map(|i| std::hint::black_box(*i));
        let c1_8 = c1_7.map(|i| std::hint::black_box(*i));
        let c1_9 = c1_8.map(|i| std::hint::black_box(*i));
        let c2_0 = src.map(|i| std::hint::black_box(*i));
        let c2_1 = c2_0.map(|i| std::hint::black_box(*i));
        let c2_2 = c2_1.map(|i| std::hint::black_box(*i));
        let c2_3 = c2_2.map(|i| std::hint::black_box(*i));
        let c2_4 = c2_3.map(|i| std::hint::black_box(*i));
        let c2_5 = c2_4.map(|i| std::hint::black_box(*i));
        let c2_6 = c2_5.map(|i| std::hint::black_box(*i));
        let c2_7 = c2_6.map(|i| std::hint::black_box(*i));
        let c2_8 = c2_7.map(|i| std::hint::black_box(*i));
        let c2_9 = c2_8.map(|i| std::hint::black_box(*i));
        let c3_0 = src.map(|i| std::hint::black_box(*i));
        let c3_1 = c3_0.map(|i| std::hint::black_box(*i));
        let c3_2 = c3_1.map(|i| std::hint::black_box(*i));
        let c3_3 = c3_2.map(|i| std::hint::black_box(*i));
        let c3_4 = c3_3.map(|i| std::hint::black_box(*i));
        let c3_5 = c3_4.map(|i| std::hint::black_box(*i));
        let c3_6 = c3_5.map(|i| std::hint::black_box(*i));
        let c3_7 = c3_6.map(|i| std::hint::black_box(*i));
        let c3_8 = c3_7.map(|i| std::hint::black_box(*i));
        let c3_9 = c3_8.map(|i| std::hint::black_box(*i));
        let c4_0 = src.map(|i| std::hint::black_box(*i));
        let c4_1 = c4_0.map(|i| std::hint::black_box(*i));
        let c4_2 = c4_1.map(|i| std::hint::black_box(*i));
        let c4_3 = c4_2.map(|i| std::hint::black_box(*i));
        let c4_4 = c4_3.map(|i| std::hint::black_box(*i));
        let c4_5 = c4_4.map(|i| std::hint::black_box(*i));
        let c4_6 = c4_5.map(|i| std::hint::black_box(*i));
        let c4_7 = c4_6.map(|i| std::hint::black_box(*i));
        let c4_8 = c4_7.map(|i| std::hint::black_box(*i));
        let c4_9 = c4_8.map(|i| std::hint::black_box(*i));
        let c5_0 = src.map(|i| std::hint::black_box(*i));
        let c5_1 = c5_0.map(|i| std::hint::black_box(*i));
        let c5_2 = c5_1.map(|i| std::hint::black_box(*i));
        let c5_3 = c5_2.map(|i| std::hint::black_box(*i));
        let c5_4 = c5_3.map(|i| std::hint::black_box(*i));
        let c5_5 = c5_4.map(|i| std::hint::black_box(*i));
        let c5_6 = c5_5.map(|i| std::hint::black_box(*i));
        let c5_7 = c5_6.map(|i| std::hint::black_box(*i));
        let c5_8 = c5_7.map(|i| std::hint::black_box(*i));
        let c5_9 = c5_8.map(|i| std::hint::black_box(*i));
        let c6_0 = src.map(|i| std::hint::black_box(*i));
        let c6_1 = c6_0.map(|i| std::hint::black_box(*i));
        let c6_2 = c6_1.map(|i| std::hint::black_box(*i));
        let c6_3 = c6_2.map(|i| std::hint::black_box(*i));
        let c6_4 = c6_3.map(|i| std::hint::black_box(*i));
        let c6_5 = c6_4.map(|i| std::hint::black_box(*i));
        let c6_6 = c6_5.map(|i| std::hint::black_box(*i));
        let c6_7 = c6_6.map(|i| std::hint::black_box(*i));
        let c6_8 = c6_7.map(|i| std::hint::black_box(*i));
        let c6_9 = c6_8.map(|i| std::hint::black_box(*i));
        let c7_0 = src.map(|i| std::hint::black_box(*i));
        let c7_1 = c7_0.map(|i| std::hint::black_box(*i));
        let c7_2 = c7_1.map(|i| std::hint::black_box(*i));
        let c7_3 = c7_2.map(|i| std::hint::black_box(*i));
        let c7_4 = c7_3.map(|i| std::hint::black_box(*i));
        let c7_5 = c7_4.map(|i| std::hint::black_box(*i));
        let c7_6 = c7_5.map(|i| std::hint::black_box(*i));
        let c7_7 = c7_6.map(|i| std::hint::black_box(*i));
        let c7_8 = c7_7.map(|i| std::hint::black_box(*i));
        let c7_9 = c7_8.map(|i| std::hint::black_box(*i));
        let c8_0 = src.map(|i| std::hint::black_box(*i));
        let c8_1 = c8_0.map(|i| std::hint::black_box(*i));
        let c8_2 = c8_1.map(|i| std::hint::black_box(*i));
        let c8_3 = c8_2.map(|i| std::hint::black_box(*i));
        let c8_4 = c8_3.map(|i| std::hint::black_box(*i));
        let c8_5 = c8_4.map(|i| std::hint::black_box(*i));
        let c8_6 = c8_5.map(|i| std::hint::black_box(*i));
        let c8_7 = c8_6.map(|i| std::hint::black_box(*i));
        let c8_8 = c8_7.map(|i| std::hint::black_box(*i));
        let c8_9 = c8_8.map(|i| std::hint::black_box(*i));
        let c9_0 = src.map(|i| std::hint::black_box(*i));
        let c9_1 = c9_0.map(|i| std::hint::black_box(*i));
        let c9_2 = c9_1.map(|i| std::hint::black_box(*i));
        let c9_3 = c9_2.map(|i| std::hint::black_box(*i));
        let c9_4 = c9_3.map(|i| std::hint::black_box(*i));
        let c9_5 = c9_4.map(|i| std::hint::black_box(*i));
        let c9_6 = c9_5.map(|i| std::hint::black_box(*i));
        let c9_7 = c9_6.map(|i| std::hint::black_box(*i));
        let c9_8 = c9_7.map(|i| std::hint::black_box(*i));
        let c9_9 = c9_8.map(|i| std::hint::black_box(*i));
        let out = c0_9.merge(&c1_9).merge(&c2_9).merge(&c3_9).merge(&c4_9).merge(&c5_9).merge(&c6_9).merge(&c7_9).merge(&c8_9).merge(&c9_9);
        out
    }
}
