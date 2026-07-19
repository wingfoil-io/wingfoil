mod time_queue;
mod value_at;

// `TimeQueue` is exposed (doc-hidden) for engine prototypes built on the
// public kernel (`codegen::Kernel`) — e.g. delay-style ops need a time queue
// with the same dedup semantics as the engine's own scheduling.
#[doc(hidden)]
pub use time_queue::TimeQueue;
pub use value_at::ValueAt;
