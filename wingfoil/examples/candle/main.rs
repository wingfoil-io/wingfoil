//! candle adapter example — short-horizon price forecasting with a small MLP.
//!
//! A synthetic price stream is fed through a two-layer perceptron
//! (`1 → 8 → 1`, tanh hidden) that predicts the next-step price *delta*. The
//! model runs **inline** — its `forward()` executes on the graph thread inside
//! each cycle — which is the right mode for a network this small.
//!
//! The weights here are fixed in code so the run is fully reproducible and needs
//! no downloaded checkpoint. In a real deployment you would load weights from a
//! safetensors / GGUF file via [`candle_nn::VarBuilder`] and capture the model
//! in the same `forward` closure; the wiring through `candle_infer` is identical.
//!
//! For a heavy model whose `forward()` would stall the cycle, switch
//! `CandleConfig::inline()` to `CandleConfig::off_thread()` (real-time only) and
//! the inference runs on a dedicated worker thread instead.
//!
//! # Run
//!
//! ```sh
//! cargo run --example candle --features candle
//! ```

use std::time::Duration;

use wingfoil::adapters::candle::*;
use wingfoil::*;

use candle_core::{DType, Tensor};
use candle_nn::{Linear, Module, VarBuilder};
use std::collections::HashMap;

/// Centre prices roughly on this level before feeding the network.
const PRICE_MEAN: f64 = 100.0;

/// Build a deterministic 1→8→1 MLP. In production these tensors would come from
/// a checkpoint; here they are fixed so the example output is reproducible.
fn build_model(
    device: &Device,
) -> anyhow::Result<impl Fn(&Tensor) -> anyhow::Result<Tensor> + use<>> {
    // Eight hidden units with a spread of weights/biases, and an output layer
    // that averages them down to a single delta.
    let w1: Vec<f32> = (0..8).map(|i| 0.15 + 0.05 * i as f32).collect();
    let b1: Vec<f32> = (0..8).map(|i| -0.1 + 0.02 * i as f32).collect();
    let w2: Vec<f32> = vec![0.25; 8];

    let mut tensors = HashMap::new();
    tensors.insert(
        "l1.weight".to_string(),
        Tensor::from_vec(w1, (8, 1), device)?,
    );
    tensors.insert("l1.bias".to_string(), Tensor::from_vec(b1, 8, device)?);
    tensors.insert(
        "l2.weight".to_string(),
        Tensor::from_vec(w2, (1, 8), device)?,
    );
    tensors.insert("l2.bias".to_string(), Tensor::new(&[0.0f32], device)?);
    let vb = VarBuilder::from_tensors(tensors, DType::F32, device);

    let l1 = Linear::new(vb.get((8, 1), "l1.weight")?, Some(vb.get(8, "l1.bias")?));
    let l2 = Linear::new(vb.get((1, 8), "l2.weight")?, Some(vb.get(1, "l2.bias")?));

    Ok(move |input: &Tensor| {
        let hidden = l1.forward(input)?.tanh()?;
        Ok(l2.forward(&hidden)?)
    })
}

/// Deterministic synthetic price: a gentle sine ripple plus slow drift.
fn synthetic_price(tick: u64) -> f64 {
    let t = tick as f64;
    PRICE_MEAN + 5.0 * (t * 0.4).sin() + 0.2 * t
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let device = Device::Cpu;
    let model = build_model(&device)?;

    // Synthetic market data: one price per tick.
    let prices = ticker(Duration::from_millis(100))
        .count()
        .map(synthetic_price);

    // Forecast the next-step price = current price + predicted delta.
    let forecast = candle_infer(
        &prices,
        CandleConfig::inline(),
        // build_input: centre + scale the price into a (1, 1) tensor.
        |price: &f64, device| {
            let x = ((*price - PRICE_MEAN) / 10.0) as f32;
            Ok(Tensor::from_vec(vec![x], (1, 1), device)?)
        },
        // forward: the MLP.
        move |input: &Tensor| model(input),
        // parse_output: read the scalar delta back out.
        |output: &Tensor| Ok(output.flatten_all()?.to_vec1::<f32>()?[0] as f64),
    )
    .collapse::<f64>();

    // Pair each forecast with the price that produced it and print.
    bimap(
        Dep::Active(forecast),
        Dep::Passive(prices),
        |delta: f64, price: f64| {
            format!(
                "price = {price:8.3}   next-step forecast = {:8.3}",
                price + delta
            )
        },
    )
    .print()
    .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(10))?;

    Ok(())
}
