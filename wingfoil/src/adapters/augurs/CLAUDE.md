# augurs Adapter

On-graph time-series analysis backed by the [`augurs`](https://docs.rs/augurs)
toolkit. Provides six transform operators over sliding windows:

| Operator | Input stream | Emits |
|---|---|---|
| `augurs_forecast` | `f64` | `AugursForecast` (point + intervals); ETS or MSTL |
| `augurs_outlier` | `Vec<f64>` | `AugursOutliers`; MAD or DBSCAN detector |
| `augurs_changepoint` | `f64` | `AugursChangepoints` (BOCPD indices) |
| `augurs_seasons` | `f64` | `AugursSeasons` (periodogram periods) |
| `augurs_dtw` | `Vec<f64>` | `AugursDistanceMatrix` (pairwise DTW) |
| `augurs_cluster` | `Vec<f64>` | `AugursClusters` (DBSCAN labels via DTW) |

The `Vec<f64>` operators take one reading per series per tick.

## Module Structure

```
augurs/
  mod.rs         # Module doc, value types, transpose_window() helper
  forecast.rs    # ETS + MSTL forecasting
  outlier.rs     # MAD + DBSCAN outlier detection
  changepoint.rs # Bayesian online changepoint detection
  seasons.rs     # periodogram seasonality detection
  dtw.rs         # dynamic time warping distance matrix
  cluster.rs     # DBSCAN clustering over the DTW distance matrix
  CLAUDE.md      # This file
```

## Key Design Decisions

- **Not a service adapter.** augurs is a pure-Rust compute library — there is no
  endpoint, no Docker container, and no `async`. Consequently there is **no
  `augurs-integration-test` feature**; coverage lives in `#[cfg(test)]` blocks in
  each file and runs under `--features augurs`. This is the skill's "Option C".

- **Transform, not pub/sub.** The skill's `_sub` / `_pub` verbs do not fit a
  computation library, so the adapter exposes fluent stream operators mirroring
  core operators like `.window(...)` and `.fold(...)`.

- **Models are fitted inside `cycle()`.** Pure CPU work on the single-threaded
  graph engine — no locks, no I/O, so the "no locks on the graph path" invariant
  holds. It is not free, though: each cycle refits/recomputes over the whole
  window (ETS model search, DTW is `O(n² · window²)`, BOCPD re-scans the window).
  Throttle the input with `.sample(...)` / `.throttle(...)` upstream if a fresh
  result every tick is too expensive.

- **Multi-series transpose.** The `Vec<f64>` operators buffer the last `window`
  ticks and transpose them into aligned per-series columns via the shared
  `transpose_window()` helper in `mod.rs`. Short samples are forward-filled so
  columns stay equal length.

- **Model / detector choice via config, not new operators.** `augurs_forecast`
  takes an `AugursForecastModel` (`Ets` | `Mstl{periods}`) and `augurs_outlier`
  an `AugursOutlierDetector` (`Mad` | `Dbscan`), keeping one operator per
  concern. `AugursForecastConfig::mstl(periods)` and
  `AugursOutlierConfig::dbscan(...)` are the ergonomic constructors.

- **Warm-up floors.** ETS needs >~8 points (floored at 12). MSTL/STL cannot
  decompose fewer than **two full seasonal periods**, so the forecast floor
  becomes `2 * max_period + 1` for MSTL. Multi-series operators need ≥2 series.

- **BOCPD index 0 is dropped.** Bayesian online changepoint detection always
  reports index 0 (the window start) as a changepoint; the node filters it out
  so only genuine internal regime changes are emitted.

- **augurs error types are not all `Send + Sync`.** `augurs::outlier::Error`
  cannot flow through `anyhow::Context`, so the outlier node renders it with
  `anyhow::anyhow!("...: {e}")`. The ETS/MSTL errors *are* `Send + Sync` and use
  `.context(...)`.

## Gotchas

- Pinned to `augurs = "0.10.2"` with `default-features = false` and the sub-
  features `ets, mstl, outlier, changepoint, seasons, dtw, clustering`. Bumping
  the version may move these APIs. **Prophet is deliberately excluded** — it
  needs a bundled Stan toolchain (cmdstan/wasmstan), a heavy external build that
  does not fit a lightweight pure-Rust adapter.
- MAD divides by the per-series median, so series whose median is `0.0` surface a
  "division by zero" error from augurs. Feed it positive-scale data.
- DBSCAN outlier detection needs **at least three** series to form a cluster;
  with fewer it reports nothing.
- `sensitivity` must be in `(0, 1)`; an out-of-range value panics at wiring time
  (in the operator, before the graph starts), which is acceptable for a factory.

## Pre-Commit Requirements

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
cargo test -p wingfoil --features augurs adapters::augurs
```
