mod time_queue;
mod value_at;

// `TimeQueue` is exposed (doc-hidden) for downstream engines built on top of
// wingfoil (e.g. the `wingfoil-next` op path) — delay-style ops need a time
// queue with the same dedup semantics as the engine's own scheduling.
#[doc(hidden)]
pub use time_queue::TimeQueue;
pub use value_at::ValueAt;
