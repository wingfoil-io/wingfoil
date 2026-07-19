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

use crate::queue::TimeQueue;
use crate::time::NanoTime;
use crate::{RunFor, RunMode};

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
}

impl Kernel {
    pub fn new(run_mode: RunMode, run_for: RunFor) -> Self {
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
        }
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
                    let Some(next) = self.scheduled.next_time() else {
                        // A standalone graph's only wake-up source is the
                        // scheduled queue; with it empty no work can ever
                        // arrive, so terminate rather than spin.
                        return false;
                    };
                    // Wait for the next callback (or the end bound), like the
                    // interpreted engine's bounded wait.
                    let target = next.min(self.end_time);
                    let now = NanoTime::now();
                    if target > now {
                        std::thread::sleep(Duration::from_nanos(u64::from(target - now)));
                    }
                    self.time = NanoTime::now().max(self.time + 1);
                    let mut progressed = false;
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
