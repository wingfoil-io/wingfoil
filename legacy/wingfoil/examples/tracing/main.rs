//! Demonstrates wingfoil's tracing and instrumentation feature flags.
//!
//! Three run modes are shown:
//! - `log`         — default behaviour, output via `env_logger`
//! - `tracing`     — events routed through `tracing-subscriber` (pretty format)
//! - `instruments` — as above, plus span open/close around `run` and each engine cycle
//!
//! # Run
//!
//! ```sh
//! # Default log output
//! RUST_LOG=info cargo run --example tracing -- log
//!
//! # Events via tracing subscriber
//! RUST_LOG=info cargo run --example tracing --features tracing -- tracing
//!
//! # Events + lifecycle and cycle spans
//! RUST_LOG=info cargo run --example tracing --features instrument-default -- instruments
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
        #[cfg(feature = "tracing")]
        "tracing" => {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .init();
        }
        #[cfg(feature = "tracing")]
        "instruments" => {
            use tracing_subscriber::fmt::format::FmtSpan;
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .with_span_events(FmtSpan::ENTER | FmtSpan::CLOSE)
                .init();
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

    let stream = build_graph();
    stream
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();
}
