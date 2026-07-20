//! [`produce_async`]: the classic `produce_async` ergonomic — an async
//! closure that yields *timestamped* values driving a graph source — over the
//! [`channel`](crate::fluent::GraphBuilder::channel) layer.
//!
//! The closure returns a [`futures::Stream`] of `Result<(NanoTime, T)>`. A
//! task spawned on the caller's tokio runtime drives it, forwarding each
//! value to the channel (timestamped, so it works in **both** run modes —
//! deterministic historical replay on the graph clock, or live realtime) and
//! closing at end-of-stream. A producer error propagates into the graph and
//! aborts the run. The graph source emits [`Burst<T>`](crate::burst::Burst),
//! never latest-wins.
//!
//! Gated behind the `async` feature (it pulls in `tokio` + `futures`); the
//! core engine stays executor-free.
//!
//! ```ignore
//! let rt = tokio::runtime::Runtime::new()?;
//! let g = GraphBuilder::new();
//! let quotes = produce_async(&g, rt.handle(), || async {
//!     Ok(futures::stream::iter(vec![
//!         Ok((NanoTime::new(100), 1.0)),
//!         Ok((NanoTime::new(200), 2.0)),
//!     ]))
//! });
//! ```

use std::future::Future;

use futures::StreamExt;
use wingfoil::{NanoTime, RunFor, RunMode};

use crate::burst::Burst;
use crate::fluent::{GraphBuilder, Stream};

/// The run parameters handed to a producer closure (mirrors classic
/// `RunParams`), so a producer can choose a historical vs live data source.
/// Snapshotted at wiring time; for the run bound, prefer emitting a finite
/// stream and letting the receiver stop at end-of-stream.
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
    let (stream, sender) = g.channel::<T>();
    handle.spawn(async move {
        match run(params).await {
            Err(e) => {
                let _ = sender.send_error(e);
            }
            Ok(source) => {
                futures::pin_mut!(source);
                while let Some(item) = source.next().await {
                    match item {
                        // `send_at` returns false once the receiver is gone —
                        // a normal teardown race; stop producing.
                        Ok((t, v)) => {
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
    stream
}
