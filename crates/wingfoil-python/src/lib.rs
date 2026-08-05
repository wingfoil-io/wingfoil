//! Python interop boundary for **wingfoil**.
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
//! seam. `PyElement` therefore has to (a) satisfy wingfoil's stream-value bounds —
//! `Clone + Default + 'static`, plus `PartialEq` (delay/distinct/feedback dedup)
//! and `Debug` (print/timed) — and (b) carry the edge conversions to and from
//! the concrete Rust scalars ops actually compute on.
//!
//! This is the same shape the legacy `wingfoil-python` bindings proved out; it
//! is lifted into the wingfoil tree so the interpreted engine has a boundary lane of
//! its own.

// So `#[pyop]`-generated code — which names `::wingfoil_python::...` for
// downstream crates — also resolves when the macro is used inside this crate.
extern crate self as wingfoil_python;

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
/// [`wingfoil_python_derive`] — placed on `impl Op for MyOp`, it generates
/// a free `#[pyfunction]` wiring the op at the erased boundary.
pub use wingfoil_python_derive::pyop;

/// Expose a Rust-authored sub-graph wiring function (`fn(&Stream<T>) ->
/// Stream<U>`) as a Python callable that splices its nodes into the caller's
/// graph. See [`wingfoil_python_derive::pygraph`].
pub use wingfoil_python_derive::pygraph;

/// Expose a user **source** adapter (`impl Trait for GraphBuilder { fn m(&self,
/// …) -> Stream<T> }`) as a Python callable `module.m(graph, …)`. See
/// [`wingfoil_python_derive::pyadapter`].
pub use wingfoil_python_derive::pyadapter;

// Re-exported so third-party op crates (and the `pyop!`/`#[pyop]` macros) can
// name the op vocabulary without depending on `wingfoil` directly.
pub use wingfoil::op::{Activation, Ctx, Op, Tick};

// Named by `register_pyfn!` through `$crate::inventory`, so a caller does not
// have to depend on `inventory` itself.
#[doc(hidden)]
pub use inventory;

/// A deferred `#[pyfunction]` registration, collected at link time.
///
/// **Why this exists.** Every binding used to be named twice: once where it is
/// defined, and once more in a hand-maintained list inside the `#[pymodule]` —
/// 64 `m.add_function(wrap_pyfunction!(…))?` lines, plus the `#[cfg(feature)]`
/// blocks and `use` imports that list needed to reach feature-gated adapters.
/// Nothing checked the two halves against each other: a binding whose second
/// mention was forgotten simply did not exist in Python, and compiled fine.
///
/// Now a binding registers itself where it is defined. `#[pyadapter]` and
/// `#[pyop]` emit the submission with the function they generate, so those are
/// zero-touch; a hand-written `#[pyfunction]` uses [`register_pyfn!`].
///
/// **Collection is per-shared-object**, which is what makes this safe for the
/// third-party crates `#[pyadapter]` is meant to serve. Their submissions land
/// in *their* cdylib's section, not this one's, and their `#[pymodule]` decides
/// whether to iterate at all — so an out-of-tree adapter cannot inject itself
/// into the `wingfoil` module, and this module's bindings do not leak into
/// theirs.
///
/// Iteration order is unspecified. That is fine here and must stay fine:
/// registering into a module dict is order-independent. Do not add a
/// registrar whose effect depends on running before or after another.
pub struct PyFnRegistrar(pub fn(&pyo3::Bound<'_, pyo3::types::PyModule>) -> pyo3::PyResult<()>);
inventory::collect!(PyFnRegistrar);

/// Register a hand-written `#[pyfunction]` with the module, at its definition
/// site rather than in the `#[pymodule]`.
///
/// ```ignore
/// #[pyfunction]
/// fn my_helper() -> i64 { 7 }
/// register_pyfn!(my_helper);
/// ```
///
/// `#[pyadapter]` and `#[pyop]` already emit this for the functions they
/// generate — reach for it only when the `#[pyfunction]` is written by hand.
#[macro_export]
macro_rules! register_pyfn {
    ($f:path) => {
        // Trait-qualified so the caller's module does not need
        // `PyModuleMethods` in scope.
        $crate::inventory::submit! {
            $crate::PyFnRegistrar(|m| {
                ::pyo3::types::PyModuleMethods::add_function(
                    m,
                    ::pyo3::wrap_pyfunction!($f, m)?,
                )?;
                Ok(())
            })
        }
    };
}
