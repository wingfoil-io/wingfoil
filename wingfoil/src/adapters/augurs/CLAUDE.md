# augurs Adapter

On-graph time-series analysis backed by the [`augurs`](https://docs.rs/augurs)
toolkit. Provides two transform operators:

- `augurs_forecast` — ETS forecasting over a sliding window of an `f64` stream.
- `augurs_outlier` — MAD outlier detection over a sliding window of a
  `Vec<f64>` stream (one value per series per tick).

## Module Structure

```
augurs/
  mod.rs        # Module doc, AugursForecast / AugursOutliers value types
  forecast.rs   # AugursForecastConfig, AugursForecastNode, AugursForecastOperators, tests
  outlier.rs    # AugursOutlierConfig, AugursOutlierNode, AugursOutlierOperators, tests
  CLAUDE.md     # This file
```

## Key Design Decisions

- **Not a service adapter.** augurs is a pure-Rust compute library — there is no
  endpoint, no Docker container, and no `async`. Consequently there is **no
  `augurs-integration-test` feature**; coverage lives in `#[cfg(test)]` blocks
  in `forecast.rs` / `outlier.rs` and runs under `--features augurs`. This is the
  skill's "Option C" (no external service).

- **Transform, not pub/sub.** The skill's `_sub` / `_pub` verbs do not fit a
  computation library, so the adapter exposes fluent stream operators
  (`.augurs_forecast(...)`, `.augurs_outlier(...)`) instead, mirroring core
  operators like `.window(...)` and `.fold(...)`.

- **Models are fitted inside `cycle()`.** This is pure CPU work on the
  single-threaded graph engine — no locks, no I/O, so it does not violate the
  "no locks on the graph path" invariant. It is, however, not free: `AutoETS`
  searches a small model space and the MAD detector recomputes medians each
  cycle. If a fresh fit every tick is too expensive, throttle the input with
  `.sample(...)` / `.throttle(...)` upstream — the idiomatic wingfoil answer
  rather than baking an interval into the node.

- **Forecast warm-up.** `AutoETS` requires more than ~8 points, so the forecast
  node does not tick until `min_points` (floored at 12) samples have arrived.

- **Outlier transpose.** Each tick of the input carries one reading per series;
  the node buffers the last `window` ticks and transposes them into aligned
  per-series columns before calling MAD. Short samples are forward-filled so the
  columns stay equal length. MAD needs ≥2 timestamps before it emits.

- **augurs error types are not `Send + Sync`.** `augurs::outlier::Error` cannot
  flow through `anyhow::Context`, so the outlier node renders it with
  `anyhow::anyhow!("...: {e}")`. The ETS error *is* `Send + Sync` and uses
  `.context(...)`.

## Gotchas

- Pinned to `augurs = "0.10.2"` with only the `ets` and `outlier` sub-features
  (`default-features = false`). Bumping the version may move the ETS / MAD APIs.
- MAD divides by the per-series median, so series whose median is `0.0` will
  surface a "division by zero" error from augurs. Feed it positive-scale data.
- `sensitivity` must be in `(0, 1)`; an out-of-range value panics at wiring time
  (in the operator, before the graph starts), which is acceptable for a factory.

## Pre-Commit Requirements

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
cargo test -p wingfoil --features augurs adapters::augurs
```
