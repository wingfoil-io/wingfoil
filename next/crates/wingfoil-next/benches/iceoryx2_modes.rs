//! Benchmarks for iceoryx2 adapter modes (Spin vs Threaded vs Signaled)
//!
//! Port of classic `wingfoil/benches/iceoryx2_modes.rs`. The workload is
//! unchanged: for each mode, wire a `Local`-variant subscriber whose bursts are
//! collapsed and collected, publish a one-element burst from a 100µs ticker into
//! the same service, and run the graph realtime for 100 cycles — graph
//! construction included in the timed body, exactly as classic times it.
//!
//! Deliberate deviations from the classic source, all mechanical (same node
//! count, same shape):
//!
//! - nodes are wired through a [`GraphBuilder`] rather than free functions over
//!   `Rc<dyn Node>`, and the run goes through `GraphBuilder::build()` +
//!   `Runner::run` rather than `Graph::new(vec![...]).run()`;
//! - `iceoryx2_sub_opts` takes the run mode and returns `Result` (next rejects a
//!   historical subscription at wiring), so the call is `?`-propagated;
//! - the publisher is the `Iceoryx2SinkOps::iceoryx2_pub_with` extension method
//!   on the burst stream instead of the free `iceoryx2_pub_with(upstream, ..)`;
//! - classic's `ticker(..).produce(closure)` is `ticker(..).map(closure)` — one
//!   node either way, producing the identical single-element burst;
//! - `collapse().collect()` is `collapse().accumulate()` (next's name for the
//!   same two-node accumulate).
//!
//! Run with: cargo bench -p wingfoil-next --features iceoryx2 -- iceoryx2_modes

use criterion::{Criterion, criterion_group, criterion_main};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use iceoryx2::prelude::ZeroCopySend;
use wingfoil_next::{RunFor, RunMode};
use wingfoil_next::adapters::iceoryx2::{
    Iceoryx2Mode, Iceoryx2ServiceVariant, Iceoryx2SinkOps, Iceoryx2SubOpts, iceoryx2_sub_opts,
};
use wingfoil_next::prelude::*;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend)]
struct BenchData {
    value: u64,
    timestamp: u64,
}

fn bench_mode(c: &mut Criterion, mode: Iceoryx2Mode, mode_name: &str) {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let service_name = format!("wingfoil/bench/{}/{}", mode_name, n);

    c.bench_function(&format!("iceoryx2_{}", mode_name), |b| {
        b.iter(|| {
            let g = GraphBuilder::new();

            let opts = Iceoryx2SubOpts {
                variant: Iceoryx2ServiceVariant::Local,
                mode,
                ..Default::default()
            };
            let sub = iceoryx2_sub_opts::<BenchData>(&g, RunMode::RealTime, &service_name, opts)
                .expect("invariant: realtime subscription is accepted at wiring");
            let _collected = sub.collapse::<BenchData>().accumulate();

            let _publisher = g
                .ticker(Duration::from_micros(100))
                .map(|_: &()| {
                    burst![BenchData {
                        value: 42,
                        timestamp: 0,
                    }]
                })
                .iceoryx2_pub_with(&service_name, Iceoryx2ServiceVariant::Local);

            let mut runner = g.build();
            runner.run(RunMode::RealTime, RunFor::Cycles(100)).unwrap();
        })
    });
}

fn iceoryx2_modes_benchmark(c: &mut Criterion) {
    bench_mode(c, Iceoryx2Mode::Spin, "spin");
    bench_mode(c, Iceoryx2Mode::Threaded, "threaded");
    bench_mode(c, Iceoryx2Mode::Signaled, "signaled");
}

criterion_group!(benches, iceoryx2_modes_benchmark);
criterion_main!(benches);
