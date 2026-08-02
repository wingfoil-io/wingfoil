#![doc = include_str!("./README.md")]
//!
//! ```sh
//! # Default log output (env_logger)
//! RUST_LOG=info cargo run -p wingfoil-next --example tracing
//! RUST_LOG=info cargo run -p wingfoil-next --example tracing -- log
//!
//! # Events via a tracing subscriber
//! RUST_LOG=info cargo run -p wingfoil-next --example tracing --features tracing -- tracing
//!
//! # Events + engine lifecycle and cycle spans
//! RUST_LOG=info cargo run -p wingfoil-next --example tracing \
//!     --features instrument-default -- instruments
//! ```

use std::time::Duration;

use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

/// A one-per-second counter, tapped by `logged` so each tick is emitted as a
/// `log` record (`"{time} tick {value}"`, target `"wingfoil"`, level `info`).
/// `logged` is a pass-through, so the value stream is just the counter.
fn build(g: &GraphBuilder) -> Stream<u64> {
    g.ticker(Duration::from_secs(1))
        .count()
        .logged("tick", log::Level::Info)
}

fn main() -> anyhow::Result<()> {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "log".into())
        .to_lowercase();

    match mode.as_str() {
        "log" => env_logger::init(),
        // Route the graph's events through a `tracing` subscriber instead of
        // `env_logger`. `logged` emits through the `log` crate (see its op
        // docs), and `tracing_subscriber`'s `init()` installs the `tracing-log`
        // bridge, so those records arrive at the subscriber as tracing events —
        // with the engine's span context attached, which is what makes the
        // `instruments` mode below read as a tree.
        #[cfg(feature = "tracing")]
        "tracing" => {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .init();
        }
        // As above, plus span open/close events so the engine's own
        // instrumentation is visible: `run` around the whole lifecycle,
        // `apply_nodes` per lifecycle phase, `cycle` per engine cycle (and
        // `cycle_node` per node, under `instrument-cycle-node`).
        #[cfg(feature = "tracing")]
        "instruments" => {
            use tracing_subscriber::fmt::format::FmtSpan;
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .with_span_events(FmtSpan::ENTER | FmtSpan::CLOSE)
                .init();
            // `tracing` alone pulls in the dependency; the spans themselves are
            // each behind their own `instrument-*` feature, so without one this
            // mode would silently look just like `tracing`.
            if !cfg!(feature = "instrument-run") {
                eprintln!(
                    "note: built with `tracing` but no `instrument-*` feature, so the engine \
                     emits no spans — rebuild with `--features instrument-default` (or \
                     `instrument-all` to include the per-node spans) to see them."
                );
            }
        }
        other => {
            eprintln!(
                "unknown mode: {other:?}. Use 'log'{}.",
                if cfg!(feature = "tracing") {
                    ", 'tracing', or 'instruments'"
                } else {
                    " (build with --features tracing for 'tracing' and 'instruments' modes)"
                }
            );
            std::process::exit(1);
        }
    }

    let g = GraphBuilder::new();
    let _out = build(&g);
    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `logged` is a pass-through debug tap: it emits a log record per tick but
    /// leaves the value stream unchanged. Over 3 cycles the counter it wraps
    /// still ticks 1, 2, 3.
    #[test]
    fn logged_passes_the_value_stream_through() {
        let g = GraphBuilder::new();
        let out = build(&g);
        let states = out.accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();

        assert_eq!(runner.value(&states), vec![1u64, 2, 3]);
    }
}
