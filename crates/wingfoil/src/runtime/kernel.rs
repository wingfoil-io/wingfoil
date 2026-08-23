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

/// How a realtime kernel waits out the gap to the next scheduled callback.
///
/// Only meaningful for a **realtime, non-spin** run: a historical replay does
/// not wait at all, and a graph with an [`Activation::ALWAYS`](crate::op::Activation)
/// op is already spinning every cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TimerPolicy {
    /// Sleep until the deadline and let the OS wake us. Costs no CPU while
    /// waiting; the wake lands wherever the scheduler puts it, which on Linux
    /// is the deadline plus timer slack and run-queue delay — tens of
    /// microseconds on an idle machine, and unbounded under load.
    ///
    /// The right default: most graphs would rather have the core than the
    /// microseconds.
    #[default]
    Park,
    /// Sleep until `guard` *before* the deadline, then busy-spin the remainder.
    ///
    /// Trades one core for the length of the guard window against the
    /// scheduler's wake-up error, which is what a timer-driven strategy — a
    /// quote refresh, a periodic hedge, a scheduled cancel — actually pays. The
    /// guard needs to exceed the worst sleep overshoot you care about;
    /// 100–500 µs is the usual range, and there is no point setting it larger
    /// than the tick interval.
    ///
    /// The spin still drains external wake-ups, so a `KernelWaker` firing
    /// inside the guard window is picked up immediately rather than at the
    /// deadline.
    SpinAhead(Duration),
}

impl TimerPolicy {
    /// The guard window in nanoseconds — zero for [`Park`](Self::Park).
    fn guard_ns(&self) -> u64 {
        match self {
            TimerPolicy::Park => 0,
            // Saturating: a guard beyond `u64::MAX` nanoseconds (584 years) is
            // a misconfiguration, and clamping it turns the whole wait into a
            // spin rather than wrapping to no guard at all.
            TimerPolicy::SpinAhead(d) => u64::try_from(d.as_nanos()).unwrap_or(u64::MAX),
        }
    }
}

/// Clock, scheduled-callback queue and run bounds for a kernel-driven engine.
/// See the [module docs](self) for how this relates to the interpreted engine.
pub struct Kernel {
    run_mode: RunMode,
    start_time: NanoTime,
    end_time: NanoTime,
    /// The cycle bound, or `None` for a run that is not cycle-bounded
    /// ([`RunFor::Forever`] / [`RunFor::Duration`]).
    ///
    /// **Deliberately an `Option`, not a `u32::MAX` sentinel.** It used to be
    /// the sentinel, and because the bound test is `cycles >= end_cycle` that
    /// made "no cycle bound" indistinguishable from "a bound of 4,294,967,295
    /// cycles": a `RunFor::Forever` run *terminated* — cleanly, returning
    /// `Ok(())` — once the counter reached it. A busy-spin realtime graph (an
    /// `Activation::ALWAYS` poll source: aeron `Spin`, iceoryx2 `Spin`, FIX
    /// `AlwaysSpin`) turns ~20M cycles/second on a modest machine, so
    /// "forever" was **3.6 minutes**, and a `RunFor::Duration(8 hours)` stopped
    /// at the same point because the duration bound left the sentinel in place.
    /// See `realtime_forever_is_not_secretly_cycle_bounded` below.
    end_cycle: Option<u64>,
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
    /// Cycles completed so far. `u64` rather than `u32` for the same reason
    /// [`end_cycle`](Self::end_cycle) is an `Option`: at busy-spin rates a
    /// `u32` wraps within minutes, and a counter that wraps under a
    /// long-running graph is a defect whether or not anything reads it.
    cycles: u64,
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
    /// How the realtime non-spin wait is served. See [`TimerPolicy`].
    timer: TimerPolicy,
}

impl Kernel {
    /// A kernel driving a run in `run_mode`, bounded by `run_for`, with no
    /// external wake-up channel — the right constructor for a graph whose
    /// sources are all timers or replayed values. Use
    /// [`with_ready`](Kernel::with_ready) when another thread must wake it.
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
        // Mirrors `Graph::resolve_start_end`. `end_time` keeps its `MAX`
        // sentinel — `NanoTime` is nanoseconds in a `u64`, so that bound is
        // ~584 years away and cannot be reached by a real run. The *cycle*
        // bound cannot be expressed that way (see `end_cycle`), so it is an
        // `Option`.
        let mut end_time = NanoTime::MAX;
        let mut end_cycle = None;
        match run_for {
            RunFor::Duration(duration) => end_time = start_time + duration,
            RunFor::Cycles(cycles) => end_cycle = Some(u64::from(cycles)),
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
            timer: TimerPolicy::default(),
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
    #[inline]
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

    /// Select how the realtime wait to the next scheduled callback is served
    /// (see [`TimerPolicy`]). Ignored by a historical run, which never waits,
    /// and by a spin run, which never parks.
    pub fn set_timer_policy(&mut self, timer: TimerPolicy) {
        self.timer = timer;
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
    #[inline]
    pub fn start_time(&self) -> NanoTime {
        self.start_time
    }

    /// Current engine time.
    #[inline]
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
    #[inline]
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
    #[inline]
    pub fn run_mode(&self) -> RunMode {
        self.run_mode
    }

    /// The run bound this kernel was built with, reconstructed from the derived
    /// end-time / end-cycle sentinels (the mirror of [`run_mode`](Self::run_mode)).
    /// Lets a source run a *sub-graph* under the same bound — e.g. a worker-thread
    /// producer (`spawn`) whose graph must stop when the driving graph does.
    pub fn run_for(&self) -> RunFor {
        if let Some(end_cycle) = self.end_cycle {
            // Lossless: `end_cycle` is only ever `Some` from a `RunFor::Cycles`,
            // whose payload is a `u32`.
            RunFor::Cycles(end_cycle as u32)
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
    #[inline]
    pub fn is_last_cycle(&self) -> bool {
        self.is_last_cycle
    }

    /// Cycles completed so far in this run. Counts without bound — a
    /// long-lived realtime graph is expected to exceed `u32` (a busy-spin run
    /// does so in minutes).
    pub fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Schedule node `index` to be marked dirty at `at`.
    #[inline]
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
            // An unbounded run has no cycle bound at all — `None` never
            // compares done, where the old `u32::MAX` sentinel eventually did.
            let cycles_done = self.end_cycle.is_some_and(|end| self.cycles >= end);
            let time_done = self.time >= self.end_time;
            if cycles_done || (self.is_last_cycle && time_done) {
                return false;
            }
            let last_cycle_of_bound = self.end_cycle.is_some_and(|end| self.cycles + 1 >= end);
            if !self.is_last_cycle && (last_cycle_of_bound || time_done) {
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
                                    // Under `TimerPolicy::SpinAhead(guard)` the
                                    // sleep stops `guard` short of the deadline
                                    // and the last stretch is spun out below, so
                                    // the wake lands on the deadline instead of
                                    // wherever the scheduler happened to put it.
                                    // `Park` leaves `guard` at zero, making
                                    // `park_until == target` and the whole block
                                    // byte-for-byte the previous behaviour.
                                    let guard = self.timer.guard_ns();
                                    let park_until =
                                        NanoTime::from(u64::from(target).saturating_sub(guard))
                                            .max(now);
                                    // `park` stays true only when nothing has
                                    // actually waited out the timeout yet.
                                    let mut park = true;
                                    let mut woken = None;
                                    if park_until > now
                                        && let Some(rx) = &self.ready
                                    {
                                        let timeout =
                                            Duration::from_nanos(u64::from(park_until - now));
                                        match rx.recv_timeout(timeout) {
                                            Ok(ix) => {
                                                woken = Some(ix);
                                                park = false;
                                            }
                                            Err(RecvTimeoutError::Timeout) => park = false,
                                            // Every waker has been dropped, so no
                                            // external wake-up can ever arrive
                                            // again — but callbacks are still
                                            // queued, so the run goes on. Retire
                                            // the receiver rather than keep polling
                                            // it: `recv_timeout` returns
                                            // *immediately* on a disconnected
                                            // channel, so leaving it in place turns
                                            // every later wait into a busy spin at
                                            // 100% of a core until the next
                                            // callback comes due. Nothing is lost —
                                            // `Disconnected` is only reported once
                                            // the channel is also empty — and this
                                            // one returned early, so `park` stands
                                            // and the sleep below serves the wait.
                                            Err(RecvTimeoutError::Disconnected) => {
                                                self.ready = None;
                                            }
                                        }
                                    }
                                    if park {
                                        let now = NanoTime::now();
                                        if park_until > now {
                                            std::thread::sleep(Duration::from_nanos(u64::from(
                                                park_until - now,
                                            )));
                                        }
                                    }
                                    // Close the guard window by spinning. Only
                                    // reached under `SpinAhead` (`Park` leaves
                                    // `park_until == target`, so the loop's
                                    // condition is already false) and only while
                                    // there is nothing to do — an external
                                    // wake-up during the window is taken
                                    // immediately rather than made to wait for
                                    // the deadline.
                                    if woken.is_none() && guard > 0 {
                                        while NanoTime::now() < target {
                                            if let Some(rx) = &self.ready
                                                && let Ok(ix) = rx.try_recv()
                                            {
                                                woken = Some(ix);
                                                break;
                                            }
                                            std::hint::spin_loop();
                                        }
                                    }
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
    #[inline]
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

    /// **`RunFor::Forever` must not have a cycle bound.**
    ///
    /// The bound used to be a `u32::MAX` sentinel tested with `cycles >=
    /// end_cycle`, which made "unbounded" identical to "bounded at
    /// 4,294,967,295". A `RunFor::Forever` run therefore *ended* — returning
    /// `Ok(())`, with every `stop`/`teardown` hook firing as though the bound
    /// had been reached — once the counter got there. That is ~3.6 minutes for
    /// a busy-spin realtime graph (measured at ~20M cycles/s on a 4-core VM),
    /// which is exactly the shape a latency-critical ingest path has: aeron
    /// `Spin`, iceoryx2 `Spin`, FIX `AlwaysSpin`.
    ///
    /// Driving 4.29 billion real cycles takes minutes, so the test seeds the
    /// counter past the old sentinel instead and asserts the kernel keeps
    /// going. That is the whole defect: what the counter *reads* must not
    /// terminate an unbounded run.
    #[test]
    fn realtime_forever_is_not_secretly_cycle_bounded() {
        let mut k = Kernel::new(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
        let mut dirty = [false; 1];
        // Straight past where the old sentinel stopped the run.
        k.cycles = u64::from(u32::MAX);
        k.schedule(0, k.start_time());

        assert!(
            k.begin_cycle(&mut dirty),
            "RunFor::Forever stopped at {} cycles — it must not be cycle-bounded",
            k.cycles
        );
        assert!(!k.is_last_cycle(), "an unbounded run has no last cycle");
        k.end_cycle(&mut dirty);
        assert_eq!(u64::from(u32::MAX) + 1, k.cycles(), "the counter is a u64");
    }

    /// The same for a **duration**-bounded run: `RunFor::Duration` left the
    /// cycle sentinel in place, so an 8-hour session bound also stopped at
    /// 4.29 billion cycles — well before its own bound.
    #[test]
    fn duration_bound_does_not_impose_a_cycle_bound() {
        let mut k = Kernel::new(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(Duration::from_secs(8 * 60 * 60)),
        );
        let mut dirty = [false; 1];
        k.cycles = u64::from(u32::MAX);
        k.schedule(0, k.start_time());

        assert!(
            k.begin_cycle(&mut dirty),
            "a duration-bounded run stopped on a cycle count it never asked for"
        );
        assert!(
            !k.is_last_cycle(),
            "the duration bound is hours away; this is not the last cycle"
        );
    }

    /// The other direction — widening the counter must not weaken a bound that
    /// *was* asked for. `RunFor::Cycles(n)` still stops at exactly `n`, and
    /// still round-trips through [`Kernel::run_for`].
    #[test]
    fn explicit_cycle_bound_still_stops_exactly_at_the_bound() {
        let mut k = Kernel::new(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3));
        assert!(matches!(k.run_for(), RunFor::Cycles(3)));
        let mut dirty = [false; 1];
        k.schedule(0, k.start_time());
        let mut cycles = 0;
        let mut at = k.start_time();
        while k.begin_cycle(&mut dirty) {
            at = at + NanoTime::new(100);
            k.schedule(0, at);
            cycles += 1;
            k.end_cycle(&mut dirty);
        }
        assert_eq!(3, cycles);
        assert_eq!(3, k.cycles());
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

    /// A realtime kernel whose wakers have all been dropped must still **park**
    /// between scheduled callbacks, not spin.
    ///
    /// `recv_timeout` on a disconnected channel returns instantly, so a kernel
    /// that kept polling it burned a full core between ticks — a graph whose
    /// channel/external producers had finished (dropping their senders without
    /// an explicit `close()`) while a ticker kept running. Thread CPU time is
    /// the direct observable: waiting out a 300 ms callback must cost ~0 ms of
    /// CPU, where the spin cost ~300 ms.
    /// This thread's user+system time in clock ticks, from
    /// `/proc/thread-self/stat` (fields 14 and 15, after the parenthesised comm
    /// field). Ticks are 10 ms on every mainstream configuration. Shared by the
    /// two tests that distinguish parking from spinning by what it costs.
    #[cfg(target_os = "linux")]
    fn thread_cpu_ticks() -> u64 {
        let stat =
            std::fs::read_to_string("/proc/thread-self/stat").expect("procfs is mounted on linux");
        // The comm field is parenthesised and may itself contain spaces, so
        // split after its closing paren rather than by whitespace throughout.
        let tail = &stat[stat.rfind(')').expect("comm field is parenthesised") + 1..];
        let fields: Vec<&str> = tail.split_whitespace().collect();
        // `tail` starts at the state field (field 3), so utime/stime (14/15)
        // are offsets 11 and 12.
        let utime: u64 = fields[11].parse().expect("utime");
        let stime: u64 = fields[12].parse().expect("stime");
        utime + stime
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn realtime_kernel_parks_after_every_waker_is_dropped() {
        let wait = Duration::from_millis(300);
        let (waker, ready) = waker_channel();
        let mut k = Kernel::with_ready(RunMode::RealTime, RunFor::Cycles(1), ready);
        let mut dirty = [false; 1];
        k.schedule(0, NanoTime::now() + wait);
        // Every waker gone: no external wake-up can arrive, but the callback
        // above still has to fire.
        drop(waker);

        let cpu_before = thread_cpu_ticks();
        let wall = std::time::Instant::now();
        assert!(
            k.begin_cycle(&mut dirty),
            "the scheduled callback must fire"
        );
        let wall = wall.elapsed();
        let cpu = thread_cpu_ticks() - cpu_before;

        assert!(dirty[0], "the scheduled node must be marked");
        assert!(
            wall >= wait / 2,
            "expected the kernel to wait for the callback, waited {wall:?}"
        );
        // 10 ticks = 100 ms, a third of the wait: comfortably above parking's
        // near-zero cost and far below the spin's ~30.
        assert!(
            cpu < 10,
            "the kernel spun instead of parking: {cpu} clock ticks of CPU over {wall:?}"
        );
    }

    /// `SpinAhead` must not let a callback fire **early**. The guard window
    /// shortens the sleep, and if the spin that replaces it were skipped or
    /// mis-bounded the cycle would open before its scheduled time — which
    /// would be a correctness bug, not a latency one.
    #[test]
    fn spin_ahead_never_fires_before_the_scheduled_time() {
        for _ in 0..20 {
            let mut k = Kernel::new(RunMode::RealTime, RunFor::Cycles(1));
            k.set_timer_policy(TimerPolicy::SpinAhead(Duration::from_micros(500)));
            let mut dirty = [false; 1];
            let target = NanoTime::now() + Duration::from_millis(2);
            k.schedule(0, target);

            assert!(k.begin_cycle(&mut dirty));
            assert!(dirty[0]);
            assert!(
                NanoTime::now() >= target,
                "the cycle opened before its scheduled time"
            );
        }
    }

    /// The point of the policy: the wake lands on the deadline rather than
    /// wherever the scheduler put it.
    ///
    /// Timing on a shared runner is noisy in one direction only — a descheduled
    /// thread is late, never early — so the assertion is on the **best** of 25
    /// trials. That makes it a claim about what the mechanism can achieve when
    /// the machine cooperates, which is the thing that would regress if the
    /// spin were removed, and it does not turn a busy CI box into a red build.
    /// A plain `nanosleep` cannot reach single-digit microseconds even at its
    /// best; the spin closes the last 500 µs itself.
    #[test]
    fn spin_ahead_lands_much_closer_to_the_deadline_than_a_bare_sleep() {
        fn best_overshoot_ns(timer: TimerPolicy) -> u64 {
            let mut best = u64::MAX;
            for _ in 0..25 {
                let mut k = Kernel::new(RunMode::RealTime, RunFor::Cycles(1));
                k.set_timer_policy(timer);
                let mut dirty = [false; 1];
                let target = NanoTime::now() + Duration::from_millis(2);
                k.schedule(0, target);
                assert!(k.begin_cycle(&mut dirty));
                let woke = NanoTime::now();
                best = best.min(u64::from(woke).saturating_sub(u64::from(target)));
            }
            best
        }

        let spun = best_overshoot_ns(TimerPolicy::SpinAhead(Duration::from_micros(500)));
        assert!(
            spun < 20_000,
            "SpinAhead's best wake was {spun} ns late; the spin is not closing the guard window"
        );
    }

    /// `Park` stays the default, and stays a park: no spinning, no CPU burnt
    /// waiting. This is the counterpart to
    /// `realtime_kernel_parks_after_every_waker_is_dropped` — that one guards
    /// the disconnected-channel path, this one guards against `SpinAhead`
    /// becoming the accidental default.
    #[cfg(target_os = "linux")]
    #[test]
    fn park_is_the_default_and_costs_no_cpu() {
        assert_eq!(TimerPolicy::Park, TimerPolicy::default());

        let wait = Duration::from_millis(300);
        let mut k = Kernel::new(RunMode::RealTime, RunFor::Cycles(1));
        let mut dirty = [false; 1];
        k.schedule(0, NanoTime::now() + wait);

        let before = thread_cpu_ticks();
        assert!(k.begin_cycle(&mut dirty));
        let cpu = thread_cpu_ticks() - before;

        assert!(
            cpu < 10,
            "the default policy spun instead of parking: {cpu} clock ticks"
        );
    }

    /// An external wake-up arriving *inside* the guard window is taken
    /// immediately. The spin drains the ready channel as it goes, so
    /// `SpinAhead` does not trade external-source latency for timer accuracy.
    #[test]
    fn spin_ahead_still_takes_external_wakeups_inside_the_guard_window() {
        let (waker, ready) = waker_channel();
        let mut k = Kernel::with_ready(RunMode::RealTime, RunFor::Cycles(1), ready);
        // A guard longer than the wait, so the whole thing is spun.
        k.set_timer_policy(TimerPolicy::SpinAhead(Duration::from_millis(50)));
        let mut dirty = [false; 1];
        let target = NanoTime::now() + Duration::from_millis(30);
        k.schedule(0, target);

        let producer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            waker.wake(0);
        });

        assert!(k.begin_cycle(&mut dirty));
        let woke = NanoTime::now();
        producer.join().expect("producer thread");

        assert!(
            woke < target,
            "the wake-up waited for the timer deadline instead of being taken during the spin"
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
