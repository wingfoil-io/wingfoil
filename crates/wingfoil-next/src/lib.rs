//! **Design prototype**: what wingfoil's core abstractions look like if
//! designed from scratch to support dual execution — interpreted *and*
//! compiled — from one definition of node semantics.
//!
//! The retrofit on the main crate (`wingfoil::codegen`) hit three walls, all
//! caused by `MutableNode` fusing three concerns into one object:
//!
//! 1. **Semantics trapped in objects** — `cycle(&mut self, &mut GraphState)`
//!    couples the computation to its storage (fields behind `RefCell`) and
//!    its feeding (peeking upstream `Rc<dyn Stream>`s), so compiled runners
//!    had to *re-implement* node semantics as emitted source.
//! 2. **Types and closures erased at wiring time** — codegen had to
//!    reverse-engineer types from name strings and could never recover
//!    closures (hence the `Inputs` re-supply and its drift risk).
//! 3. **Activation invisible** — nothing declared "this node schedules
//!    callbacks", forcing name-based allowlists.
//!
//! This crate inverts all three:
//!
//! - [`Op`](op::Op) defines node semantics as a pure, monomorphizable
//!   associated function over **external** state and **typed inputs passed
//!   in** by the engine, with a `const ACTIVATION` declaration.
//! - [`interp::Builder`] is the interpreted engine: it owns the value slots
//!   and the state, adapts each `Op` behind one dyn boundary, and drives the
//!   shared [`Kernel`](runtime::kernel::Kernel). [`fluent`] layers the
//!   legacy chaining style (`ticker(d).count().map(f)`) on top — wiring
//!   sugar only, identical execution.
//! - A compiled runner is a plain function with state in locals that calls
//!   **the same `Op::cycle` functions**, monomorphized — see
//!   `tests/compiled_parity.rs` for the hand-expanded odds/evens graph. (In
//!   the full design this expansion is produced by a `nitro!` proc macro so
//!   wiring is written once; the macro is mechanical once this shape works.)
//!
//! The load-bearing property demonstrated here: **both engines execute the
//! identical semantics code**. There is no duplicated cycle logic anywhere —
//! not per-kind emitter strings, not `cycle_inline` twins — so the engines
//! cannot drift.
//!
//! Built out from that prototype, and what each module now holds:
//!
//! - **[`nitro!`](macro@nitro)** — one wiring function, three execution paths
//!   from the same tokens: `interpreted()`, fully-monomorphized `compiled()`,
//!   and `nested()` (a compiled island as one node of an interpreted graph).
//! - **[`fluent`]** — the chaining API as *extension traits*
//!   ([`SourceOps`](fluent::SourceOps) for sources,
//!   [`StreamOps`](fluent::StreamOps) for combinators), so the op vocabulary
//!   is open; [`prelude`] brings the common set into scope.
//! - **[`ops`]** — the op catalog (map/filter/fold/join/delay/window/… plus
//!   the sources), and **[`stats`]** — EWMA and rolling-window statistics as a
//!   separate opt-in [`StatisticsOps`](stats::StatisticsOps) trait. An op is
//!   single-sourced through **one mechanism**: `#[op(build = name)]` on its
//!   `Op` impl generates the interpreted `Builder` method *and* the `nitro!`
//!   forwarder functions every compiled/nested emission dispatches through,
//!   both derived from the op's declared shape — there is no per-op table in
//!   the macro, so built-in and user ops take the identical path — see
//!   `docs/port-plan.md` "Adding an op".
//! - **Sources in every activation mode**: `Activation::THREADED`
//!   [`external`](fluent::SourceOps::external), busy-spin `Activation::ALWAYS`
//!   [`poll`](fluent::SourceOps::poll), the both-modes
//!   [`channel`](fluent::SourceOps::channel), and
//!   [`feedback`](fluent::SourceOps::feedback) edges. All
//!   non-coalescing: same-instant values ride one [`Burst`], never latest-wins.
//! - **[`channel`]** — the `Message` envelope and senders; **`async_source`**
//!   (the `async` feature) wraps it as `produce_async`, an async producer of
//!   timestamped values that replays deterministically in historical mode.
//! - **Fallible lifecycle** — every `Op` function returns `anyhow::Result`;
//!   the interpreted [`Runner`](interp::Runner) reports the first
//!   `start`/`cycle`/`stop`/`teardown` error with node context and still runs
//!   cleanup. **[`compat`]** offers a legacy-style `Signal` facade over it.
//!
//! # Tracing and instrumentation
//!
//! The engine can emit `tracing` spans around its own execution, ported
//! feature-for-feature from legacy wingfoil. Every span site is behind its own
//! feature, and the `tracing` dependency itself is optional, so a default build
//! carries neither the dependency nor a single span:
//!
//! | feature | what it adds |
//! |---|---|
//! | `tracing` | The `tracing` dependency. On its own it emits nothing — enable one of the below. |
//! | `instrument-run` | A span around [`Runner::run`](interp::Runner::run) (and `run_dynamic`, under `dynamic-graph`) — the whole start→cycles→stop→teardown lifecycle. |
//! | `instrument-cycle` | A span around each engine cycle (one per dirty-node batch). |
//! | `instrument-apply-nodes` | A span around each lifecycle phase (start / stop / teardown) applied over all nodes, recording the phase in `desc`. |
//! | `instrument-initialise` | A span around graph initialisation ([`Builder::build`](interp::Builder::build)), named `initialise` after legacy's. |
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
//! Still out of scope for the prototype (documented, not forgotten):
//! variadic-input ops (merge/join are fixed at two inputs), an arena/SoA value
//! store and topologically-ordered dirty-list scheduling for the interpreted engine
//! (see `docs/port-plan.md` "Phase 4.5"), and dynamic (runtime-mutated) graphs.

// Lets this crate refer to itself as `wingfoil_next`, so the paths that
// `nitro!`-generated code emits (`::wingfoil_next::...`) resolve when the
// macro is expanded *inside* this crate — its own tests and examples — as
// well as downstream.
extern crate self as wingfoil_next;

pub mod adapters;
#[cfg(feature = "async")]
pub mod async_source;
/// The criterion harness (`add_bench`) used by the graph benchmarks — the
/// twin of legacy wingfoil's `bench`-gated `bencher` module.
#[cfg(feature = "bench")]
pub mod bencher;
pub mod channel;
pub mod compat;
pub mod interp;
pub mod latency;
pub mod op;
pub mod runtime;
// `ops` before `fluent`: `#[op(fluent)]` emits each op's fluent method as a
// `macro_rules!`, and a macro generated by a proc macro is reachable only
// through textual scope (rustc issue #52234 rejects the `crate::` path form),
// which runs in module declaration order. `fluent`'s trait impls invoke those
// macros, so the catalog that defines them has to be declared first.
#[macro_use]
pub mod ops;
pub mod fluent;
pub mod stats;

/// The common wiring vocabulary, re-exported for `use wingfoil_next::prelude::*`.
///
/// Brings in the graph builder, the stream type, and the two core op traits
/// ([`SourceOps`](crate::fluent::SourceOps) for sources,
/// [`StreamOps`](crate::fluent::StreamOps) for combinators) so chaining works
/// without naming each trait. Adapter-specific op traits stay opt-in — pull
/// them in alongside, e.g. `use wingfoil_next::stats::StatisticsOps;`.
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
pub use crate::runtime::kernel::{Kernel, KernelWaker, ReadyReceiver, waker_channel};
#[doc(inline)]
pub use crate::runtime::run::{RunFor, RunMode};
#[doc(inline)]
pub use crate::runtime::time::NanoTime;
#[doc(inline)]
pub use crate::runtime::time_queue::TimeQueue;

/// One wiring definition, two engines: expands to a module with
/// `interpreted()` (fluent wiring) and `compiled(run_mode, run_for)` (fully
/// monomorphized runner) emitted from the same tokens. See
/// [`wingfoil_next_macros`] for the DSL.
pub use wingfoil_next_macros::nitro;

// Re-exported so `nitro!`-generated code (the fallible `compiled()` /
// `nested()` expansions) can name `Result` without the caller depending on
// `anyhow` directly.
#[doc(hidden)]
pub use anyhow;

/// Re-exported so callers of [`StreamOps::logged`](crate::fluent::StreamOps::logged)
/// can name [`log::Level`] without adding a direct `log` dependency.
pub use log;
