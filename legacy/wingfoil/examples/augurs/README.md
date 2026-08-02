# augurs Adapter Example

Demonstrates on-graph time-series analysis with the
[`augurs`](https://docs.rs/augurs) adapter. augurs is a pure-Rust toolkit, so
there is nothing to install or run — the example drives synthetic streams
through four of the adapter's operators:

1. **forecasting** — a noisy upward ramp is fed to `augurs_forecast`, which
   buffers a sliding window, fits an ETS model and emits a 5-step-ahead forecast
   with a 90% prediction interval each tick.
2. **outlier detection** — four monitored series (three moving together, one
   that diverges half-way through) are fed to `augurs_outlier`, which flags the
   diverging series via the MAD detector.
3. **seasonality detection** — a period-24 sine wave is fed to `augurs_seasons`,
   which reports the dominant seasonal period.
4. **changepoint detection** — a series that jumps regime is fed to
   `augurs_changepoint`, which reports where the change occurs.

The adapter also provides `augurs_dtw` (dynamic time warping distance matrix)
and `augurs_cluster` (DBSCAN clustering of series), plus MSTL seasonal
forecasting and DBSCAN outlier detection — see the module docs.

## Setup

None — augurs runs in-process.

## Run

```sh
cargo run --example augurs --features augurs
```

## Code

See `main.rs` for the full source.

## Output

```
== forecasting (ETS, 5 steps ahead, 90% interval) ==
  11000000000  next 5: [12.8, 13.9, 15.0, 16.1, 17.1]
  ...
  35000000000  next 5: [36.2, 37.2, 38.1, 39.1, 40.1]

== outlier detection (MAD over 4 series) ==
  20000000000  outlying series: [3]
  ...
  35000000000  outlying series: [3]

== seasonality detection (periodogram) ==
  ...
  119000000000  seasonal period ~= 30 samples (true 24)

== changepoint detection (BOCPD) ==
  30000000000  changepoint at window index [29]
  ...
  49000000000  changepoint at window index [29]
```

(The periodogram gives a coarse period estimate — `~=30` for a true period of
24 — so it flags the presence and rough scale of a season, not the exact value.)
