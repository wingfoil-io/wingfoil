//! Reactive status side-channel for Aeron subscriber / publisher nodes.
//!
//! [`AeronStatusStream`] is a [`MutableNode`] that holds a
//! [`Burst<AeronStatus>`] and emits onto it **only when the observed status
//! differs from the previous one**. Producer nodes (the Aeron subscriber and
//! publisher in this crate) clear the burst at the start of their own
//! `cycle()` and call [`record`](AeronStatusStream::record) after each poll /
//! offer. Consumers wire the stream as a `Dep::Active` upstream and iterate
//! `peek_ref()` to read transitions for the cycle.

use crate::adapters::aeron::status::AeronStatus;
use crate::{Burst, GraphState, MutableNode, StreamPeekRef, UpStreams};

/// A passive wingfoil node that buffers `AeronStatus` transitions for the
/// current cycle.
///
/// The producer node (e.g. `AeronSpinSubBurstNode`) owns an
/// `Rc<RefCell<AeronStatusStream>>` and drives it via `clear()` (cycle start)
/// and `record(new_status)` (after each poll). The stream's own `cycle()` is
/// passive — it returns `Ok(!self.out.is_empty())` so any active downstream
/// node is scheduled to re-cycle exactly when a transition was just recorded.
#[derive(Debug, Default)]
pub struct AeronStatusStream {
    last: AeronStatus,
    out: Burst<AeronStatus>,
    /// When `true`, the node registers an `always_callback` in `start()` so it
    /// is re-cycled every graph cycle and can propagate transitions recorded by
    /// its producer. Set for the driven cases (spin subscriber, publisher);
    /// left `false` for the threaded-subscriber placeholder, which never records
    /// and so must not keep the graph thread spinning.
    self_schedule: bool,
}

impl AeronStatusStream {
    /// Construct a status stream that re-schedules itself every graph cycle.
    ///
    /// Use this when a producer node records transitions on the graph thread
    /// (spin subscriber / publisher) so the node cycles and forwards them to
    /// downstream consumers. The threaded-subscriber placeholder uses
    /// [`default`](Default::default) instead — it never records, so leaving it
    /// unscheduled lets the graph idle.
    #[must_use]
    pub(crate) fn self_scheduling() -> Self {
        Self {
            self_schedule: true,
            ..Default::default()
        }
    }

    /// Records `new` as a transition iff it differs from the previously
    /// recorded status. Called by the host node from its poll / offer path.
    pub(crate) fn record(&mut self, new: AeronStatus) {
        if new != self.last {
            self.last = new.clone();
            self.out.push(new);
        }
    }

    /// Clears the burst at the start of the host node's cycle. Mirrors the
    /// per-cycle data-burst clear contract from Story 12.4.
    pub(crate) fn clear(&mut self) {
        self.out.clear();
    }

    /// Returns the most recent observed status. Prefers the latest value
    /// pushed onto the current cycle's burst, falling back to the previously
    /// recorded `last` when no transition is pending. Useful for outside-graph
    /// callers (e.g. test fixtures, migration shims) — graph consumers should
    /// iterate `peek_ref()` instead.
    pub fn current(&self) -> AeronStatus {
        self.out
            .last()
            .cloned()
            .unwrap_or_else(|| self.last.clone())
    }
}

impl MutableNode for AeronStatusStream {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        Ok(!self.out.is_empty())
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        // Driven status streams (spin subscriber / publisher) must re-cycle
        // every graph cycle to forward producer-recorded transitions; the
        // threaded placeholder stays inert so the graph thread can idle.
        if self.self_schedule {
            state.always_callback();
        }
        Ok(())
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::none()
    }
}

impl StreamPeekRef<Burst<AeronStatus>> for AeronStatusStream {
    fn peek_ref(&self) -> &Burst<AeronStatus> {
        &self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NanoTime, RunFor, RunMode};

    fn make_graph_state() -> GraphState {
        GraphState::new(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(1),
            NanoTime::ZERO,
        )
    }

    #[test]
    fn given_status_stream_when_record_same_then_no_emission() {
        let mut s = AeronStatusStream::default();
        // default `last` is Disconnected — recording the same value emits nothing.
        s.record(AeronStatus::Disconnected);
        assert!(s.peek_ref().is_empty());
        // Even after a different record followed by a same record, the same-record
        // does not push.
        s.record(AeronStatus::Connected);
        s.record(AeronStatus::Connected);
        assert_eq!(s.peek_ref().len(), 1);
        assert_eq!(s.peek_ref().last(), Some(&AeronStatus::Connected));
    }

    #[test]
    fn given_status_stream_when_record_different_then_emission() {
        let mut s = AeronStatusStream::default();
        s.record(AeronStatus::Connected);
        assert_eq!(s.peek_ref().len(), 1);
        assert_eq!(s.peek_ref().last(), Some(&AeronStatus::Connected));
        assert_eq!(s.current(), AeronStatus::Connected);
    }

    #[test]
    fn given_status_stream_when_clear_between_cycles_then_burst_resets() {
        let mut s = AeronStatusStream::default();
        s.record(AeronStatus::Connected);
        assert!(!s.peek_ref().is_empty());
        s.clear();
        assert!(s.peek_ref().is_empty());
        // `last` is preserved across clear — a subsequent same-value record
        // must remain quiet.
        s.record(AeronStatus::Connected);
        assert!(s.peek_ref().is_empty());
    }

    #[test]
    fn given_connect_then_disconnect_when_two_cycles_then_two_separate_transitions() {
        let mut s = AeronStatusStream::default();
        // Cycle 1: simulate clear → record(Connected) sequence.
        s.clear();
        s.record(AeronStatus::Connected);
        assert_eq!(s.peek_ref().len(), 1);
        assert_eq!(s.peek_ref().last(), Some(&AeronStatus::Connected));
        // Cycle 2: clear → record(Disconnected).
        s.clear();
        s.record(AeronStatus::Disconnected);
        assert_eq!(s.peek_ref().len(), 1);
        assert_eq!(s.peek_ref().last(), Some(&AeronStatus::Disconnected));
    }

    #[test]
    fn given_status_stream_when_no_record_then_cycle_returns_false() {
        let mut s = AeronStatusStream::default();
        let mut gs = make_graph_state();
        assert!(!s.cycle(&mut gs).unwrap());
    }

    #[test]
    fn given_status_stream_when_recorded_then_cycle_returns_true_once_until_clear() {
        let mut s = AeronStatusStream::default();
        let mut gs = make_graph_state();
        s.record(AeronStatus::Connected);
        assert!(s.cycle(&mut gs).unwrap());
        // cycle() is non-destructive — burst is still populated.
        assert!(s.cycle(&mut gs).unwrap());
        // Producer clears before the next observation.
        s.clear();
        assert!(!s.cycle(&mut gs).unwrap());
    }

    #[test]
    fn given_status_stream_when_current_with_no_record_then_returns_default() {
        let s = AeronStatusStream::default();
        assert_eq!(s.current(), AeronStatus::Disconnected);
    }

    #[test]
    fn given_status_stream_when_current_after_record_then_returns_recorded() {
        let mut s = AeronStatusStream::default();
        s.record(AeronStatus::BackPressured);
        assert_eq!(s.current(), AeronStatus::BackPressured);
    }
}
