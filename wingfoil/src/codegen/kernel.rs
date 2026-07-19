//! The minimal engine kernel driven by standalone generated runners
//! ([`generate_standalone`](crate::codegen::generate_standalone)).
//!
//! A standalone runner has no `dyn Node`s and no [`Graph`](crate::Graph) —
//! node state lives in monomorphized locals inside the generated function.
//! What must remain at runtime is exactly this: engine time, the scheduled
//! callback queue, and the run bounds. The `begin_cycle` logic transcribes
//! the interpreted engine's loop head ([`Graph::advance`]) minus the parts a
//! standalone graph cannot have (ready callbacks from threaded sources,
//! always-callbacks, dynamic graph changes) — the standalone generator only
//! accepts node kinds that use scheduled callbacks.
//!
//! [`Graph::advance`]: crate::Graph

use std::time::Duration;

use crossbeam::channel::{Receiver, RecvTimeoutError, Sender, unbounded};

use crate::queue::TimeQueue;
use crate::time::NanoTime;
use crate::{RunFor, RunMode};

/// Wakes a realtime [`Kernel`] from another thread, marking a node dirty —
/// the kernel-level equivalent of the interpreted engine's `ReadyNotifier`.
/// Cheap to clone; hand one to each producer thread / async task.
#[derive(Clone)]
pub struct KernelWaker {
    sender: Sender<usize>,
}

impl KernelWaker {
    /// Mark `node` dirty and wake the kernel if it is waiting. Returns false
    /// if the kernel (and its receiver) are gone — producers can use this to
    /// stop.
    pub fn wake(&self, node: usize) -> bool {
        self.sender.send(node).is_ok()
    }
}

/// The receiving half of a [`waker_channel`], to be handed to
/// [`Kernel::with_ready`].
pub type ReadyReceiver = Receiver<usize>;

/// Create a waker/receiver pair for external (threaded/async) sources. Hand
/// the [`KernelWaker`] to producers and the receiver to
/// [`Kernel::with_ready`].
pub fn waker_channel() -> (KernelWaker, ReadyReceiver) {
    let (sender, receiver) = unbounded();
    (KernelWaker { sender }, receiver)
}

/// Clock, scheduled-callback queue and run bounds for a standalone generated
/// runner. See the [module docs](self) for how this relates to the
/// interpreted engine.
pub struct Kernel {
    run_mode: RunMode,
    start_time: NanoTime,
    end_time: NanoTime,
    end_cycle: u32,
    time: NanoTime,
    first_cycle: bool,
    is_last_cycle: bool,
    cycles: u32,
    scheduled: TimeQueue<usize>,
    /// External wake-ups from [`KernelWaker`]s, realtime mode only. Mirrors
    /// the interpreted engine's ready-callback channel.
    ready: Option<Receiver<usize>>,
    /// Busy-spin mode: realtime `begin_cycle` never parks — it drains
    /// wake-ups non-blockingly, advances time to now, and starts a cycle
    /// unconditionally. Set by engines running graphs with always-active
    /// (busy-poll) ops, which need every cycle regardless of callbacks.
    spin: bool,
}

impl Kernel {
    pub fn new(run_mode: RunMode, run_for: RunFor) -> Self {
        Self::build(run_mode, run_for, None)
    }

    /// A kernel that can also be woken by external threads through the
    /// receiver from [`waker_channel`]. Realtime mode only — external
    /// wake-ups have no place in a deterministic historical replay (the
    /// interpreted engine errors on them; callers of this API should reject
    /// historical runs for graphs with external sources).
    pub fn with_ready(run_mode: RunMode, run_for: RunFor, ready: Receiver<usize>) -> Self {
        Self::build(run_mode, run_for, Some(ready))
    }

    fn build(run_mode: RunMode, run_for: RunFor, ready: Option<Receiver<usize>>) -> Self {
        let start_time = run_mode.start_time();
        // Mirrors `Graph::resolve_start_end`: MAX sentinels for bounds that
        // don't apply.
        let mut end_time = NanoTime::MAX;
        let mut end_cycle = u32::MAX;
        match run_for {
            RunFor::Duration(duration) => end_time = start_time + duration,
            RunFor::Cycles(cycles) => end_cycle = cycles,
            RunFor::Forever => {}
        }
        Self {
            run_mode,
            start_time,
            end_time,
            end_cycle,
            time: NanoTime::ZERO,
            first_cycle: true,
            is_last_cycle: false,
            cycles: 0,
            scheduled: TimeQueue::new(),
            ready,
            spin: false,
        }
    }

    /// Enable busy-spin mode (see the `spin` field). Meaningful for
    /// realtime runs only; a historical run ignores it (there is nothing to
    /// poll in a replay — engines reject always-active ops there).
    pub fn set_spin(&mut self, spin: bool) {
        self.spin = spin;
    }

    /// Drain pending external wake-ups into `dirty`. Out-of-range indices
    /// (a misbehaving waker) are ignored.
    fn drain_ready(&mut self, dirty: &mut [bool]) -> bool {
        let Some(rx) = &self.ready else {
            return false;
        };
        let mut any = false;
        while let Ok(ix) = rx.try_recv() {
            if let Some(d) = dirty.get_mut(ix) {
                *d = true;
                any = true;
            }
        }
        any
    }

    /// The run's start time (wall clock for realtime runs).
    pub fn start_time(&self) -> NanoTime {
        self.start_time
    }

    /// Current engine time.
    pub fn time(&self) -> NanoTime {
        self.time
    }

    /// Schedule node `index` to be marked dirty at `at`.
    pub fn schedule(&mut self, index: usize, at: NanoTime) {
        self.scheduled.push(index, at);
    }

    /// Advance to the next cycle: check the run bounds, advance engine time
    /// and mark due callbacks in `dirty`. Returns `false` when the run is
    /// complete. Transcribes `Graph::advance` + the historical/realtime
    /// callback processing, minus ready callbacks (standalone graphs have no
    /// threaded sources).
    pub fn begin_cycle(&mut self, dirty: &mut [bool]) -> bool {
        loop {
            // Bounds handling is identical to the interpreted engine: the
            // cycle-count bound terminates immediately; the time bound is
            // gated on `is_last_cycle` so the final scheduled cycle runs.
            let cycles_done = self.cycles >= self.end_cycle;
            let time_done = self.time >= self.end_time;
            if cycles_done || (self.is_last_cycle && time_done) {
                return false;
            }
            if !self.is_last_cycle && (self.cycles + 1 >= self.end_cycle || time_done) {
                self.is_last_cycle = true;
            }
            match self.run_mode {
                RunMode::HistoricalFrom(_) => {
                    let Some(next) = self.scheduled.next_time() else {
                        // No further work: terminate early, as the interpreted
                        // engine does for a historical run with nothing queued.
                        return false;
                    };
                    self.time = if self.first_cycle {
                        self.first_cycle = false;
                        // First cycle fires at the callback's own time.
                        next
                    } else {
                        // Strict monotonic progression: bump to prev+1.
                        next.max(self.time + 1)
                    };
                    let mut progressed = false;
                    while let Some(ix) = self.scheduled.pop_if_pending(self.time) {
                        dirty[ix] = true;
                        progressed = true;
                    }
                    if !progressed {
                        return false;
                    }
                    return true;
                }
                RunMode::RealTime => {
                    // External wake-ups first — they cost no waiting.
                    let mut progressed = self.drain_ready(dirty);
                    if self.spin {
                        // Busy-spin: never park. Advance to now, mark due
                        // callbacks, and start the cycle unconditionally —
                        // always-active ops need it even with nothing due.
                        self.time = NanoTime::now().max(self.time + 1);
                        while let Some(ix) = self.scheduled.pop_if_pending(self.time) {
                            dirty[ix] = true;
                        }
                        return true;
                    }
                    if !progressed {
                        match self.scheduled.next_time() {
                            Some(next) => {
                                // Wait for the next callback, the end bound,
                                // or an external wake-up — whichever first.
                                let target = next.min(self.end_time);
                                let now = NanoTime::now();
                                if target > now {
                                    let timeout = Duration::from_nanos(u64::from(target - now));
                                    match &self.ready {
                                        Some(rx) => match rx.recv_timeout(timeout) {
                                            Ok(ix) => {
                                                if let Some(d) = dirty.get_mut(ix) {
                                                    *d = true;
                                                    progressed = true;
                                                }
                                            }
                                            Err(RecvTimeoutError::Timeout)
                                            | Err(RecvTimeoutError::Disconnected) => {}
                                        },
                                        None => std::thread::sleep(timeout),
                                    }
                                }
                            }
                            None => {
                                // Nothing scheduled: only external wake-ups
                                // can produce work. Without a ready channel,
                                // terminate rather than spin.
                                let Some(rx) = &self.ready else {
                                    return false;
                                };
                                let woken = if self.end_time == NanoTime::MAX {
                                    rx.recv().ok()
                                } else {
                                    let now = NanoTime::now();
                                    if self.end_time <= now {
                                        None
                                    } else {
                                        let timeout =
                                            Duration::from_nanos(u64::from(self.end_time - now));
                                        match rx.recv_timeout(timeout) {
                                            Ok(ix) => Some(ix),
                                            Err(RecvTimeoutError::Timeout) => None,
                                            // All wakers dropped and nothing
                                            // scheduled: no work can ever
                                            // arrive.
                                            Err(RecvTimeoutError::Disconnected) => {
                                                return false;
                                            }
                                        }
                                    }
                                };
                                match woken {
                                    Some(ix) => {
                                        if let Some(d) = dirty.get_mut(ix) {
                                            *d = true;
                                            progressed = true;
                                        }
                                    }
                                    // All wakers dropped (recv Err) or the
                                    // end bound passed with nothing queued.
                                    None if self.end_time == NanoTime::MAX => return false,
                                    None => {}
                                }
                            }
                        }
                    }
                    self.time = NanoTime::now().max(self.time + 1);
                    while let Some(ix) = self.scheduled.pop_if_pending(self.time) {
                        dirty[ix] = true;
                        progressed = true;
                    }
                    if progressed {
                        return true;
                    }
                    // Nothing due yet (e.g. woke at the end bound): re-check
                    // the bounds and wait again.
                }
            }
        }
    }

    /// Finish the current cycle: clear the dirty flags.
    pub fn end_cycle(&mut self, dirty: &mut [bool]) {
        for d in dirty.iter_mut() {
            *d = false;
        }
        self.cycles += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_kernel_advances_like_the_engine() {
        // A 100ns ticker equivalent: schedule at start, reschedule on fire.
        let mut k = Kernel::new(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3));
        let mut dirty = [false; 1];
        k.schedule(0, k.start_time());
        let mut times = Vec::new();
        let mut at: Option<NanoTime> = None;
        while k.begin_cycle(&mut dirty) {
            assert!(dirty[0]);
            let next = match at {
                Some(t) => t + NanoTime::new(100),
                None => k.time() + NanoTime::new(100),
            };
            at = Some(next);
            k.schedule(0, next);
            times.push(k.time());
            k.end_cycle(&mut dirty);
        }
        // Matches the interpreted engine: first cycle at t=0, then strict
        // 100ns steps (see tick.rs / graph.rs historical semantics).
        assert_eq!(
            vec![NanoTime::new(0), NanoTime::new(100), NanoTime::new(200)],
            times
        );
    }

    #[test]
    fn historical_kernel_terminates_when_queue_empties() {
        // A constant equivalent: one callback, never rescheduled.
        let mut k = Kernel::new(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
        let mut dirty = [false; 1];
        k.schedule(0, k.start_time());
        let mut cycles = 0;
        while k.begin_cycle(&mut dirty) {
            cycles += 1;
            k.end_cycle(&mut dirty);
        }
        assert_eq!(1, cycles);
    }

    #[test]
    fn cycles_zero_runs_no_cycles() {
        let mut k = Kernel::new(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(0));
        let mut dirty = [false; 1];
        k.schedule(0, k.start_time());
        assert!(!k.begin_cycle(&mut dirty));
    }

    #[test]
    fn duration_bound_runs_final_cycle() {
        // 100ns ticker with a 250ns duration bound. The interpreted engine
        // crosses the bound at t=300, flags the *next* cycle as the last and
        // still executes it (is_last_cycle gating) — so t=400 fires too, then
        // the run stops. See `merge_emits_from_both_streams` for the same
        // behaviour on the interpreted engine.
        let mut k = Kernel::new(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(Duration::from_nanos(250)),
        );
        let mut dirty = [false; 1];
        k.schedule(0, k.start_time());
        let mut times = Vec::new();
        let mut at: Option<NanoTime> = None;
        while k.begin_cycle(&mut dirty) {
            let next = match at {
                Some(t) => t + NanoTime::new(100),
                None => k.time() + NanoTime::new(100),
            };
            at = Some(next);
            k.schedule(0, next);
            times.push(k.time());
            k.end_cycle(&mut dirty);
        }
        assert_eq!(
            vec![
                NanoTime::new(0),
                NanoTime::new(100),
                NanoTime::new(200),
                NanoTime::new(300),
                NanoTime::new(400)
            ],
            times
        );
    }

    #[test]
    fn realtime_kernel_wakes_on_external_events_and_terminates() {
        // One wake, then the waker drops: exactly one cycle fires, then the
        // kernel sees the disconnected channel with nothing scheduled and
        // terminates (even under RunFor::Forever).
        let (waker, ready) = waker_channel();
        let mut k = Kernel::with_ready(RunMode::RealTime, RunFor::Forever, ready);
        let mut dirty = [false; 1];
        let producer = std::thread::spawn(move || {
            waker.wake(0);
        });
        let mut fires = 0;
        while k.begin_cycle(&mut dirty) {
            if dirty[0] {
                fires += 1;
            }
            k.end_cycle(&mut dirty);
        }
        producer.join().expect("producer thread");
        assert_eq!(1, fires);
    }

    #[test]
    fn realtime_kernel_fires_scheduled_callbacks() {
        let mut k = Kernel::new(RunMode::RealTime, RunFor::Cycles(3));
        let mut dirty = [false; 1];
        k.schedule(0, k.start_time());
        let mut fires = 0;
        let mut at: Option<NanoTime> = None;
        while k.begin_cycle(&mut dirty) {
            if dirty[0] {
                fires += 1;
                let interval = NanoTime::new(1_000_000); // 1ms
                let next = match at {
                    Some(t) => t + interval,
                    None => k.time() + interval,
                };
                at = Some(next);
                k.schedule(0, next);
            }
            k.end_cycle(&mut dirty);
        }
        assert_eq!(3, fires);
    }
}
