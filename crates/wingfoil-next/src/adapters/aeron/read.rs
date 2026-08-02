//! Typed-parser Aeron subscriber sources — the spin and threaded modes.
//!
//! Backs [`aeron_sub_fragment`](super::aeron_sub_fragment) and
//! [`aeron_sub_fragment_with_status`](super::aeron_sub_fragment_with_status).
//! The typed-parser surface exposes Aeron's per-fragment header (`position`,
//! `session_id`, `stream_id`) and lets parsers signal recoverable errors via
//! [`TransportError`] instead of collapsing "valid but skip" and "malformed"
//! into a single `None`.
//!
//! # Data and status share one stream, then split
//!
//! Both modes emit an internal [`AeronEvent`] envelope multiplexing decoded
//! values and lifecycle-status transitions, which the factories split back into
//! the `(data, status)` pair with `map_filter`. That keeps status **ordered
//! in-band** with the fragments around it (a `Connected` transition is delivered
//! before the fragments that followed it) in *both* modes, and means next needs
//! no bespoke status node — it is the same shape the
//! [`zmq`](crate::adapters::zmq) subscriber uses.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};

use super::buffer::FragmentBuffer;
use super::error::TransportError;
use super::status::AeronStatus;
use super::transport::AeronSubscriberBackend;
use super::{AeronMode, AeronSubOptions, effective_mode};
use crate::Burst;
use crate::fluent::{GraphBuilder, SourceOps, Stream, StreamOps};
use crate::interp::StopHandle;
use crate::op::{Activation, Tick};

/// Data / status multiplexing envelope, private to the adapter: both polling
/// modes produce it and both factories split it apart again.
#[derive(Debug, Clone)]
pub(crate) enum AeronEvent<T> {
    /// A decoded data fragment.
    Data(T),
    /// A lifecycle-status transition derived from the backend after a poll.
    Status(AeronStatus),
}

// `Burst<T>`'s backing array needs `Default`; the value is never observed.
impl<T: Default> Default for AeronEvent<T> {
    fn default() -> Self {
        AeronEvent::Data(T::default())
    }
}

/// Derive the lifecycle status from the backend, in classic's order: `Closed`
/// is terminal and checked first, then `Connected`, else `Disconnected`.
fn derive_status<B: AeronSubscriberBackend>(backend: &B) -> AeronStatus {
    if backend.is_closed() {
        AeronStatus::Closed
    } else if backend.is_connected() {
        AeronStatus::Connected
    } else {
        AeronStatus::Disconnected
    }
}

/// Split a multiplexed event stream into the `(data, status)` pair. Each half
/// ticks only when it has something — status only on a genuine transition, since
/// the producers dedup before emitting.
pub(crate) fn split_events<T>(
    events: &Stream<Burst<AeronEvent<T>>>,
) -> (Stream<Burst<T>>, Stream<Burst<AeronStatus>>)
where
    T: Clone + Default + 'static,
{
    let data = events.map_filter(|burst: &Burst<AeronEvent<T>>| {
        let data: Burst<T> = burst
            .iter()
            .filter_map(|e| match e {
                AeronEvent::Data(v) => Some(v.clone()),
                AeronEvent::Status(_) => None,
            })
            .collect();
        let ticked = !data.is_empty();
        (data, ticked)
    });
    let status = events.map_filter(|burst: &Burst<AeronEvent<T>>| {
        let status: Burst<AeronStatus> = burst
            .iter()
            .filter_map(|e| match e {
                AeronEvent::Status(s) => Some(*s),
                AeronEvent::Data(_) => None,
            })
            .collect();
        let ticked = !status.is_empty();
        (status, ticked)
    });
    (data, status)
}

/// Build the multiplexed event source for `opts.mode`, honouring the
/// graph-thread-poll downgrade.
///
/// `track_status` mirrors classic's two constructors: the plain
/// `aeron_sub_fragment` never derives status at all (no `is_closed` /
/// `is_connected` calls per poll), while the `_with_status` twin does.
pub(crate) fn event_source<T, F, B>(
    g: &GraphBuilder,
    subscriber: B,
    parser: F,
    opts: AeronSubOptions,
    track_status: bool,
) -> Stream<Burst<AeronEvent<T>>>
where
    T: Clone + Default + Send + 'static,
    F: FnMut(&FragmentBuffer<'_>) -> Result<Option<T>, TransportError> + Send + 'static,
    B: AeronSubscriberBackend,
{
    let subscriber = subscriber.with_fragment_limit(opts.fragment_limit);
    match effective_mode(opts.mode, &subscriber) {
        AeronMode::Spin => spin_source(g, subscriber, parser, track_status),
        AeronMode::Threaded => threaded_source(g, subscriber, parser, track_status),
    }
}

// ---------------------------------------------------------------------------
// Spin mode — a busy-spin custom_node polling on the graph thread
// ---------------------------------------------------------------------------

/// Graph-thread state for a spin subscriber. Single-threaded interior
/// mutability (`Rc<RefCell<..>>`), never a lock — the no-locks-on-the-graph-path
/// invariant. Backends that would need a lock report
/// `supports_graph_thread_poll() == false` and are downgraded to `Threaded`
/// before they ever reach here.
struct SpinState<F, B> {
    backend: B,
    parser: F,
    track_status: bool,
    last_status: AeronStatus,
}

/// Wire a `Spin` subscriber: a busy-spin `custom_node` polling Aeron inside the
/// cycle, emitting the fragments decoded this cycle plus any status transition.
fn spin_source<T, F, B>(
    g: &GraphBuilder,
    backend: B,
    parser: F,
    track_status: bool,
) -> Stream<Burst<AeronEvent<T>>>
where
    T: Clone + Default + 'static,
    F: FnMut(&FragmentBuffer<'_>) -> Result<Option<T>, TransportError> + 'static,
    B: AeronSubscriberBackend,
{
    let state = Rc::new(RefCell::new(SpinState {
        backend,
        parser,
        track_status,
        // Mirrors classic's `AeronStatusStream` default, so the first observed
        // state (e.g. Connected) registers as a transition.
        last_status: AeronStatus::Disconnected,
    }));

    g.custom_node::<Burst<AeronEvent<T>>, _>(&[], &[], Activation::ALWAYS, move |_ctx| {
        let st = &mut *state.borrow_mut();
        let mut out: Burst<AeronEvent<T>> = Burst::default();
        {
            let parser = &mut st.parser;
            st.backend.poll_fragments(&mut |frag| match parser(frag) {
                Ok(Some(v)) => out.push(AeronEvent::Data(v)),
                Ok(None) => {}
                // A malformed fragment is logged and skipped — it never aborts
                // the cycle (classic's zero-stopping rule).
                Err(e) => log::warn!(
                    "aeron sub: parser dropped fragment at position {}: {e}",
                    frag.position()
                ),
            })?;
        }
        // A poll error short-circuits above (via `?`) *before* the status is
        // derived, so a transient I/O failure never registers a phantom
        // `Disconnected` transition — classic's ordering.
        if st.track_status {
            let new_status = derive_status(&st.backend);
            if new_status != st.last_status {
                st.last_status = new_status;
                out.push(AeronEvent::Status(new_status));
            }
        }
        Ok(if out.is_empty() {
            Tick::Quiet
        } else {
            Tick::Value(out)
        })
    })
}

// ---------------------------------------------------------------------------
// Threaded mode — a background poll thread over the channel layer
// ---------------------------------------------------------------------------

/// Sets the shared stop flag at teardown so the poll thread exits on its next
/// turn instead of leaking.
struct ThreadStopGuard(Arc<AtomicBool>);

impl Drop for ThreadStopGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Wire a `Threaded` subscriber: `source_at_start` allocates the channel at
/// wiring but spawns the polling thread at graph `start()`, returning a stop
/// guard dropped at teardown.
fn threaded_source<T, F, B>(
    g: &GraphBuilder,
    backend: B,
    parser: F,
    track_status: bool,
) -> Stream<Burst<AeronEvent<T>>>
where
    T: Clone + Default + Send + 'static,
    F: FnMut(&FragmentBuffer<'_>) -> Result<Option<T>, TransportError> + Send + 'static,
    B: AeronSubscriberBackend,
{
    // `source_at_start`'s setup closure is `FnMut` (it may be called on each
    // run), but the backend and parser can only be moved once — the graph is
    // single-run for I/O sources either way, so the `Option::take` makes a
    // second run a clear error rather than a compile-time impossibility.
    let mut moved = Some((backend, parser));
    g.source_at_start::<AeronEvent<T>, _>(move |sender| {
        let (mut backend, mut parser) = moved
            .take()
            .context("aeron sub: the threaded subscriber source is single-run")?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        std::thread::Builder::new()
            .name("aeron-sub".into())
            .spawn(move || {
                let mut idle_count = 0u32;
                let mut last_status = AeronStatus::Disconnected;
                loop {
                    if thread_stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let mut receiver_gone = false;
                    let polled = backend.poll_fragments(&mut |frag| match parser(frag) {
                        Ok(Some(v)) => {
                            // `send` returns false once the graph is gone — a
                            // normal teardown race; stop producing quietly.
                            if !sender.send(AeronEvent::Data(v)) {
                                receiver_gone = true;
                            }
                        }
                        Ok(None) => {}
                        Err(e) => log::warn!(
                            "aeron sub: parser dropped fragment at position {}: {e}",
                            frag.position()
                        ),
                    });
                    if receiver_gone {
                        return;
                    }
                    let count = match polled {
                        Ok(count) => count,
                        Err(e) => {
                            sender.send_error(
                                anyhow::Error::new(e).context("aeron sub: polling fragments"),
                            );
                            return;
                        }
                    };

                    if track_status {
                        let new_status = derive_status(&backend);
                        if new_status != last_status {
                            last_status = new_status;
                            if !sender.send(AeronEvent::Status(new_status)) {
                                return;
                            }
                        }
                    }

                    if count == 0 {
                        idle_count = (idle_count + 1).min(20);
                        let micros = 1u64 << idle_count.min(10);
                        std::thread::sleep(Duration::from_micros(micros));
                    } else {
                        idle_count = 0;
                    }
                }
            })
            .context("aeron sub: spawning poll thread")?;
        Ok(StopHandle::new(ThreadStopGuard(stop)))
    })
}
