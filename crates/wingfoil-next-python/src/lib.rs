//! Python interop boundary for **wingfoil-next**.
//!
//! The design objective (see `docs/python-interop.md`) is that users can
//! author IO adapters, ops, and wiring logic in Rust, then compose *and extend*
//! them from Python alongside the built-in vocabulary. That only works if
//! everything Python-composable rides a single **erased value type** so the
//! interpreted engine can wire any node to any node. This crate provides that
//! type: [`PyElement`].
//!
//! The rule is **only the Python-exposed edges erase**. A node the user wants
//! Python to wire into is `Stream<PyElement>`; the *interior* of a Rust op or
//! sub-graph stays natively typed, converting to/from `PyElement` only at the
//! seam. `PyElement` therefore has to (a) satisfy next's stream-value bounds —
//! `Clone + Default + 'static`, plus `PartialEq` (delay/distinct/feedback dedup)
//! and `Debug` (print/timed) — and (b) carry the edge conversions to and from
//! the concrete Rust scalars ops actually compute on.
//!
//! This is the same shape the legacy `wingfoil-python` bindings proved out; it
//! is lifted into the next tree so the interpreted engine has a boundary lane of
//! its own.

// So `#[pyop]`-generated code — which names `::wingfoil_next_python::...` for
// downstream crates — also resolves when the macro is used inside this crate.
extern crate self as wingfoil_next_python;

pub mod adapters;
pub mod element;
pub mod graph;
pub mod island;
pub mod latency;
#[macro_use]
mod macros;
mod python;
pub mod statistics;

pub use element::PyElement;
pub use graph::{PyGraph, PyStream};
pub use python::{Graph, Stream, to_pyerr};

/// Derive a Python-callable function from an `Op` impl. See
/// [`wingfoil_next_python_macros`] — placed on `impl Op for MyOp`, it generates
/// a free `#[pyfunction]` wiring the op at the erased boundary.
pub use wingfoil_next_python_macros::pyop;

/// Expose a Rust-authored sub-graph wiring function (`fn(&Stream<T>) ->
/// Stream<U>`) as a Python callable that splices its nodes into the caller's
/// graph. See [`wingfoil_next_python_macros::pygraph`].
pub use wingfoil_next_python_macros::pygraph;

/// Expose a user **source** adapter (`impl Trait for GraphBuilder { fn m(&self,
/// …) -> Stream<T> }`) as a Python callable `module.m(graph, …)`. See
/// [`wingfoil_next_python_macros::pyadapter`].
pub use wingfoil_next_python_macros::pyadapter;

// Re-exported so third-party op crates (and the `pyop!`/`#[pyop]` macros) can
// name the op vocabulary without depending on `wingfoil-next` directly.
pub use wingfoil_next::op::{Activation, Ctx, Op, Tick};
