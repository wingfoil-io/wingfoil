# augurs Adapter Example (wingfoil-next)

On-graph time-series analysis with [augurs](https://github.com/grafana/augurs),
Grafana's pure-Rust time-series toolkit.

**No service to start** — augurs is a library, so this adapter is pure
computation on the graph clock rather than an I/O edge. That makes it the easiest
adapter example to run, and the one that best shows what "analysis as a node"
buys you: forecasts and detectors update incrementally as data arrives, over
sliding windows, without a batch job anywhere.

## Run

```sh
cargo run -p wingfoil-next --example augurs_adapter --features augurs
```

## What it drives

The example feeds synthetic streams through each of the adapter's six ops:

| # | Op | Input | What it prints |
|---|---|---|---|
| 1 | `augurs_forecast` | a noisy upward ramp | the 5-step-ahead ETS forecast and its 90% prediction interval, each tick |
| 2 | `augurs_outlier` | four series, three moving together and one diverging half-way | which series the MAD detector flags |
| 3 | `augurs_seasons` | a seasonal signal | the detected period |
| 4 | `augurs_changepoint` | a series with a regime shift | where the changepoint lands |
| 5 | `augurs_dtw` | five series — two tight pairs plus a wild one | pairwise dynamic-time-warping distances |
| 6 | `augurs_cluster` | the same five series | the DBSCAN cluster labels |

## Output

```text
== forecasting (ETS, 5 steps ahead, 90% interval) ==
  ...  next 5: [...]

== outlier detection (MAD over 4 series) ==
  ...  outlying series: [3]

== seasonality detection (periodogram) ==
  ...  seasonal period ~= 24 samples (true 24)

== changepoint detection (BOCPD) ==
  ...

== DTW distances + DBSCAN clustering (5 series) ==
  ...  distance from series 0:
    -> series 1: 0.42
    -> series 2: 8.13
```

Each section is driven on the graph clock, so the detectors report as the window
fills rather than once at the end.

## See also

- [`core/statistics`](../../core/statistics/) — the built-in rolling statistics,
  for when you need a mean rather than a model.
- [`core/ema_crossover`](../../core/ema_crossover/) — hand-rolled signal logic
  over the same kind of price stream.
