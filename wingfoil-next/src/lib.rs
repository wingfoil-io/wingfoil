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
//! Built out from that prototype, and what each module now holds:
//!
//! - **[`graph!`](macro@graph)** — one wiring function, three execution paths
//!   from the same tokens: `interpreted()`, fully-monomorphized `compiled()`,
//!   and `nested()` (a compiled island as one node of an interpreted graph).
//! - **[`fluent`]** — the chaining API as *extension traits*
//!   ([`SourceOps`](fluent::SourceOps) for sources,
//!   [`StreamOps`](fluent::StreamOps) for combinators), so the op vocabulary
//!   is open; [`prelude`] brings the common set into scope.
//! - **[`ops`]** — the op catalog (map/filter/fold/join/delay/window/… plus
//!   the sources), and **[`stats`]** — EWMA and rolling-window statistics as a
//!   separate opt-in [`StatisticsOps`](stats::StatisticsOps) trait.
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
//!   cleanup. **[`compat`]** offers a classic-style `Signal` facade over it.
//!
//! Still out of scope for the prototype (documented, not forgotten):
//! variadic-input ops (merge/join are fixed at two inputs), an arena/SoA value
//! store and breadth-first dirty-list scheduling for the interpreted engine
//! (see `docs/port-plan.md` "Phase 4.5"), and dynamic (runtime-mutated) graphs.

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
