//! [`Burst<T>`]: a group of values that occur *together* — same timestamp in
//! a historical replay, or arrived-between-cycles in realtime.
//!
//! Sources never coalesce (no latest-wins, no dropped values): when several
//! values land at one instant they are delivered as one `Burst` in a single
//! cycle, so a downstream node sees every value with its grouping intact.
//!
//! This is the **same type** as the classic engine's `Burst<T>` — a
//! `tinyvec::TinyVec<[T; 1]>` (small-vector-optimised for the common
//! single-value case), re-exported from `wingfoil` so both engines agree on
//! the grouping type. Build one with the [`burst!`] macro
//! (`burst![]`, `burst![v]`, `burst![a, b, c]`).

pub use wingfoil::{Burst, burst};
