#![doc = include_str!("./README.md")]
//!
//! ```sh
//! # Default log output (env_logger)
//! RUST_LOG=info cargo run -p wingfoil-next --example tracing
//! RUST_LOG=info cargo run -p wingfoil-next --example tracing -- log
//! ```

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

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
        // The classic example also offers `tracing` (route the events through a
        // `tracing-subscriber`) and `instruments` (engine spans around `run` and
        // each cycle). Neither is available on wingfoil-next yet: the engine has
        // no `tracing` / `instrument-*` features, so there are no spans to emit,
        // and the op catalog logs through the `log` crate only. Both are tracked
        // in `docs/port-plan.md` (Phase 6). Fall back to the `log` mode so the
        // command still does something useful.
        "tracing" | "instruments" => {
            eprintln!(
                "mode {mode:?} is not available in wingfoil-next yet — it needs the \
                 `tracing` / `instrument-*` engine features, which have not been ported \
                 from legacy (only the `log` mode is supported today). Running `log` instead."
            );
            env_logger::init();
        }
        other => {
            eprintln!("unknown mode: {other:?}. Use 'log'.");
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
