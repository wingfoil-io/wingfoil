//! The minimal engine kernel: engine time, the scheduled-callback queue and
//! the run bounds, factored out so an engine can drive a graph without the
//! `dyn Node` / `Graph` machinery — node state lives in the caller's own
//! structures instead. The `begin_cycle` logic transcribes the legacy
//! engine's loop head (`Graph::advance` in the `wingfoil` crate), so a
//! kernel-driven run and the legacy engine cannot drift on timing or bounds.
//!
//! This is the engine core for both trees. It reached its present shape as
//! the residue of an ahead-of-time retrofit code generator that once lived in
//! `wingfoil::codegen` (it walked a wired legacy graph and emitted a
//! standalone static-schedule Rust runner); that generator was removed,
//! superseded by this crate's `nitro!` `compiled()` / islands path, and the
//! kernel it left behind now lives here. The `wingfoil` crate re-exports it
//! as `wingfoil::codegen` so the legacy path is unchanged.

use std::cell::Cell;
use std::time::Duration;

use crossbeam::channel::{Receiver, RecvTimeoutError, Sender, unbounded};

use crate::runtime::run::{RunFor, RunMode};
use crate::runtime::time::NanoTime;
use crate::runtime::time_queue::TimeQueue;

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

/// Clock, scheduled-callback queue and run bounds for a kernel-driven engine.
/// See the [module docs](self) for how this relates to the interpreted engine.
pub struct Kernel {
    run_mode: RunMode,
    start_time: NanoTime,
    end_time: NanoTime,
    end_cycle: u32,
    time: NanoTime,
    /// This cycle's wall-clock snap, taken **on first read** and shared by
    /// every later reader in the same cycle (see [`Kernel::wall_time`]).
    /// `None` means "not yet read this cycle". Distinct from `time`, which is
    /// source-driven (logical) in historical mode.
    ///
    /// Lazy rather than snapped in `begin_cycle` because the snap is a TSC
    /// read — ~24 ns on the `nanotime` bench — and almost no graph reads it:
    /// the only consumer in the tree is [`latency::Stamp`](crate::latency),
    /// which most graphs do not wire. Eagerly snapping put that read on every
    /// cycle of every run to serve a value nothing looked at. `Cell` (rather
    /// than a `&mut self` accessor) keeps
    /// [`Ctx::wall_time`](crate::op::Ctx::wall_time) on `&self`, so ops are
    /// unaffected.
    ///
    /// The saving is **one clock read per cycle**, which is a property of the
    /// code rather than of any measurement: a cycle in which no op calls
    /// `wall_time` now calls `NanoTime::now` zero times, where it used to call
    /// it once (twice in the realtime spin path, whose second read existed
    /// only to fill this field). Do not expect the per-cycle benches to
    /// resolve it — against a 78–307 ns/cycle baseline on a shared 4-core VM,
    /// criterion's own confidence intervals are ±10–25%, i.e. wider than the
    /// effect. A paired run there showed the interpreted tier improving at
    /// both depths (p < 0.05) with point estimates of 58 ns and 17 ns
    /// straddling the predicted ~24, and the island tier not moving in a
    /// consistent direction at all — which is the expected result, since an
    /// island still takes exactly one snap per activation (the composite reads
    /// it eagerly to share with its inner nodes, and cannot know whether any
    /// of them will look).
    wall_time: Cell<Option<NanoTime>>,
    first_cycle: bool,
    is_last_cycle: bool,
    cycles: u32,
    scheduled: TimeQueue<usize>,
    /// Indices this kernel marked dirty in the current cycle, in the order it
    /// marked them. Two jobs, both of which take an `O(N)` term off the
    /// per-cycle cost of a graph that is mostly quiet:
    ///
    /// * [`end_cycle`](Kernel::end_cycle) clears exactly these flags instead of
    ///   memsetting the whole `dirty` array — the array is as long as the graph,
    ///   so the clear used to scale with graph size on every cycle;
    /// * [`due`](Kernel::due) hands an engine the frontier directly, so it can
    ///   seed its work set from the nodes that actually came due rather than
    ///   scanning every callback-activated node in the graph to ask.
    ///
    /// Entries are unique: a marker only records an index whose flag it
    /// actually flipped.
    due: Vec<usize>,
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
            wall_time: Cell::new(None),
            first_cycle: true,
            is_last_cycle: false,
            cycles: 0,
            scheduled: TimeQueue::new(),
            due: Vec::new(),
            ready,
            spin: false,
        }
    }

    /// Mark `index` dirty, recording it in [`due`](Self::due) if this is the
    /// call that flipped the flag. Out-of-range indices (a misbehaving waker)
    /// are ignored.
    ///
    /// Returns whether `index` was **in range** — deliberately not whether the
    /// flag changed. It is the caller's `progressed` signal, and a node marked
    /// twice in one cycle (woken twice, or scheduled at two times that both
    /// come due) is still progress: the cycle must run.
    fn mark(&mut self, index: usize, dirty: &mut [bool]) -> bool {
        match dirty.get_mut(index) {
            Some(d) => {
                if !*d {
                    *d = true;
                    self.due.push(index);
                }
                true
            }
            None => false,
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
        if self.ready.is_none() {
            return false;
        }
        let mut any = false;
        // Collected first so `mark` can take `&mut self` while the receiver
        // borrow is released.
        while let Some(ix) = self.ready.as_ref().and_then(|rx| rx.try_recv().ok()) {
            any |= self.mark(ix, dirty);
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

    /// This cycle's wall-clock snap: taken on the **first** call in a cycle
    /// and returned unchanged to every later caller in that same cycle, so
    /// all readers agree on one instant (which is what separates this from
    /// [`Ctx::wall_time_precise`](crate::op::Ctx::wall_time_precise)).
    ///
    /// The same in both realtime and historical mode — always a wall-clock
    /// snap — so latency stamping and perf telemetry mean "wall-clock time
    /// spent" regardless of run mode. Mirrors legacy `GraphState::wall_time`.
    /// Never use for business logic (use [`time`](Self::time) for
    /// deterministic replay).
    ///
    /// A cycle that never calls this never reads the clock at all — see the
    /// field's comment for why that is worth the `Cell`.
    pub fn wall_time(&self) -> NanoTime {
        match self.wall_time.get() {
            Some(snap) => snap,
            None => {
                let snap = NanoTime::now();
                self.wall_time.set(Some(snap));
                snap
            }
        }
    }

    /// The run mode (realtime vs historical) — lets a source op choose
    /// wall-clock (waker-driven) or graph-clock (schedule-driven) behaviour.
    pub fn run_mode(&self) -> RunMode {
        self.run_mode
    }

    /// The run bound this kernel was built with, reconstructed from the derived
    /// end-time / end-cycle sentinels (the mirror of [`run_mode`](Self::run_mode)).
    /// Lets a source run a *sub-graph* under the same bound — e.g. a worker-thread
    /// producer (`spawn`) whose graph must stop when the driving graph does.
    pub fn run_for(&self) -> RunFor {
        if self.end_cycle != u32::MAX {
            RunFor::Cycles(self.end_cycle)
        } else if self.end_time != NanoTime::MAX {
            RunFor::Duration(Duration::from_nanos(
                u64::from(self.end_time).saturating_sub(u64::from(self.start_time)),
            ))
        } else {
            RunFor::Forever
        }
    }

    /// Whether this is the final cycle of the run (the run bound is about to
    /// stop it). Ops that buffer and flush on a boundary (window, buffer) use
    /// this to flush their pending contents before the run ends.
    pub fn is_last_cycle(&self) -> bool {
        self.is_last_cycle
    }

    /// Schedule node `index` to be marked dirty at `at`.
    pub fn schedule(&mut self, index: usize, at: NanoTime) {
        self.scheduled.push(index, at);
    }

    /// Advance to the next cycle: check the run bounds, advance engine time
    /// and mark due callbacks in `dirty`. Returns `false` when the run is
    /// complete. Transcribes `Graph::advance` together with the
    /// historical/realtime callback processing (including external wake-ups
    /// via [`KernelWaker`]).
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
                        progressed |= self.mark(ix, dirty);
                    }
                    if !progressed {
                        return false;
                    }
                    self.wall_time.set(None);
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
                            self.mark(ix, dirty);
                        }
                        self.wall_time.set(None);
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
                                    let woken = match &self.ready {
                                        Some(rx) => match rx.recv_timeout(timeout) {
                                            Ok(ix) => Some(ix),
                                            Err(RecvTimeoutError::Timeout)
                                            | Err(RecvTimeoutError::Disconnected) => None,
                                        },
                                        None => {
                                            std::thread::sleep(timeout);
                                            None
                                        }
                                    };
                                    if let Some(ix) = woken {
                                        progressed |= self.mark(ix, dirty);
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
                                        progressed |= self.mark(ix, dirty);
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
                        progressed |= self.mark(ix, dirty);
                    }
                    if progressed {
                        self.wall_time.set(None);
                        return true;
                    }
                    // Nothing due yet (e.g. woke at the end bound): re-check
                    // the bounds and wait again.
                }
            }
        }
    }

    /// The nodes this kernel marked dirty in the cycle
    /// [`begin_cycle`](Self::begin_cycle) just opened — the activation frontier,
    /// in the order it was marked, with no duplicates.
    ///
    /// This is the same information as the `true` entries of the `dirty` slice,
    /// handed over as a list so an engine need not go looking for them. The
    /// interpreted engine seeds its work set from here; the alternative is to
    /// walk every callback-activated node in the graph asking "were you marked?",
    /// which costs a cycle in proportion to how many timers the graph *has*
    /// rather than how many just fired.
    ///
    /// Valid until the matching [`end_cycle`](Self::end_cycle), which clears it.
    pub fn due(&self) -> &[usize] {
        &self.due
    }

    /// Finish the current cycle: clear the dirty flags.
    ///
    /// Clears exactly the flags this kernel set (see [`due`](Self::due)) rather
    /// than sweeping the whole slice, so a quiet graph does not pay a memset of
    /// its own size on every cycle. `dirty` must therefore be the same slice
    /// `begin_cycle` marked — which it is for every engine in the tree, all of
    /// which keep one long-lived array per run.
    pub fn end_cycle(&mut self, dirty: &mut [bool]) {
        for &ix in &self.due {
            if let Some(d) = dirty.get_mut(ix) {
                *d = false;
            }
        }
        self.due.clear();
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
