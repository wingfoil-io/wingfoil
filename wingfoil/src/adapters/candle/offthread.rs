//! Off-thread inference — the model's `forward()` runs on a dedicated worker
//! thread so a slow forward pass never stalls the graph cycle. Predictions
//! rejoin the graph asynchronously via a [`ReceiverStream`], the same primitive
//! the zmq / aeron subscribers use. Real-time only.
//!
//! Wiring:
//!
//! ```text
//!   upstream ──active──▶ Feeder ──kanal──▶ worker thread ──ChannelSender──▶ ReceiverStream
//!                                                                                 │ active
//!                                            feeder ──passive──▶ OffThreadInfer ◀─┘
//! ```
//!
//! [`OffThreadInfer`] is the node the caller holds. It re-emits the
//! `ReceiverStream`'s results (its active upstream) and keeps the [`Feeder`]
//! reachable in the graph as a passive upstream, so the engine cycles the feeder
//! whenever `upstream` ticks and pumps inputs to the worker.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use candle_core::Device;
use derive_new::new;
use kanal::ReceiveErrorTimeout;

use crate::ReceiverStream;
use crate::channel::{ChannelSender, Message};
use crate::types::*;

/// How long the worker blocks on the input channel before re-checking the stop
/// flag. Bounds shutdown latency without busy-spinning.
const WORKER_POLL: Duration = Duration::from_millis(25);

/// Forwards each upstream value to the worker thread. A pure sink: it never
/// emits, it only side-effects onto the channel.
#[derive(new)]
struct Feeder<T: Element + Send> {
    upstream: Rc<dyn Stream<T>>,
    input_tx: kanal::Sender<T>,
}

#[node(active = [upstream])]
impl<T: Element + Send> MutableNode for Feeder<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        // Unbounded send never blocks the graph thread (preserving the
        // "cycle never blocks" invariant). A closed channel means the worker is
        // gone; drop the value rather than erroring — the result stream surfaces
        // the worker's failure.
        let _ = self.input_tx.send(self.upstream.peek_value());
        Ok(false)
    }
}

/// The caller-facing node: re-emits worker results, keeps the feeder alive.
#[derive(new)]
struct OffThreadInfer<U: Element + Send> {
    results: Rc<dyn Stream<Burst<U>>>,
    feeder: Rc<dyn Node>,
    #[new(default)]
    value: Burst<U>,
}

#[node(output = value: Burst<U>)]
impl<U: Element + Send> MutableNode for OffThreadInfer<U> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = self.results.peek_value();
        Ok(!self.value.is_empty())
    }

    fn upstreams(&self) -> UpStreams {
        // `results` is active (drives this node when predictions arrive);
        // `feeder` is passive (pulled into the graph, but cycled by its own
        // active dependency on `upstream`, not by us).
        UpStreams::new(
            vec![self.results.clone().as_node()],
            vec![self.feeder.clone()],
        )
    }
}

/// The worker loop: pull inputs, run `infer`, push results back to the graph.
fn worker_loop<T, U>(
    input_rx: &kanal::Receiver<T>,
    sender: &ChannelSender<U>,
    stop: &AtomicBool,
    device: &Device,
    infer: &impl Fn(&T, &Device) -> anyhow::Result<U>,
) where
    T: Element + Send,
    U: Element + Send,
{
    while !stop.load(Ordering::Relaxed) {
        match input_rx.recv_timeout(WORKER_POLL) {
            Ok(value) => match infer(&value, device) {
                Ok(prediction) => {
                    if sender
                        .send_message(Message::RealtimeValue(prediction))
                        .is_err()
                    {
                        return; // graph gone — stop quietly
                    }
                }
                Err(e) => {
                    // Surface the failure on the result stream, then stop.
                    let _ = sender.send_message(Message::Error(Arc::new(e)));
                    return;
                }
            },
            Err(ReceiveErrorTimeout::Timeout) => continue,
            // Feeder dropped its sender (graph tearing down): clean shutdown.
            Err(ReceiveErrorTimeout::Closed | ReceiveErrorTimeout::SendClosed) => break,
        }
    }
    let _ = sender.send_message(Message::EndOfStream);
}

/// Build an off-thread inference stream. See [`candle_infer`](super::candle_infer).
pub(super) fn offthread_infer<T, U>(
    upstream: &Rc<dyn Stream<T>>,
    device: Device,
    infer: impl Fn(&T, &Device) -> anyhow::Result<U> + Send + 'static,
) -> Rc<dyn Stream<Burst<U>>>
where
    T: Element + Send,
    U: Element + Send,
{
    let (input_tx, input_rx) = kanal::unbounded::<T>();
    let feeder: Rc<dyn Node> = Feeder::new(upstream.clone(), input_tx).into_node();

    let results = ReceiverStream::new(
        move |sender, stop| {
            // The callback owns these captures; lend them to the worker loop.
            worker_loop(&input_rx, &sender, &stop, &device, &infer);
            Ok(())
        },
        true, // off-thread inference is real-time only
    )
    .into_stream();

    OffThreadInfer::new(results, feeder).into_stream()
}

#[cfg(test)]
mod tests {
    use crate::adapters::candle::*;
    use crate::*;
    use candle_core::Tensor;
    use std::time::Duration;

    /// End-to-end off-thread run in real-time mode: a synthetic price stream is
    /// fed through a worker-thread model (`x * 10`) and the predictions rejoin
    /// the graph. We assert the worker produced sane results.
    #[test]
    fn offthread_produces_predictions() {
        let prices = ticker(Duration::from_millis(5)).count(); // 1, 2, 3, ...
        let predictions = candle_infer(
            &prices,
            CandleConfig::off_thread(),
            |price: &u64, device| Ok(Tensor::new(&[*price as f32], device)?),
            |input: &Tensor| Ok((input * 10.0)?),
            |output: &Tensor| Ok(output.to_vec1::<f32>()?[0] as f64),
        )
        .collapse::<f64>()
        .collect();

        predictions
            .clone()
            .run(
                RunMode::RealTime,
                RunFor::Duration(Duration::from_millis(200)),
            )
            .unwrap();

        let values: Vec<f64> = predictions.peek_value().iter().map(|v| v.value).collect();
        assert!(!values.is_empty(), "worker thread produced no predictions");
        // Every prediction must be a positive multiple of 10 (10*price).
        for v in &values {
            assert!(
                *v >= 10.0 && (*v % 10.0).abs() < 1e-3,
                "unexpected prediction {v}"
            );
        }
    }

    /// Off-thread mode rejects historical runs (the worker can't be replayed
    /// deterministically), matching the `async` adapter's constraint.
    #[test]
    fn offthread_rejects_historical_mode() {
        let prices = constant(1.0f64);
        let result = candle_infer(
            &prices,
            CandleConfig::off_thread(),
            |price: &f64, device| Ok(Tensor::new(&[*price as f32], device)?),
            |input: &Tensor| Ok(input.clone()),
            |output: &Tensor| Ok(output.to_vec1::<f32>()?[0] as f64),
        )
        .collapse::<f64>()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1));

        assert!(
            result.is_err(),
            "off-thread mode must reject historical runs"
        );
    }
}
