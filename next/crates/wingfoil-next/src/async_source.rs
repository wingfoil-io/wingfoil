//! [`produce_async`]: the classic `produce_async` ergonomic — an async closure
//! that yields *timestamped* values driving a graph source — over the
//! [`channel`](crate::fluent::SourceOps::channel) layer.
//!
//! The closure returns a [`futures::Stream`] of `Result<(NanoTime, T)>`. A
//! task spawned on the caller's tokio runtime drives it, forwarding each
//! value to the channel (timestamped, so it works in **both** run modes —
//! deterministic historical replay on the graph clock, or live realtime) and
//! closing at end-of-stream. A producer error propagates into the graph and
//! aborts the run. The graph source emits [`Burst<T>`](crate::Burst),
//! never latest-wins.
//!
//! Gated behind the `async` feature (it pulls in `tokio` + `futures`); the
//! core engine stays executor-free.
//!
//! # Two guarantees classic gives that this layer must not drop
//!
//! Classic `produce_async` derives its [`RunParams`] from the graph's own run
//! (in the node's `setup`, from the live `run_mode`/`run_for`/`start_time`) and
//! bounds the producer→graph channel with `buffer_size`. Here the producer task
//! is spawned at *wiring* time, before [`Runner::run`](crate::interp::Runner::run)
//! is called, so the caller has to hand in the [`RunParams`] up front. That
//! makes it possible for the caller to pass params that disagree with the
//! actual `run(run_mode, run_for)` — which classic makes impossible by
//! construction. To close the gap:
//!
//! * **Validation.** In historical mode the run's `start_time` is knowable and
//!   deterministic (it is the `HistoricalFrom(_)` instant). A validating
//!   passthrough on the source checks, on the first burst it sees, that the
//!   caller-supplied `params.start_time` equals the run's real `start_time`
//!   and [aborts the run](anyhow::bail) with context on a mismatch, rather
//!   than silently replaying against a bogus timeline. (`run_mode`/`run_for`
//!   are not observable from an op's [`Ctx`](crate::op::Ctx) at this layer, so
//!   the caller stays responsible for those matching the eventual `run(..)`;
//!   note a `run_mode` mismatch that shifts `start_time` — e.g. declaring
//!   historical while running realtime, or vice versa — is caught here too.)
//! * **Backpressure.** `buffer_size` bounds how far the realtime producer may
//!   run ahead of the graph: the producer takes a permit before each send and
//!   the passthrough returns one per delivered value, so at most ~`buffer_size`
//!   values sit undelivered — the producer blocks instead of growing memory
//!   without limit. See [`produce_async`] for the historical caveat.
//!
//! ```ignore
//! let rt = tokio::runtime::Runtime::new()?;
//! let g = GraphBuilder::new();
//! let quotes = produce_async_bounded(&g, rt.handle(), params, Some(64), |_p| async {
//!     Ok(futures::stream::iter(vec![
//!         Ok((NanoTime::new(100), 1.0)),
//!         Ok((NanoTime::new(200), 2.0)),
//!     ]))
//! });
//! ```

use std::future::Future;

use futures::{SinkExt, StreamExt};
use wingfoil::{NanoTime, RunFor, RunMode};

use crate::Burst;
use crate::fluent::{GraphBuilder, SourceOps, Stream};
use crate::op::{Activation, Tick};

/// The run parameters handed to a producer closure (mirrors classic
/// `RunParams`), so a producer can choose a historical vs live data source.
///
/// These describe the run the graph will actually be driven with. They must
/// match the `run(run_mode, run_for)` the caller ultimately invokes; a
/// disagreement in `start_time` is rejected at run time (see the module docs).
/// For the run *bound*, prefer emitting a finite stream and letting the
/// receiver stop at end-of-stream.
#[derive(Clone, Copy, Debug)]
pub struct RunParams {
    pub run_mode: RunMode,
    pub run_for: RunFor,
    pub start_time: NanoTime,
}

/// Drive a graph source from an async producer of timestamped values. See the
/// module docs. Returns the source [`Stream<Burst<T>>`].
///
/// The producer closure receives [`RunParams`]; it must return a stream of
/// `Result<(NanoTime, T)>`. Each `Ok((t, v))` is delivered at graph time `t`
/// (historical replay) or live (realtime); an `Err` aborts the run.
///
/// `params` must describe the same run passed to
/// [`run`](crate::interp::Runner::run): in historical mode the run's
/// `start_time` is validated against `params.start_time` and a mismatch aborts
/// the run with context (the caller-supplied params are never trusted blind).
///
/// This is the unbounded variant — a fast producer feeding a slower graph can
/// accumulate an arbitrarily large backlog. Use [`produce_async_bounded`] to
/// apply `buffer_size` back-pressure in realtime.
pub fn produce_async<T, F, Fut, S>(
    g: &GraphBuilder,
    handle: &tokio::runtime::Handle,
    params: RunParams,
    run: F,
) -> Stream<Burst<T>>
where
    T: Clone + Default + Send + 'static,
    F: FnOnce(RunParams) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<S>> + Send + 'static,
    S: futures::Stream<Item = anyhow::Result<(NanoTime, T)>> + Send + 'static,
{
    produce_async_bounded(g, handle, params, None, run)
}

/// [`produce_async`] with `buffer_size` back-pressure (mirrors classic
/// `produce_async`'s `buffer_size`). See the module docs.
///
/// `params` must describe the same run passed to
/// [`run`](crate::interp::Runner::run): in historical mode the run's
/// `start_time` is validated against `params.start_time` and a mismatch aborts
/// the run with context (the caller-supplied params are never trusted blind).
///
/// `buffer_size` bounds the producer→graph backlog in **realtime**: `Some(n)`
/// lets the producer run at most ~`n` values ahead of the graph before it
/// blocks (back-pressure); `None` is unbounded (a fast producer feeding a
/// slower graph can accumulate an arbitrarily large backlog). `Some(0)` is
/// treated as `Some(1)`. Historical replay collects the whole timestamped
/// stream up front (the producer sends everything, then closes), so
/// `buffer_size` is not applied there — bounding it would deadlock that
/// up-front collection.
pub fn produce_async_bounded<T, F, Fut, S>(
    g: &GraphBuilder,
    handle: &tokio::runtime::Handle,
    params: RunParams,
    buffer_size: Option<usize>,
    run: F,
) -> Stream<Burst<T>>
where
    T: Clone + Default + Send + 'static,
    F: FnOnce(RunParams) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<S>> + Send + 'static,
    S: futures::Stream<Item = anyhow::Result<(NanoTime, T)>> + Send + 'static,
{
    let (stream, sender) = g.channel::<T>();

    // Realtime backpressure: a bounded permit channel. The producer takes a
    // permit before each send; the validating passthrough returns one per
    // delivered value. Only wired in realtime — a historical run collects the
    // whole stream at graph start, so throttling the producer there would
    // deadlock that collection. Dropping the receiver (when the graph is torn
    // down) closes the channel, which unblocks a parked producer.
    let (mut permit_tx, mut permit_rx) = match (params.run_mode, buffer_size) {
        (RunMode::RealTime, Some(n)) => {
            // futures' bounded channel grants each sender one extra slot on top
            // of `buffer`, so `n - 1` bounds in-flight values to ~`n`.
            let (tx, rx) = futures::channel::mpsc::channel::<()>(n.max(1) - 1);
            (Some(tx), Some(rx))
        }
        _ => (None, None),
    };

    // Validating passthrough: forwards each burst unchanged, checks the
    // caller's declared params against the real run once, and releases permits.
    let mut checked = false;
    let declared = params;
    let validated = stream.wire(move |b, h| {
        b.register_op1(
            h,
            "produce_async::validate",
            Activation::NONE,
            (),
            (),
            move |_cfg: &mut (), _state: &mut (), input: &Burst<T>, ctx| {
                if !checked {
                    checked = true;
                    if let RunMode::HistoricalFrom(_) = declared.run_mode {
                        let actual = ctx.start_time();
                        if declared.start_time != actual {
                            anyhow::bail!(
                                "produce_async: caller-supplied RunParams.start_time ({:?}) \
                                 does not match the graph run's start_time ({:?}); RunParams \
                                 must describe the same run passed to run(run_mode, run_for)",
                                declared.start_time,
                                actual,
                            );
                        }
                    }
                }
                // Return one permit per delivered value so the producer may
                // advance. Draining exactly `len` keeps the bound intact
                // (never freeing capacity for values not yet delivered).
                if let Some(rx) = permit_rx.as_mut() {
                    for _ in 0..input.len() {
                        if rx.try_recv().is_err() {
                            break;
                        }
                    }
                }
                Ok(Tick::Value(input.clone()))
            },
        )
    });

    handle.spawn(async move {
        match run(params).await {
            Err(e) => {
                let _ = sender.send_error(e);
            }
            Ok(source) => {
                futures::pin_mut!(source);
                while let Some(item) = source.next().await {
                    match item {
                        Ok((t, v)) => {
                            // Take a permit first (realtime only). A closed
                            // permit channel means the graph is gone — a normal
                            // teardown race; stop producing.
                            if let Some(tx) = permit_tx.as_mut()
                                && tx.send(()).await.is_err()
                            {
                                return;
                            }
                            // `send_at` returns false once the receiver is gone —
                            // a normal teardown race; stop producing.
                            if !sender.send_at(v, t) {
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = sender.send_error(e);
                            return;
                        }
                    }
                }
                let _ = sender.close();
            }
        }
    });
    validated
}
