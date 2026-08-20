//! A Rust stream-processing library: build a directed acyclic graph of data
//! transformations once, then run it against live data or replay it over
//! history with identical semantics.
//!
//! ```
//! use std::time::Duration;
//! use wingfoil::prelude::*;
//! use wingfoil::{NanoTime, RunFor, RunMode};
//!
//! let g = GraphBuilder::new();
//! let count = g.ticker(Duration::from_millis(10)).count();
//! let is_even = count.map(|n: &u64| n.is_multiple_of(2));
//! let total = count.filter(&is_even).fold(0u64, |acc, v| *acc += v);
//!
//! let mut runner = g.build();
//! runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(10)).unwrap();
//! assert_eq!(runner.value(&total), 2 + 4 + 6 + 8 + 10);
//! ```
//!
//! Swap `RunMode::HistoricalFrom` for `RunMode::RealTime` and the same graph
//! runs against the wall clock — that is the point of the two run modes.
//!
//! # The idea: node semantics as a function, not an object
//!
//! An [`Op`](op::Op) defines what a node *does* as a pure associated
//! function — `cycle(cfg, state, input, ctx)` — over engine-owned state and
//! typed inputs the engine passes in, with a `const ACTIVATION` declaring how
//! it is scheduled. Nothing about a node's computation is tied to how its
//! storage is allocated or how its inputs are fetched.
//!
//! That separation is what lets **one** definition drive several execution
//! strategies, and it is the property the whole design is arranged around:
//!
//! - the **interpreted** engine ([`interp::Builder`]) owns the value slots and
//!   the state, adapts each `Op` behind a single dyn boundary, and drives the
//!   [`Kernel`];
//! - a **compiled** runner is a plain function with node state in local
//!   variables, calling the same `Op::cycle` functions monomorphized, with
//!   tick propagation as `bool`s the optimiser can see through;
//! - a **nested island** packs a whole sub-graph into one node of an
//!   interpreted graph, running that same compiled code inside it.
//!
//! Every one of those executes the *identical* semantics code. There is no
//! duplicated cycle logic anywhere — no per-kind emitter strings, no
//! `cycle_inline` twins — so the strategies cannot drift from each other.
//! [`nitro!`](macro@nitro) is the front door: one wiring function in, all
//! three out.
//!
//! # The tour
//!
//! - **[`nitro!`](macro@nitro)** — one wiring function, three execution paths
//!   from the same tokens: `interpreted()`, fully-monomorphized `compiled()`,
//!   and `nested()` (a compiled island as one node of an interpreted graph).
//! - **[`fluent`]** — the chaining API as *extension traits*
//!   ([`SourceOps`](fluent::SourceOps) for sources,
//!   [`StreamOps`](fluent::StreamOps) for combinators), so the op vocabulary
//!   is open; [`prelude`] brings the common set into scope.
//! - **[`ops`]** — the op catalog (map/filter/fold/join/delay/window/… plus
//!   the sources), and
//!   **[`adapters::statistics`](adapters::statistics)** — EWMA and
//!   rolling-window statistics as a separate opt-in
//!   [`StatisticsOps`](adapters::statistics::StatisticsOps) trait. An op is
//!   single-sourced through **one mechanism**: `#[op(build = name)]` on its
//!   `Op` impl generates the interpreted `Builder` method *and* the `nitro!`
//!   forwarder functions every compiled/nested emission dispatches through,
//!   both derived from the op's declared shape — there is no per-op table in
//!   the macro, so built-in and user ops take the identical path.
//! - **Sources in every activation mode**: `Activation::THREADED`
//!   [`external`](fluent::SourceOps::external), busy-spin `Activation::ALWAYS`
//!   [`poll`](fluent::SourceOps::poll), the both-modes
//!   [`channel`](fluent::SourceOps::channel), and
//!   [`feedback`](fluent::SourceOps::feedback) edges. All
//!   non-coalescing: same-instant values ride one [`Burst`], never latest-wins.
//! - **[`adapters`]** — the I/O surface (CSV, Kafka, ZeroMQ, KDB+, Redis,
//!   Postgres, etcd, FIX, web, Aeron, iceoryx2, Fluvio, augurs, Prometheus,
//!   OTLP), each behind its own feature and kept out of the prelude — opt in
//!   with `use wingfoil::adapters::<name>::…`.
//! - **[`latency`]** — stamp wall-clock timestamps onto messages as they hop
//!   through ops and across processes, then aggregate per-stage deltas.
//! - **[`introspect`]** — the wired topology as data and as pictures
//!   (Graphviz / Mermaid / JSON / GML), from
//!   [`GraphBuilder::snapshot`](fluent::GraphBuilder::snapshot) or
//!   [`Runner::snapshot`](interp::Runner::snapshot). Active and passive edges
//!   are drawn differently, which is usually the reason to want the picture.
//! - **[`channel`]** — the `Message` envelope and senders; **`async_source`**
//!   (the `async` feature) wraps it as `produce_async`, an async producer of
//!   timestamped values that replays deterministically in historical mode.
//! - **[`pool`]** — recycled payload buffers behind cheap non-atomic
//!   [`Pooled`](pool::Pooled) handles, and the loan-based
//!   [`pooled_channel`](fluent::SourceOps::pooled_channel) producer API:
//!   zero payload allocations at steady state, with the bounded pool as
//!   backpressure.
//! - **Fallible lifecycle** — every `Op` function returns `anyhow::Result`;
//!   the interpreted [`Runner`](interp::Runner) reports the first
//!   `start`/`cycle`/`stop`/`teardown` error with node context and still runs
//!   cleanup.
//!
//! For how the pieces fit together, and why, see
//! `docs/wingfoil-architecture.md`.
//!
//! # Threading: a graph lives on one thread
//!
//! **[`GraphBuilder`](fluent::GraphBuilder), [`Stream<T>`](fluent::Stream) and
//! [`Runner`](interp::Runner) are `!Send` and `!Sync`.** They hold `Rc`
//! internally, so wiring, [`build`](fluent::GraphBuilder::build) and
//! [`run`](interp::Runner::run) all have to happen on the same thread, and
//! moving any of them into `std::thread::spawn` is a compile error along the
//! lines of:
//!
//! ```text
//! error[E0277]: `Rc<RefCell<wingfoil::interp::Builder>>` cannot be sent
//!               between threads safely
//!   = help: within `{closure@...}`, the trait `Send` is not implemented for
//!           `Rc<RefCell<wingfoil::interp::Builder>>`
//! note: required because it appears within the type
//!       `wingfoil::fluent::GraphBuilder`
//! ```
//!
//! (Whichever of the three you moved: `Stream<T>` names the same `Rc`, and
//! `Runner` names the `Rc` slots it owns.)
//!
//! That is a deliberate contract, not a missing impl. Node state is
//! `RefCell`-owned by the engine and read back through `Rc` slots precisely so
//! that **no lock is ever taken on the graph execution path** — a mutex inside
//! `cycle` would be a correctness problem as much as a cost one. Making the
//! wiring types `Send` would only move the synchronisation somewhere less
//! visible.
//!
//! Nothing stops you running *several* graphs, one per thread. What you cannot
//! do is share one.
//!
//! ## Crossing the thread boundary
//!
//! Every supported crossing hands you a `Send` half while the graph stays
//! where it is:
//!
//! | You want to… | Wire | The `Send` half to move |
//! |---|---|---|
//! | feed a graph from a thread, socket or async task | [`channel`](fluent::SourceOps::channel) | [`ChannelSender<T>`](channel::ChannelSender) |
//! | …the same, with recycled buffers and no payload allocation | [`pooled_channel`](fluent::SourceOps::pooled_channel) | [`PooledSender<T>`](pool::PooledSender) |
//! | …the same, but connect at run start rather than at wiring | [`source_at_start`](fluent::SourceOps::source_at_start) | `ChannelSender<T>`, handed to your `setup` |
//! | run a whole producer sub-graph on a worker thread | [`spawn`](fluent::SourceOps::spawn) | nothing — the worker wires its own graph |
//! | offload one stage of an existing pipeline | [`spawn_map`](fluent::StreamOps::spawn_map) | nothing — ditto, in lock-step |
//! | push values in from realtime code, minimal envelope | [`external`](fluent::SourceOps::external) | [`ExternalSource<T>`](interp::ExternalSource) |
//!
//! [`channel`](fluent::SourceOps::channel) is the general answer, and the one
//! the I/O adapters are built on. The sender is `Send` and clonable, each
//! `send` wakes the realtime kernel, and
//! [`send_at`](channel::ChannelSender::send_at) stamps a value so a historical
//! replay stays deterministic:
//!
//! ```
//! use std::thread;
//! use std::time::Duration;
//! use wingfoil::prelude::*;
//! use wingfoil::{NanoTime, RunFor, RunMode};
//!
//! let g = GraphBuilder::new();
//! // The `Stream` stays on this thread; the `ChannelSender` is the `Send` half.
//! let (values, sender) = g.channel::<u64>();
//! let total = values
//!     .map(|b: &Burst<u64>| b.iter().sum::<u64>())
//!     .fold(0u64, |acc, v| *acc += v);
//! let mut runner = g.build();
//!
//! let producer = thread::spawn(move || {
//!     for i in 1..=3u64 {
//!         sender.send_at(i, NanoTime::from(Duration::from_secs(i)));
//!     }
//!     // End-of-stream: the receiving graph winds down once it has drained.
//!     sender.close();
//! });
//!
//! runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever).unwrap();
//! producer.join().unwrap();
//! assert_eq!(runner.value(&total), 1 + 2 + 3);
//! ```
//!
//! Note what did *not* cross: `g`, `values`, `total` and `runner` all stayed on
//! the calling thread. If the producer is itself a wingfoil graph, reach for
//! [`spawn`](fluent::SourceOps::spawn) instead and it will wire and run one on
//! the worker for you — see `examples/core/threading` and `examples/core/spawn`.
//!
//! # Tracing and instrumentation
//!
//! The engine can emit `tracing` spans around its own execution. Every span
//! site is behind its own feature, and the `tracing` dependency itself is
//! optional, so a default build carries neither the dependency nor a single
//! span:
//!
//! | feature | what it adds |
//! |---|---|
//! | `tracing` | The `tracing` dependency. On its own it emits nothing — enable one of the below. |
//! | `instrument-run` | A span around [`Runner::run`](interp::Runner::run) (and `run_dynamic`, under `dynamic-graph`) — the whole start→cycles→stop→teardown lifecycle. |
//! | `instrument-cycle` | A span around each engine cycle (one per dirty-node batch). |
//! | `instrument-apply-nodes` | A span around each lifecycle phase (start / stop / teardown) applied over all nodes, recording the phase in `desc`. |
//! | `instrument-initialise` | A span around graph initialisation ([`Builder::build`](interp::Builder::build)). |
//! | `instrument-cycle-node` | A span per node execution, recording the node index and label. High frequency — opt in deliberately. |
//! | `instrument-default` | `instrument-run` + `instrument-cycle` + `instrument-apply-nodes` + `instrument-initialise`. |
//! | `instrument-all` | `instrument-default` plus `instrument-cycle-node`. |
//!
//! All `instrument-*` features imply `tracing`. Both dispatch strategies (the
//! sparse drain and the [`FullSweep`](interp::Dispatch::FullSweep) oracle) emit
//! the same spans, so instrumentation cannot tell them apart — just as results
//! cannot. See `examples/core/tracing` for a runnable demonstration, and
//! [`StreamOps::logged`](fluent::StreamOps::logged) for the per-value debug tap
//! (which emits through the `log` crate, independently of these features).
//!
//! # Known limits
//!
//! Documented, not forgotten: `merge`/`join` are fixed at two inputs on the
//! compiled path (the interpreted side has variadic `merge_n`); the
//! interpreted value store is per-node slots rather than an arena/SoA, and its
//! dirty list is drained rather than topologically ordered; and `compiled()`
//! is a closed box — static topology, outputs only, no I/O or live inputs, by
//! design. See `docs/planning/port-plan.md` "Deferred / post-v1 work".

// Every public item carries rustdoc, and CI keeps it that way: `cargo lint`
// runs clippy with `-D warnings`, so this warn is effectively a deny. Adding a
// public item without a doc comment breaks the build — which is the point.
// Write a real sentence; a comment restating the item's name silences the lint
// and tells the reader nothing.
#![warn(missing_docs)]

// Lets this crate refer to itself as `wingfoil`, so the paths that
// `nitro!`-generated code emits (`::wingfoil::...`) resolve when the
// macro is expanded *inside* this crate — its own tests and examples — as
// well as downstream.
extern crate self as wingfoil;

#[cfg(feature = "async")]
pub mod async_source;
/// The criterion harness (`add_bench`) used by the graph benchmarks — the
/// twin of legacy wingfoil's `bench`-gated `bencher` module.
#[cfg(feature = "bench")]
pub mod bencher;
pub mod channel;
pub mod interp;
pub mod introspect;
pub mod latency;
pub mod op;
pub mod pool;
pub mod runtime;
pub mod tier;
// `ops` before `fluent` / `adapters`: `#[op(fluent)]` emits each op's
// fluent method as a `macro_rules!`, and a macro generated by a
// proc macro is reachable only through textual scope (rustc issue #52234
// rejects the `crate::` path form), which runs in module declaration order.
// Both invoke the macros, so the catalog that defines them has to be
// declared first.
//
// `adapters` is in that list because of one member: `adapters::statistics`
// invokes `__wf_fluent_*!` for its whole surface. It used to be a top-level
// `stats` module declared right here for exactly that reason; moving it under
// `adapters` moved the constraint with it, so the whole adapter tree now sits
// below `ops`. Nothing else under `adapters` depends on the order.
#[macro_use]
pub mod ops;
pub mod adapters;
pub mod fluent;

/// The common wiring vocabulary, re-exported for `use wingfoil::prelude::*`.
///
/// Brings in the graph builder, the stream type, and the two core op traits
/// ([`SourceOps`](crate::fluent::SourceOps) for sources,
/// [`StreamOps`](crate::fluent::StreamOps) for combinators) so chaining works
/// without naming each trait. Adapter-specific op traits stay opt-in — pull
/// them in alongside, e.g. `use wingfoil::adapters::statistics::StatisticsOps;`.
pub mod prelude {
    pub use crate::fluent::{GraphBuilder, SourceOps, Stream, StreamOps, Upstream};
    pub use crate::op::{Activation, Ctx, Tick};
    pub use crate::{Burst, burst};
}

/// A group of same-instant values, delivered atomically in one cycle (never
/// coalesced / latest-wins). A `tinyvec::TinyVec<[T; 1]>`, defined here and
/// re-exported by the legacy `wingfoil` crate (with its [`burst!`]
/// constructor macro), so both engines share one grouping type.
#[doc(inline)]
pub use crate::runtime::burst::Burst;

/// The shared runtime core, re-exported at the crate root: engine time, the
/// run bounds, the scheduled-callback queue and the [`Kernel`] that drives a
/// run. The legacy `wingfoil` crate re-exports these same items — they are
/// one set of types, not two — see [`runtime`] for why the core lives here.
#[doc(inline)]
pub use crate::runtime::kernel::{Kernel, KernelWaker, ReadyReceiver, TimerPolicy, waker_channel};
#[doc(inline)]
pub use crate::runtime::run::{RunFor, RunMode};
#[doc(inline)]
pub use crate::runtime::time::NanoTime;
#[doc(inline)]
pub use crate::runtime::time_queue::TimeQueue;

/// Which `nitro!` engine executes a graph, for the generated
/// `run(tier, run_mode, run_for)` — interpreted while developing, compiled in
/// production, one argument apart.
#[doc(inline)]
pub use crate::tier::Tier;

/// One wiring definition, two engines: expands to a module with
/// `interpreted()` (fluent wiring), `compiled(run_mode, run_for)` (fully
/// monomorphized runner) and `run(tier, ..)` (either, same outputs) emitted
/// from the same tokens. See [`wingfoil_derive`] for the DSL.
pub use wingfoil_derive::nitro;

/// Turn an `impl Op for …` block into a first-class op: the interpreted
/// wiring (an extension trait on [`interp::Builder`]), the
/// naming-convention forwarders `nitro!`'s `compiled()` / `nested()` emission
/// dispatches through, and — with `fluent` — the `macro_rules!` that writes
/// the op's fluent method.
///
/// Re-exported here (in the *macro* namespace, alongside the [`op`](mod@op)
/// module) so a downstream crate can author an op with `use wingfoil::op;`
/// and nothing else. The expansion names `::wingfoil::…` throughout, so it
/// resolves in any crate that depends on this one under its own name — see
/// `docs/adding-an-op.md` for the renamed-dependency caveat.
pub use wingfoil_derive::op;

// Re-exported so `nitro!`-generated code (the fallible `compiled()` /
// `nested()` expansions) can name `Result` without the caller depending on
// `anyhow` directly.
#[doc(hidden)]
pub use anyhow;

/// Re-exported so callers of [`StreamOps::logged`](crate::fluent::StreamOps::logged)
/// can name [`log::Level`] without adding a direct `log` dependency.
pub use log;

// Re-exported so `latency_stages!`-generated records can derive `Serialize` /
// `Deserialize` without the calling crate depending on `serde` directly. The
// generated struct carries `#[serde(crate = "::wingfoil::__serde")]` so the
// derive's own internal paths resolve here too.
#[doc(hidden)]
pub use serde as __serde;
