//! `pyop_fn!` — generate a Python-callable op from a Rust step function.
//!
//! pyo3 forbids adding `#[pymethods]` to a *foreign* pyclass, so a user op in
//! another crate cannot become `stream.myop()`. The feasible shape — the same
//! one polars expression plugins use — is a free function: `module.myop(stream
//! [, cfg])`. `pyop_fn!` generates exactly that `#[pyfunction]`, wiring the op onto
//! the stream via [`PyStream::wire_op1`](crate::PyStream::wire_op1) so only the
//! edges convert between [`PyElement`](crate::PyElement) and the op's concrete
//! types.
//!
//! This declarative form covers the common **stateless single-input** op (with
//! or without one config argument). Stateful / multi-input ops call
//! `wire_op1` (or the object form) directly; a `#[pyop]` *proc* macro that reads
//! an `Op` impl and derives this is the planned sugar on top.
//!
//! ```ignore
//! use wingfoil_python::{pyop_fn, Tick};
//!
//! pyop_fn! {
//!     /// Multiply each value by `factor`.
//!     fn scale(factor: f64): f64 => f64 = |cfg, _state, a, _ctx| Ok(Tick::Value(*a * *cfg))
//! }
//! pyop_fn! {
//!     /// Negate each value.
//!     fn negate(): f64 => f64 = |_cfg, _state, a, _ctx| Ok(Tick::Value(-*a))
//! }
//! ```
//!
//! The generated function still needs adding to the module
//! (`m.add_function(wrap_pyfunction!(scale, m)?)?`).

/// Generate a `#[pyfunction]` for a stateless single-input op. See the module
/// docs for the two forms (with / without a config argument).
#[macro_export]
macro_rules! pyop_fn {
    (
        $(#[$meta:meta])*
        fn $name:ident ( $cfg:ident : $cfg_ty:ty ) : $in_ty:ty => $out_ty:ty = $body:expr
    ) => {
        $(#[$meta])*
        #[pyo3::pyfunction]
        fn $name(
            stream: pyo3::PyRef<'_, $crate::Stream>,
            $cfg: $cfg_ty,
        ) -> $crate::Stream {
            $crate::Stream::from(stream.object().wire_op1::<$in_ty, _, _, $out_ty, _, _>(
                stringify!($name),
                $crate::Activation::NONE,
                $cfg,
                || (),
                $body,
            ))
        }
    };
    (
        $(#[$meta:meta])*
        fn $name:ident () : $in_ty:ty => $out_ty:ty = $body:expr
    ) => {
        $(#[$meta])*
        #[pyo3::pyfunction]
        fn $name(stream: pyo3::PyRef<'_, $crate::Stream>) -> $crate::Stream {
            $crate::Stream::from(stream.object().wire_op1::<$in_ty, _, _, $out_ty, _, _>(
                stringify!($name),
                $crate::Activation::NONE,
                (),
                || (),
                $body,
            ))
        }
    };
}
