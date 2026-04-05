#![warn(clippy::perf)]
#![allow(clippy::type_complexity)]
#![allow(clippy::needless_doctest_main)]
#![doc = include_str!("../README.md")]

//! ## Graph Execution

//! Wingfoil abstracts away the details of how to co-ordinate the calculation of your
//! application, parts of which may be executing at different frequencies.

//! Only the nodes that actually require cycling are executed which allows wingfoil to
//! efficiently scale to very large graphs.

//! Consider the following example:
//! ```rust
//! use wingfoil::*;
//! use std::time::Duration;
//!
//! fn main() {
//!     let period = Duration::from_millis(10);
//!     let source = ticker(period).count(); // 1, 2, 3 etc
//!     let is_even = source.map(|i| i % 2 == 0);
//!     let odds = source
//!         .filter(is_even.not())
//!         .map(|i| format!("{:} is odd", i));
//!     let evens = source
//!         .filter(is_even)
//!         .map(|i| format!("{:} is even", i));
//!     merge(vec![odds, evens])
//!         .print()
//!         .run(
//!             RunMode::HistoricalFrom(NanoTime::ZERO),
//!             RunFor::Duration(period * 5),
//!         );
//! }
//! ```
//! This output is produced:  
//! ```pre
//! 1 is odd
//! 2 is even
//! 3 is odd
//! 4 is even
//! 5 is odd
//! 6 is even
//! ````
//! We can visualise the graph like this:
//! <div align="center">
//! <img alt="diagram" src="https://raw.githubusercontent.com/wingfoil-io/wingfoil/refs/heads/main/wingfoil/diagrams/odds_evens.png" width="400"/>
//! </div>
//! The input and output nodes tick 6 times each and the evens and odds nodes tick 3 times.
//!
//! ## Historical vs RealTime
//! Time is a first-class citizen in wingfoil.  Engine time is measured in nanoseconds from the
//! [UNIX epoch](https://en.wikipedia.org/wiki/Unix_time) and represented by a [NanoTime].
//!
//! In this example we compare and contrast RealTime vs Historical RunMode.   RealTime is used for
//! production deployment.   Historical is used for development, unit-testing, integration-testing
//! and back-testing.
//!
//! ```rust
//! use wingfoil::*;
//! use std::time::Duration;
//! use log::Level::Info;
//!
//! pub fn main() {
//!     env_logger::init();
//!     for run_mode in vec![
//!         RunMode::RealTime,
//!         RunMode::HistoricalFrom(NanoTime::ZERO)
//!     ] {
//!         println!("\nUsing RunMode::{:?}", run_mode);
//!         ticker(Duration::from_secs(1))
//!             .count()
//!             .logged("tick", Info)
//!             .run(run_mode, RunFor::Cycles(3));
//!     }
//! }
//! ```
//! This output is produced:
//! <pre>
//! Using RunMode::RealTime
//! [17:34:4<span style=color:red>6</span>Z INFO wingfoil] <span style=color:red>0</span>.000_001 tick 1
//! [17:34:4<span style=color:red>7</span>Z INFO wingfoil] <span style=color:red>1</span>.000_131 tick 2
//! [17:34:4<span style=color:red>8</span>Z INFO wingfoil] <span style=color:red>2</span>.000_381 tick 3
//!
//! Using RunMode::HistoricalFrom(NanoTime(0))
//! [17:34:4<span style=color:red>8</span>Z INFO  wingfoil] <span style=color:red>0</span>.000_000 tick 1
//! [17:34:4<span style=color:red>8</span>Z INFO  wingfoil] <span style=color:red>1</span>.000_000 tick 2
//! [17:34:4<span style=color:red>8</span>Z INFO  wingfoil] <span style=color:red>2</span>.000_000 tick 3
//! </pre>
//! In Realtime mode the log statements are written every second.  In Historical mode the log statements are written
//! immediately.   In both cases engine time advances by 1 second between each tick.
//!
#![doc = include_str!("../examples/order_book/README.md")]
//! See the [order book example](https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/order_book/) for more details.
#![doc = include_str!("../examples/async/README.md")]
//! See the [async example](https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/async/) for more details.
#![doc = include_str!("../examples/breadth_first/README.md")]
//! See the [breadth first example](https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/breadth_first/) for more details.
#![doc = include_str!("../examples/dynamic/README.md")]
//! See the [dynamic examples](https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/dynamic/) for more details.
//!
//! ## Multithreading
//!
//! Wingfoil supports multi-threading to distribute workloads across cores.  The approach
//! is to compose sub-graphs, each running in its own dedicated thread.
//!
//! ## Messaging
//!
//! Wingfoil is agnostic to messaging and we plan to add support for shared memory IPC adapters such as Aeron
//! for ultra-low latency use cases, ZeroMq for low latency server to server communication and Kafka for high latency,
//! fault tolerant messaging.
//!
//! ## Graph Dynamism
//!
//! Some use cases require graph-dynamism - consider for example Request for Quote (RFQ) markets, where
//! the graph needs to be able to adapt to an incoming RFQ.  Conceptually this could be done by changing the
//! shape of the graph at run time.  However, we take a simpler approach by virtualising this, following a
//! demux pattern.  
//!
//! ## Performance
//!
//! We take performance seriously, and ongoing work is focused on making **Wingfoil** even closer to a **zero-cost abstraction**,
//! Currently, the overhead of cycling a node in the graph is less than **10 nanoseconds**.
//!  
//! For best performance we recommend using **cheaply cloneable** types:
//!
//! - For small strings: [`arraystring`](https://crates.io/crates/arraystring)
//! - For small vectors: [`tinyvec`](https://crates.io/crates/tinyvec)
//! - For larger or heap-allocated types:
//!   - Use [`Rc<T>`](https://doc.rust-lang.org/std/rc/struct.Rc.html) for single threaded contexts.
//!   - Use [`Arc<T>`](https://doc.rust-lang.org/std/sync/struct.Arc.html) for multithreaded contexts.
//!
//! ## Observability
//!
//! Wingfoil supports both the [`log`](https://docs.rs/log) and [`tracing`](https://docs.rs/tracing)
//! ecosystems via cargo feature flags.
//!
//! ### Feature flags
//!
//! | Feature | Effect |
//! |---|---|
//! | *(none)* | [`logged()`](StreamExt::logged) and [`GraphState::log()`] emit via the `log` crate — wire up any `log`-compatible backend (e.g. `env_logger`). |
//! | `tracing` | Events are emitted via `tracing` instead. A `tracing` subscriber is required; the `log` bridge ensures events still reach `env_logger` if none is installed. |
//! | `instrument-run` | Adds a tracing span around [`Graph::run()`] (the full setup→run→teardown lifecycle). |
//! | `instrument-cycle` | Adds a tracing span around each engine cycle (one span per dirty-node batch). |
//! | `instrument-apply-nodes` | Adds a tracing span around each lifecycle phase (setup / start / stop / teardown), recording the phase name. |
//! | `instrument-initialise` | Adds a tracing span around graph initialisation. |
//! | `instrument-cycle-node` | Adds a tracing span per node execution, recording the node index and type name. High frequency — opt in deliberately. |
//! | `instrument-default` | Enables `instrument-run`, `instrument-cycle`, `instrument-apply-nodes`, and `instrument-initialise`. |
//! | `instrument-all` | Enables `instrument-default` plus `instrument-cycle-node`. |
//!
//! All `instrument-*` features imply `tracing`.
//!
//! ### Example
//!
//! ```rust,no_run
//! use log::Level::Info;
//! use std::time::Duration;
//! use wingfoil::*;
//!
//! // With the `tracing` feature and a subscriber installed:
//! // tracing_subscriber::fmt::init();
//!
//! ticker(Duration::from_secs(1))
//!     .count()
//!     .logged("tick", Info)  // emits a tracing event per tick when feature = "tracing"
//!     .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
//!     .unwrap();
//! ```
//!
//! See the [tracing example](https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/tracing/)
//! for a runnable demonstration.

#[macro_use]
extern crate log;
extern crate derive_new;

// Dispatch a `log::Level` runtime value to the matching `tracing` event macro.
//
// Two forms:
// - `tracing_log!(level, <tracing args>)` — generic passthrough
// - `tracing_log!(level; time, label, value)` — wingfoil stream event; accepts a
//   `NanoTime` and only calls `.pretty()` inside the enabled arm.
//
// Only available with the `tracing` feature.
#[cfg(feature = "tracing")]
macro_rules! tracing_log {
    ($level:expr; $time:expr, $label:expr, $value:expr) => {
        match $level {
            log::Level::Error => tracing::error!(target: "wingfoil", "{} {} {:?}", $time.pretty(), $label, $value),
            log::Level::Warn  => tracing::warn!(target:  "wingfoil", "{} {} {:?}", $time.pretty(), $label, $value),
            log::Level::Info  => tracing::info!(target:  "wingfoil", "{} {} {:?}", $time.pretty(), $label, $value),
            log::Level::Debug => tracing::debug!(target: "wingfoil", "{} {} {:?}", $time.pretty(), $label, $value),
            log::Level::Trace => tracing::trace!(target: "wingfoil", "{} {} {:?}", $time.pretty(), $label, $value),
        }
    };
    ($level:expr, $($rest:tt)*) => {
        match $level {
            log::Level::Error => tracing::error!($($rest)*),
            log::Level::Warn  => tracing::warn!($($rest)*),
            log::Level::Info  => tracing::info!($($rest)*),
            log::Level::Debug => tracing::debug!($($rest)*),
            log::Level::Trace => tracing::trace!($($rest)*),
        }
    };
}

// Check whether a `log::Level` is enabled in the current `tracing` subscriber.
// Only available with the `tracing` feature.
#[cfg(feature = "tracing")]
macro_rules! tracing_log_enabled {
    ($level:expr) => {
        match $level {
            log::Level::Error => tracing::enabled!(tracing::Level::ERROR),
            log::Level::Warn => tracing::enabled!(tracing::Level::WARN),
            log::Level::Info => tracing::enabled!(tracing::Level::INFO),
            log::Level::Debug => tracing::enabled!(tracing::Level::DEBUG),
            log::Level::Trace => tracing::enabled!(tracing::Level::TRACE),
        }
    };
}

pub mod adapters;

mod bencher;
mod channel;
mod graph;
mod nodes;
mod queue;
mod time;
mod types;

pub use bencher::*;
pub use graph::*;
pub use nodes::*;
pub use queue::*;
pub use types::*;
