# augurs Adapter Example

Demonstrates on-graph time-series analysis with the
[`augurs`](https://docs.rs/augurs) adapter. augurs is a pure-Rust toolkit, so
there is nothing to install or run — the example drives two synthetic streams:

1. **forecasting** — a noisy upward ramp is fed to `augurs_forecast`, which
   buffers a sliding window, fits an ETS model and emits a 5-step-ahead forecast
   with a 90% prediction interval each tick.
2. **outlier detection** — four monitored series (three moving together, one
   that diverges half-way through) are fed to `augurs_outlier`, which flags the
   diverging series via the MAD detector.

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
  12000000000  next 5: [14.2, 15.2, 16.1, 17.1, 18.1]
  ...
  35000000000  next 5: [36.2, 37.2, 38.1, 39.1, 40.1]

== outlier detection (MAD over 4 series) ==
  20000000000  outlying series: [3]
  21000000000  outlying series: [3]
  ...
  35000000000  outlying series: [3]
```
