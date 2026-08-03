//! The burst grouping type, shared by both engines.
//!
//! A group of same-instant values delivered atomically in one cycle — never
//! coalesced / latest-wins, never dropped. The legacy `wingfoil` crate
//! re-exports this type and its constructor macro, so `wingfoil::Burst<T>`
//! and [`wingfoil::Burst<T>`](crate::Burst) are the *same* type and a
//! burst crosses the engine boundary without conversion.

use tinyvec::TinyVec;

/// A small vector optimised for single-element bursts.
///
/// In multi-threaded or async contexts, multiple values may arrive between
/// engine cycles, so incoming data is always a `Burst<T>` rather than a
/// plain `T`.
pub type Burst<T> = TinyVec<[T; 1]>;

/// Macro to create a [`Burst<T>`] with type inference.
///
/// # Examples
///
/// ```
/// # use wingfoil::burst;
/// # use wingfoil::Burst;
/// // Create an empty burst
/// let empty: Burst<i32> = burst![];
///
/// // Create a burst with one element
/// let one: Burst<i32> = burst![42];
///
/// // Create a burst with multiple elements
/// let many: Burst<i32> = burst![1, 2, 3];
/// ```
#[macro_export]
macro_rules! burst {
    () => {
        ::tinyvec::TinyVec::new()
    };
    ($($item:expr),* $(,)?) => {
        ::tinyvec::tiny_vec!($($item),*)
    };
}
