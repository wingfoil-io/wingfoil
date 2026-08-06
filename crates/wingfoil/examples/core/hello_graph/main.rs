//! Smallest wingfoil graph, in the fluent style: a ticker is counted,
//! formatted, and tapped with `logged` — run historically (instant,
//! deterministic) and then in realtime. `logged` emits through the `log`
//! crate, so run with `RUST_LOG=info` to see the output.
//!
//! ```sh
//! RUST_LOG=info cargo run --manifest-path crates/wingfoil/Cargo.toml --example hello_graph
//! ```

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// `ticker → count → format → logged`. `logged` is a pass-through tap, so the
/// same wiring runs unchanged in either mode.
fn wire(g: &GraphBuilder) {
    g.ticker(Duration::from_millis(100))
        .count()
        .map(|i| format!("tick {i}"))
        .logged("tick", log::Level::Info);
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    // Historical: the whole run happens instantly at simulated times.
    let g = GraphBuilder::new();
    wire(&g);
    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))?;

    // Realtime: the same wiring, paced by the wall clock.
    let g = GraphBuilder::new();
    wire(&g);
    g.build().run(RunMode::RealTime, RunFor::Cycles(3))
}
