//! Inline inference — the model's `forward()` runs on the graph thread inside
//! `cycle()`. Deterministic and supported in both run modes.

use std::rc::Rc;

use candle_core::Device;
use derive_new::new;

use crate::types::*;

/// A node that runs `infer` on each upstream value, emitting a one-element
/// [`Burst`] per tick. Built by [`inline_infer`].
#[derive(new)]
pub(super) struct InlineInferStream<T, U, F>
where
    T: Element,
    U: Element,
    F: Fn(&T, &Device) -> anyhow::Result<U>,
{
    upstream: Rc<dyn Stream<T>>,
    device: Device,
    infer: F,
    #[new(default)]
    value: Burst<U>,
}

#[node(active = [upstream], output = value: Burst<U>)]
impl<T, U, F> MutableNode for InlineInferStream<T, U, F>
where
    T: Element,
    U: Element,
    F: Fn(&T, &Device) -> anyhow::Result<U> + 'static,
{
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let input = self.upstream.peek_value();
        let prediction = (self.infer)(&input, &self.device)?;
        self.value.clear();
        self.value.push(prediction);
        Ok(true)
    }
}

/// Build an inline inference stream. See [`candle_infer`](super::candle_infer).
pub(super) fn inline_infer<T, U>(
    upstream: &Rc<dyn Stream<T>>,
    device: Device,
    infer: impl Fn(&T, &Device) -> anyhow::Result<U> + 'static,
) -> Rc<dyn Stream<Burst<U>>>
where
    T: Element,
    U: Element,
{
    InlineInferStream::new(upstream.clone(), device, infer).into_stream()
}

#[cfg(test)]
mod tests {
    use crate::adapters::candle::*;
    use crate::*;
    use candle_core::Tensor;

    /// A trivial element-wise model (`x * 2`) exercises the build → forward →
    /// parse pipeline deterministically in historical mode.
    #[test]
    fn inline_doubles_each_value() {
        let prices = ticker(std::time::Duration::from_secs(1)).count(); // 1, 2, 3, ...
        let forecast = candle_infer(
            &prices,
            CandleConfig::inline(),
            |price: &u64, device| Ok(Tensor::new(&[*price as f32], device)?),
            |input: &Tensor| Ok((input * 2.0)?),
            |output: &Tensor| Ok(output.to_vec1::<f32>()?[0] as f64),
        )
        .collapse::<f64>()
        .collect();

        forecast
            .clone()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();

        let values: Vec<f64> = forecast.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(values, vec![2.0, 4.0, 6.0]);
    }

    /// A two-layer MLP whose weights are loaded from an in-memory `VarBuilder`
    /// proves the real safetensors-style model path works on-graph, not just
    /// hand-rolled tensor ops. The network computes `relu(x*W1+b1)*W2+b2`; with
    /// the chosen weights that reduces to `2*x + 1`.
    #[test]
    fn inline_runs_a_loaded_nn_module() -> anyhow::Result<()> {
        use candle_core::DType;
        use candle_nn::{Linear, Module, VarBuilder};
        use std::collections::HashMap;

        let device = Device::Cpu;
        // Weights as if loaded from a checkpoint: 1->1 then 1->1, no nonlinearity
        // exercised on the path (inputs stay positive), so output = 2*x + 1.
        let mut tensors = HashMap::new();
        tensors.insert("l1.weight".to_string(), Tensor::new(&[[2.0f32]], &device)?);
        tensors.insert("l1.bias".to_string(), Tensor::new(&[0.0f32], &device)?);
        tensors.insert("l2.weight".to_string(), Tensor::new(&[[1.0f32]], &device)?);
        tensors.insert("l2.bias".to_string(), Tensor::new(&[1.0f32], &device)?);
        let vb = VarBuilder::from_tensors(tensors, DType::F32, &device);

        let l1 = Linear::new(vb.get((1, 1), "l1.weight")?, Some(vb.get(1, "l1.bias")?));
        let l2 = Linear::new(vb.get((1, 1), "l2.weight")?, Some(vb.get(1, "l2.bias")?));

        let prices = constant(5.0f64);
        let forecast = candle_infer(
            &prices,
            CandleConfig::inline(),
            |price: &f64, device| Ok(Tensor::new(&[[*price as f32]], device)?),
            move |input: &Tensor| {
                let h = l1.forward(input)?.relu()?;
                Ok(l2.forward(&h)?)
            },
            |output: &Tensor| Ok(output.flatten_all()?.to_vec1::<f32>()?[0] as f64),
        )
        .collapse::<f64>();

        forecast
            .clone()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))?;

        assert_eq!(forecast.peek_value(), 11.0); // 2*5 + 1
        Ok(())
    }

    /// An error from any inference stage aborts the run.
    #[test]
    fn inline_propagates_stage_errors() {
        let prices = constant(1.0f64);
        let result = candle_infer(
            &prices,
            CandleConfig::inline(),
            |_price: &f64, _device| anyhow::bail!("build_input boom"),
            |input: &Tensor| Ok(input.clone()),
            |output: &Tensor| Ok(output.to_vec1::<f32>()?[0] as f64),
        )
        .collapse::<f64>()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1));

        // The graph may prepend node context to the top-level message, so match
        // against the full error chain.
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("build_input"), "unexpected error: {err}");
    }
}
