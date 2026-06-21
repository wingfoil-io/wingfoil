# candle Adapter Example

Short-horizon price forecasting with a small neural network. A deterministic
synthetic price stream is fed through a two-layer perceptron (`1 → 8 → 1`, tanh
hidden) that predicts the next-step price *delta*, using the
[`candle`](https://github.com/huggingface/candle) adapter's `candle_infer`.

The model runs **inline** — its `forward()` executes on the graph thread inside
each cycle — which is the right mode for a network this small. The weights are
fixed in code so the run is fully reproducible and needs no downloaded
checkpoint; in production you would load weights from a safetensors / GGUF file
via `candle_nn::VarBuilder` and capture the model in the same `forward` closure.

For a heavy model whose `forward()` would stall the cycle, switch
`CandleConfig::inline()` to `CandleConfig::off_thread()` (real-time only) and the
inference runs on a dedicated worker thread instead.

## Run

```sh
cargo run --example candle --features candle
```

## Output

```text
"price =  102.147   next-step forecast =  102.226"
"price =  103.987   next-step forecast =  104.184"
"price =  105.260   next-step forecast =  105.537"
"price =  105.798   next-step forecast =  106.108"
"price =  105.546   next-step forecast =  105.841"
"price =  104.577   next-step forecast =  104.812"
"price =  103.075   next-step forecast =  103.214"
"price =  101.308   next-step forecast =  101.333"
"price =   99.587   next-step forecast =   99.501"
"price =   98.216   next-step forecast =   98.041"
```
