//! candle adapter — neural inference on streams.
//!
//! Runs a [`candle`](https://github.com/huggingface/candle) model over a
//! wingfoil stream: each upstream value is shaped into a tensor, pushed through
//! a user-supplied forward pass, and the result tensor is parsed back into a
//! graph value. candle is pure-Rust and synchronous, so its `forward()` maps
//! directly onto wingfoil's `cycle()` model — no libtorch or ONNX Runtime
//! native dependency.
//!
//! This is a **transform** adapter, not a pub/sub I/O adapter: there is no
//! external service to connect to and no `_sub` / `_pub` pair. The single entry
//! point is [`candle_infer`].
//!
//! # The model is a closure, not a path
//!
//! candle has no uniform "load any model from a path" type — a model's
//! architecture lives in Rust code. So instead of a `model_path`, the caller
//! supplies the model as a **`forward` closure** `Fn(&Tensor) -> Result<Tensor>`
//! and loads its weights however it likes (safetensors / GGUF via
//! [`candle_nn::VarBuilder`], or hand-built tensors). This keeps the adapter
//! architecture-agnostic: the same `candle_infer` drives a one-layer linear
//! model, an MLP, an LSTM, or a transformer.
//!
//! Inference is expressed as three closures applied in order each tick:
//!
//! 1. `build_input(&value, &device) -> Tensor` — shape the graph value into a tensor.
//! 2. `forward(&tensor) -> Tensor` — the model's forward pass.
//! 3. `parse_output(&tensor) -> U` — read the result tensor back into a graph value.
//!
//! The output is a [`Burst<U>`]: inline mode emits exactly one element per tick;
//! off-thread mode emits whatever predictions completed since the last cycle
//! (zero, one, or many). Use [`collapse`](crate::StreamOperators::collapse) for
//! single-value handling.
//!
//! # Execution modes
//!
//! Selected via [`CandleMode`] on the [`CandleConfig`]:
//!
//! - [`CandleMode::Inline`] (default) — `forward()` runs directly inside
//!   `cycle()` on the graph thread. Simple and deterministic; works in both
//!   `RealTime` and `HistoricalFrom` modes. Use for small / quantized models
//!   whose forward-pass latency fits the cycle budget.
//! - [`CandleMode::OffThread`] — inference runs on a dedicated worker thread;
//!   completed predictions rejoin the graph asynchronously. Use for heavy models
//!   (transformers, embeddings) where a slow `forward()` would stall the graph.
//!   **Real-time only**, like the `async` adapter.
//!
//! # Example
//!
//! ```ignore
//! use wingfoil::adapters::candle::*;
//! use wingfoil::*;
//!
//! // A trivial "model" that doubles its input, to show the wiring.
//! let prices = constant(3.0f64);
//! let forecast = candle_infer(
//!     &prices,
//!     CandleConfig::inline(),
//!     |price: &f64, device| Ok(Tensor::new(&[*price as f32], device)?),
//!     |input: &Tensor| Ok((input * 2.0)?),
//!     |output: &Tensor| Ok(output.to_vec1::<f32>()?[0] as f64),
//! )
//! .collapse::<f64>();
//!
//! forecast
//!     .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
//!     .unwrap();
//! ```
//!
//! # GPU backends
//!
//! v1 ships the CPU backend only. CUDA / Metal are deliberately not wired as
//! cargo features yet: `cargo lint-all` / CI build with `--all-features`, which
//! would force the GPU toolchains to compile on CPU-only runners. GPU support is
//! a follow-up behind a dedicated workflow (issue #239 §9).

mod inference;
mod offthread;

use std::rc::Rc;

use anyhow::Context;

use crate::types::*;

// Re-export the candle types callers need to write the closures, so they don't
// have to depend on `candle-core` directly.
pub use candle_core::{Device, Tensor};

/// How a [`candle_infer`] node runs its forward pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CandleMode {
    /// Run `forward()` inline on the graph thread. Deterministic; supports both
    /// `RealTime` and `HistoricalFrom`. The default.
    #[default]
    Inline,
    /// Run `forward()` on a dedicated worker thread; predictions rejoin the
    /// graph asynchronously. Real-time only.
    OffThread,
}

/// Configuration for a [`candle_infer`] node: execution mode + compute device.
#[derive(Debug, Clone)]
pub struct CandleConfig {
    /// Inline vs. off-thread execution.
    pub mode: CandleMode,
    /// The candle [`Device`] the model runs on. CPU only in v1.
    pub device: Device,
}

impl Default for CandleConfig {
    fn default() -> Self {
        Self {
            mode: CandleMode::Inline,
            device: Device::Cpu,
        }
    }
}

impl CandleConfig {
    /// Inline execution on the CPU.
    #[must_use]
    pub fn inline() -> Self {
        Self::default()
    }

    /// Off-thread execution on the CPU. Real-time only.
    #[must_use]
    pub fn off_thread() -> Self {
        Self {
            mode: CandleMode::OffThread,
            device: Device::Cpu,
        }
    }

    /// Override the compute device.
    #[must_use]
    pub fn with_device(mut self, device: Device) -> Self {
        self.device = device;
        self
    }
}

/// Run a candle model over `upstream`, emitting one prediction per input.
///
/// Each tick applies, in order:
/// `build_input` → `forward` → `parse_output`. See the [module docs](self) for
/// the rationale behind the closure-based model API and the two execution modes.
///
/// Returns a `Stream<Burst<U>>`: inline mode yields exactly one `U` per input;
/// off-thread mode yields whatever predictions completed since the last cycle.
///
/// # Errors at run time
///
/// Any error returned by the three closures aborts the run (inline) or is
/// surfaced on the result stream (off-thread), wrapped with the failing stage.
#[must_use]
pub fn candle_infer<T, U>(
    upstream: &Rc<dyn Stream<T>>,
    config: CandleConfig,
    build_input: impl Fn(&T, &Device) -> anyhow::Result<Tensor> + Send + 'static,
    forward: impl Fn(&Tensor) -> anyhow::Result<Tensor> + Send + 'static,
    parse_output: impl Fn(&Tensor) -> anyhow::Result<U> + Send + 'static,
) -> Rc<dyn Stream<Burst<U>>>
where
    T: Element + Send,
    U: Element + Send,
{
    // Collapse the three stages into one inference closure shared by both
    // backends, so neither backend re-implements the pipeline.
    let infer = move |value: &T, device: &Device| -> anyhow::Result<U> {
        let input = build_input(value, device).context("candle build_input failed")?;
        let output = forward(&input).context("candle forward failed")?;
        parse_output(&output).context("candle parse_output failed")
    };

    let device = config.device;
    match config.mode {
        CandleMode::Inline => inference::inline_infer(upstream, device, infer),
        CandleMode::OffThread => offthread::offthread_infer(upstream, device, infer),
    }
}
