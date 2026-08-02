//! The shared runtime core: engine time, run bounds, the scheduled-callback
//! queue, the burst grouping type and the [`Kernel`](kernel::Kernel) that
//! drives a run.
//!
//! This is the one piece of machinery both engines share. It lives here, in
//! `wingfoil-next`, and the legacy `wingfoil` crate depends on this crate and
//! re-exports it — so `wingfoil::NanoTime` and [`wingfoil_next::NanoTime`] are
//! the *same* type, not two structurally-identical twins, and values cross the
//! engine boundary without conversion.
//!
//! The direction matters for the cutover: `next/` is replacing the legacy tree
//! wholesale, so nothing under `next/` may depend on `wingfoil`. Keeping the
//! shared core here means the eventual swap deletes the legacy crate outright
//! rather than having to disentangle it first.
//!
//! [`wingfoil_next::NanoTime`]: crate::NanoTime

pub mod burst;
pub mod kernel;
pub mod latency;
pub mod run;
pub mod time;
pub mod time_queue;
