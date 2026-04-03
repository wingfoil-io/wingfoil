//! Demonstrates wingfoil's tracing and instrumentation feature flags.
//!
//! Three run modes are shown:
//! - `log`         — default behaviour, output via `env_logger`
//! - `tracing`     — events routed through `tracing-subscriber` (pretty format)
//! - `instruments` — as above, plus lifecycle spans around `run` and `run_nodes`
//!
//! # Run
//!
//! ```sh
//! # Default log output
//! RUST_LOG=info cargo run --example tracing -- log
//!
//! # Events via tracing (same output, different subscriber)
//! RUST_LOG=info cargo run --example tracing --features tracing -- tracing
//!
//! # Events + lifecycle spans
//! RUST_LOG=info cargo run --example tracing --features instrument-run,instrument-run-nodes -- instruments
//! ```

use log::Level::Info;
use std::rc::Rc;
use std::time::Duration;
use wingfoil::*;

fn build_graph() -> Rc<dyn Stream<u64>> {
    ticker(Duration::from_secs(1)).count().logged("tick", Info)
}

fn main() {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "log".into())
        .to_lowercase();

    match mode.as_str() {
        "log" => {
            env_logger::init();
        }
        "tracing" | "instruments" => {
            #[cfg(feature = "tracing")]
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive(tracing_subscriber::filter::LevelFilter::INFO.into()),
                )
                .init();
            #[cfg(not(feature = "tracing"))]
            {
                eprintln!(
                    "Build with --features tracing (or instrument-run,instrument-run-nodes) \
                     to use the tracing subscriber."
                );
                std::process::exit(1);
            }
        }
        other => {
            eprintln!("unknown mode: {other:?}. Use 'log', 'tracing', or 'instruments'.");
            std::process::exit(1);
        }
    }

    let stream = build_graph();
    stream
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();
}
