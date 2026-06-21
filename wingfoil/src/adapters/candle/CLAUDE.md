# candle Adapter

Neural inference on streams via [`candle`](https://github.com/huggingface/candle).
A **transform** adapter, not pub/sub I/O: the single entry point is `candle_infer`,
which runs a user-supplied model over a stream.

This adapter was scoped by issue #239. It deviates from the default `/new-adapter`
shape in several deliberate ways — see **Deviations from `/new-adapter`** below.

## Module Structure

```
candle/
  mod.rs        # CandleConfig, CandleMode, Device/Tensor re-exports, candle_infer factory
  inference.rs  # Inline backend: InlineInferStream (MutableNode) + unit tests
  offthread.rs  # Off-thread backend: Feeder + worker loop + OffThreadInfer + tests
  CLAUDE.md     # This file
```

There is no `read.rs` / `write.rs` (no I/O direction), no `integration_tests.rs`
(no external service — tests are inline `#[cfg(test)]` modules), and no Python
bindings (deferred to v2).

## Key Components

### `candle_infer` — the one public factory

```rust
candle_infer(upstream, config, build_input, forward, parse_output)
    -> Rc<dyn Stream<Burst<U>>>
```

Three closures applied in order each tick:

1. `build_input(&T, &Device) -> Result<Tensor>` — shape the graph value into a tensor.
2. `forward(&Tensor) -> Result<Tensor>` — the model's forward pass.
3. `parse_output(&Tensor) -> Result<U>` — read the result tensor back into a graph value.

`mod.rs` collapses these into one internal `infer: Fn(&T, &Device) -> Result<U>`
closure (each stage wrapped with `.context(...)`), so the two backends share the
exact same pipeline and neither re-implements it.

### The model is a closure, not a path

candle has no uniform "load any model from a path" type — a model's architecture
lives in Rust code. So the caller supplies the model as the `forward` closure and
loads weights however they like (safetensors / GGUF via `candle_nn::VarBuilder`,
or hand-built tensors). This is the key API decision (issue #239 open question 1):
it keeps the adapter architecture-agnostic. `inference.rs`'s
`inline_runs_a_loaded_nn_module` test exercises the real `VarBuilder` → `Linear`
path to prove it.

### `CandleMode` / `CandleConfig`

- `CandleMode::Inline` (default) — `forward()` runs inside `cycle()` on the graph
  thread. Deterministic; supports both `RealTime` and `HistoricalFrom`.
- `CandleMode::OffThread` — `forward()` runs on a dedicated worker thread;
  predictions rejoin the graph asynchronously. **Real-time only.**
- `CandleConfig::inline()` / `::off_thread()` / `.with_device(d)` constructors.

### Output type

Both modes return `Rc<dyn Stream<Burst<U>>>` for a consistent API. Inline emits a
one-element burst per input; off-thread emits whatever completed since the last
cycle (0, 1, or many). Use `.collapse::<U>()` for single-value handling. (Issue
#239 open question 2 — batching — is resolved this way: the burst *is* the batch.)

## Off-thread design

```
upstream ──active──▶ Feeder ──kanal──▶ worker thread ──ChannelSender──▶ ReceiverStream
                                                                              │ active
                                         feeder ──passive──▶ OffThreadInfer ◀─┘
```

- **`Feeder`** — a sink `MutableNode` with `active = [upstream]`. On each cycle it
  sends the upstream value to the worker over an **unbounded** kanal channel. The
  send never blocks the graph thread, preserving the "cycle never blocks"
  invariant. A closed channel (worker gone) drops the value silently — the result
  stream surfaces the failure.
- **worker thread** — runs inside `ReceiverStream`'s background thread. Blocks on
  the input channel with a 25 ms timeout (so it re-checks the stop flag), runs
  `infer`, and pushes results back via `ChannelSender` (which wakes the graph). On
  an inference error it sends `Message::Error` and stops; on channel close it sends
  `Message::EndOfStream` and exits cleanly.
- **`OffThreadInfer`** — the caller-facing node. Its active upstream is the
  `ReceiverStream` (so it ticks when predictions arrive); the `Feeder` is a
  **passive** upstream, which pulls the feeder into the graph without making it
  trigger this node. The feeder is still cycled by its own active dependency on
  `upstream`.

The `ReceiverStream` primitive (and `ChannelReceiverStream::finished`) is gated in
`src/nodes/mod.rs` / `src/nodes/channel.rs` behind the zmq/aeron adapters; the
`candle` feature was added to those `cfg` gates so the off-thread path can reuse it
rather than re-implementing graph-wakeup + thread lifecycle.

## Deviations from `/new-adapter`

| `/new-adapter` default | candle | Why |
|------------------------|--------|-----|
| `read.rs` / `write.rs` | `inference.rs` / `offthread.rs` | Transform node, no I/O direction. |
| `_sub` / `_pub` verbs | single `candle_infer` | Not pub/sub; it transforms one stream into another. |
| `integration_tests.rs` + `-integration-test` feature | inline `#[cfg(test)]` tests | No external service (Option C). Tests use a toy model. |
| testcontainers | none | No service to containerise. |
| Python bindings | none in v1 | The tensor-shaping closure API is Rust-native; deferred to v2 per issue #239 §8. |
| Standalone CI workflow | none | Covered by `cargo test --features candle` in the normal test job (issue #239 §9). |
| GPU feature flags (`candle-cuda`, `candle-metal`) | CPU only | `cargo lint-all` / CI build `--all-features`, which would force CUDA/Metal toolchains on CPU-only runners. GPU is a follow-up behind a dedicated workflow (issue #239 §9). |
| Branch named `candle` | `claude/new-adapter-issues-r34l0f` | Session working branch (task instruction override). |

## Pre-Commit Requirements

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features   # CI's `lint-all`
cargo test -p wingfoil --features candle candle::
cargo run -p wingfoil --example candle --features candle
```

There is no integration-test feature for this adapter; the unit tests above cover
both backends (inline determinism in historical mode, off-thread in real-time).

## Notes

- `candle-core` + `candle-nn` are pinned to `"0.10"`, optional, gated by the
  `candle` feature. `candle-nn` provides `VarBuilder` / `Linear` for loading
  weights and is used by the example and the loaded-module test.
- candle `Tensor` and `Device` are `Send`, so the off-thread worker carries them
  across the thread boundary safely. `Device` and `Tensor` are re-exported from
  `mod.rs` so callers don't need a direct `candle-core` dependency.
- Off-thread mode asserts `RunMode::RealTime` in `ReceiverStream::start`; a
  historical run fails fast (see `offthread_rejects_historical_mode`).
- Back-pressure (issue #239 open question 3) is currently unbounded: a worker
  slower than the input rate grows the kanal queue. A bounded channel with an
  explicit drop/error policy is the natural follow-up; unbounded keeps the graph
  thread non-blocking for v1.
