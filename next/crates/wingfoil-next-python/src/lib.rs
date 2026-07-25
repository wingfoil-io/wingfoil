//! Python interop boundary for **wingfoil-next**.
//!
//! The design objective (see `next/docs/python-interop.md`) is that users can
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

pub mod element;

pub use element::PyElement;
