//! The shared runtime core: engine time, run bounds, the scheduled-callback
//! queue, the burst grouping type and the [`Kernel`](kernel::Kernel) that
//! drives a run.
//!
//! This is the one piece of machinery both engines share. It lives here, in
//! `wingfoil`, and the legacy `wingfoil` crate depends on this crate and
//! re-exports it — so `wingfoil::NanoTime` and [`wingfoil::NanoTime`] are
//! the *same* type, not two structurally-identical twins, and values cross the
//! engine boundary without conversion.
//!
//! The direction matters for the cutover: `crates/` is replacing the legacy tree
//! wholesale, so nothing under `crates/` may depend on `wingfoil`. Keeping the
//! shared core here means the eventual swap deletes the legacy crate outright
//! rather than having to disentangle it first.
//!
//! [`wingfoil::NanoTime`]: crate::NanoTime

pub mod burst;
pub mod kernel;
pub mod latency;
pub mod run;
/// Engine time: [`NanoTime`](time::NanoTime), a strictly-monotonic nanosecond
/// instant, plus its conversions to and from `Duration`, `NaiveDateTime` and
/// KDB+ timestamps. Re-exported at the crate root.
pub mod time;
/// The scheduled-callback queue: [`TimeQueue<T>`](time_queue::TimeQueue),
/// which backs the graph scheduler, `delay` and `feedback`. Deduplicates
/// `(value, time)` pairs by design — see its module docs before changing it.
/// Re-exported at the crate root.
pub mod time_queue;
