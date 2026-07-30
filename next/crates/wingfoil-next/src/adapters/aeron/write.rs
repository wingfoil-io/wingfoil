//! Aeron publisher sink.
//!
//! Serialises values from an upstream burst stream and offers them to an Aeron
//! publication on every cycle. Only [`RunMode::RealTime`] is supported (checked
//! at graph `start()`, as in classic).
//!
//! An opt-in **status side-channel**
//! ([`AeronSinkOps::aeron_pub_with_status`]) returns the sink paired with a
//! `Stream<Burst<AeronStatus>>` emitting `Connected` / `BackPressured` /
//! `Closed` / `Disconnected` transitions.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use wingfoil::RunMode;

use super::error::TransportError;
use super::status::AeronStatus;
use super::transport::AeronPublisherBackend;
use crate::Burst;
use crate::fluent::{Stream, StreamOps};
use crate::interp::StopHandle;
use crate::op::{Activation, Ctx, Tick};

/// Graph-thread state for the publisher: the backend plus the last recorded
/// status (for transition dedup). `Rc<RefCell<..>>`, never a lock.
struct PubState<S, B> {
    backend: B,
    serialiser: S,
    last_status: AeronStatus,
}

/// One cycle of publishing: offer every item in the burst, then derive the
/// status. Returns the status transition for this cycle, if any.
///
/// Ordering matches classic exactly: `Closed` is terminal and short-circuits
/// before any offer; a `BackPressure` error records `BackPressured` and drops
/// the rest of the burst (latest-wins); any other offer error aborts the run;
/// otherwise a successful offer means `Connected`, and an empty burst falls back
/// to the backend's own `is_connected`.
fn publish_cycle<T, S, B>(st: &mut PubState<S, B>, burst: &Burst<T>) -> Result<Option<AeronStatus>>
where
    T: Default,
    S: Fn(&T) -> Vec<u8>,
    B: AeronPublisherBackend,
{
    let record = |st: &mut PubState<S, B>, new: AeronStatus| {
        if new != st.last_status {
            st.last_status = new;
            Some(new)
        } else {
            None
        }
    };

    if st.backend.is_closed() {
        return Ok(record(st, AeronStatus::Closed));
    }

    let mut offer_outcome: Option<AeronStatus> = None;
    for item in burst.iter() {
        let bytes = (st.serialiser)(item);
        match st.backend.offer(&bytes) {
            Ok(()) => offer_outcome = Some(AeronStatus::Connected),
            Err(e) => {
                if let Some(TransportError::BackPressure) = e.downcast_ref::<TransportError>() {
                    return Ok(record(st, AeronStatus::BackPressured));
                }
                return Err(e);
            }
        }
    }

    let new_status = offer_outcome.unwrap_or_else(|| {
        if st.backend.is_connected() {
            AeronStatus::Connected
        } else {
            AeronStatus::Disconnected
        }
    });
    Ok(record(st, new_status))
}

/// Reject a historical run at graph `start()`, matching classic's publisher.
fn realtime_only(run_mode: RunMode) -> Result<StopHandle> {
    if run_mode != RunMode::RealTime {
        anyhow::bail!("Aeron publisher only supports RealTime mode");
    }
    Ok(StopHandle::new(()))
}

/// Extension trait letting a `Stream<Burst<T>>` publish to Aeron.
///
/// Two constructors, mirroring classic's `AeronPub`:
///
/// - [`aeron_pub`](Self::aeron_pub) — offer every burst item each cycle.
/// - [`aeron_pub_with_status`](Self::aeron_pub_with_status) — also return a
///   `Stream<Burst<AeronStatus>>` of lifecycle transitions.
///
/// Bring it in with `use wingfoil_next::adapters::aeron::AeronSinkOps;`.
pub trait AeronSinkOps<T> {
    /// Publish every burst item on every cycle. No status side-channel.
    ///
    /// Returns the sink `Stream<()>`, which ticks on every upstream tick.
    /// Publishing is real-time only: a historical run aborts at graph `start()`.
    fn aeron_pub<B: AeronPublisherBackend>(
        &self,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> Stream<()>;

    /// Publish every burst item and emit lifecycle transitions on a paired
    /// status stream.
    ///
    /// Returns `(sink, status)`. The status stream ticks **only on transition
    /// cycles** (no re-emission in steady state): `Closed` is terminal and
    /// checked first, a `BackPressure` offer error records `BackPressured` and
    /// drops the rest of the burst, a successful offer records `Connected`, and
    /// an empty burst falls back to the backend's `is_connected`.
    fn aeron_pub_with_status<B: AeronPublisherBackend>(
        &self,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> (Stream<()>, Stream<Burst<AeronStatus>>);
}

impl<T> AeronSinkOps<T> for Stream<Burst<T>>
where
    T: Clone + Default + 'static,
{
    fn aeron_pub<B: AeronPublisherBackend>(
        &self,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> Stream<()> {
        let state = Rc::new(RefCell::new(PubState {
            backend: publisher,
            serialiser,
            last_status: AeronStatus::Disconnected,
        }));
        self.wire(move |b, h| {
            let sink = b.register_op1(
                h,
                "aeron_pub",
                Activation::NONE,
                state,
                || (),
                move |state, _s: &mut (), burst: &Burst<T>, _ctx: &mut Ctx<'_>| {
                    publish_cycle(&mut state.borrow_mut(), burst)?;
                    Ok(Tick::Value(()))
                },
            );
            // Classic checked the run mode in the node's `start`; so do we.
            b.compose_spawn_at_start(sink.index(), move |run_mode, _run_for, _start_time| {
                realtime_only(run_mode)
            });
            sink
        })
    }

    fn aeron_pub_with_status<B: AeronPublisherBackend>(
        &self,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> (Stream<()>, Stream<Burst<AeronStatus>>) {
        let state = Rc::new(RefCell::new(PubState {
            backend: publisher,
            serialiser,
            last_status: AeronStatus::Disconnected,
        }));
        // The op emits the cycle's transitions (empty when none); the sink half
        // is derived from it, so both halves ride one node — the same
        // split-a-multiplexed-stream shape the subscriber uses.
        let transitions = self.wire(move |b, h| {
            let node = b.register_op1(
                h,
                "aeron_pub",
                Activation::NONE,
                state,
                || (),
                move |state, _s: &mut (), burst: &Burst<T>, _ctx: &mut Ctx<'_>| {
                    let transition = publish_cycle(&mut state.borrow_mut(), burst)?;
                    let mut out: Burst<AeronStatus> = Burst::default();
                    if let Some(status) = transition {
                        out.push(status);
                    }
                    Ok(Tick::Value(out))
                },
            );
            b.compose_spawn_at_start(node.index(), move |run_mode, _run_for, _start_time| {
                realtime_only(run_mode)
            });
            node
        });
        let sink = transitions.map(|_: &Burst<AeronStatus>| ());
        let status = transitions.map_filter(|transitions: &Burst<AeronStatus>| {
            (transitions.clone(), !transitions.is_empty())
        });
        (sink, status)
    }
}
