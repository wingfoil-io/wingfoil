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
//!   shared [`Kernel`](wingfoil::codegen::Kernel). [`fluent`] layers the
//!   classic chaining style (`ticker(d).count().map(f)`) on top — wiring
//!   sugar only, identical execution.
//! - A compiled runner is a plain function with state in locals that calls
//!   **the same `Op::cycle` functions**, monomorphized — see
//!   `tests/compiled_parity.rs` for the hand-expanded odds/evens graph. (In
//!   the full design this expansion is produced by a `graph!` proc macro so
//!   wiring is written once; the macro is mechanical once this shape works.)
//!
//! The load-bearing property demonstrated here: **both engines execute the
//! identical semantics code**. There is no duplicated cycle logic anywhere —
//! not per-kind emitter strings, not `cycle_inline` twins — so the engines
//! cannot drift.
//!
//! Since built out from that prototype: the `graph!` macro (one wiring
//! function, both engines), threaded/IO sources (`Activation::THREADED` +
//! busy-spin `Activation::ALWAYS`), compiled islands nested in interpreted graphs,
//! and — as the first step of the full port — **fallible lifecycle**: every
//! `Op` function returns `anyhow::Result`, and the interpreted [`Runner`]
//! reports the first `start`/`cycle`/`stop`/`teardown` error with node
//! context while still running cleanup afterwards.
//!
//! Still out of scope for the prototype (documented, not forgotten):
//! variadic-input ops (merge/join are fixed at two inputs), an arena/SoA
//! value store for the interpreted engine, feedback edges, and dynamic
//! (runtime-mutated) graphs.

#[cfg(feature = "async")]
pub mod async_source;
pub mod channel;
pub mod compat;
pub mod fluent;
pub mod interp;
pub mod op;
pub mod ops;
pub mod stats;

/// The common wiring vocabulary, re-exported for `use wingfoil_next::prelude::*`.
///
/// Brings in the graph builder, the stream type, and the two core op traits
/// ([`SourceOps`](crate::fluent::SourceOps) for sources,
/// [`StreamOps`](crate::fluent::StreamOps) for combinators) so chaining works
/// without naming each trait. Adapter-specific op traits stay opt-in — pull
/// them in alongside, e.g. `use wingfoil_next::stats::StatisticsOps;`.
pub mod prelude {
    pub use crate::fluent::{GraphBuilder, SourceOps, Stream, StreamOps};
    pub use crate::{Burst, burst};
}

/// A group of same-instant values, delivered atomically in one cycle (never
/// coalesced / latest-wins). The **same type** as the classic engine's
/// `Burst<T>` — a `tinyvec::TinyVec<[T; 1]>` — re-exported (with its
/// [`burst!`] constructor macro) so both engines share the grouping type.
#[doc(inline)]
pub use wingfoil::{Burst, burst};

/// One wiring definition, two engines: expands to a module with
/// `interpreted()` (fluent wiring) and `compiled(run_mode, run_for)` (fully
/// monomorphized runner) emitted from the same tokens. See
/// [`wingfoil_next_macros`] for the DSL.
pub use wingfoil_next_macros::graph;

// Re-exported so `graph!`-generated code can reach the kernel and run types
// through `::wingfoil_next::wingfoil::...` without the caller depending on
// the `wingfoil` crate directly.
#[doc(hidden)]
pub use wingfoil;

// Re-exported so `graph!`-generated code (the fallible `compiled()` /
// `nested()` expansions) can name `Result` without the caller depending on
// `anyhow` directly.
#[doc(hidden)]
pub use anyhow;
