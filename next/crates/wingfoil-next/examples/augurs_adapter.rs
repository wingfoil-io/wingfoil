//! augurs adapter example — on-graph forecasting and outlier detection.
//!
//! Run with the `augurs` feature:
//!
//! ```sh
//! cargo run -p wingfoil-next --features augurs --example augurs_adapter
//! ```
//!
//! augurs is a pure-Rust time-series toolkit, so there is no service to start.
//! wingfoil-next currently ports two of the classic adapter's six operators —
//! forecasting and outlier detection. (The other four — seasonality,
//! changepoint detection, DTW, and clustering — remain classic-only for now, a
//! tracked capability gap; see `next/docs/port-plan.md` and the deviation
//! register C5.) This example
//! drives a synthetic stream through each op, on the graph clock:
//!
//! 1. a noisy upward ramp fed to `augurs_forecast`, printing the 5-step-ahead
//!    forecast and its 90% prediction interval each tick; and
//! 2. four monitored series — three moving together and one that diverges
//!    half-way through — fed to `augurs_outlier`, printing which series the MAD
//!    detector flags.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::augurs::{
    AugursForecastConfig, AugursForecastOps, AugursOutlierConfig, AugursOutlierOps,
};
use wingfoil_next::prelude::*;

fn forecasting() -> anyhow::Result<()> {
    println!("== forecasting (ETS, 5 steps ahead, 90% interval) ==");

    let g = GraphBuilder::new();

    // A rising series with mild seasonality: n + sin(n/2).
    let forecast = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|n| *n as f64 + (*n as f64 * 0.5).sin())
        .augurs_forecast(AugursForecastConfig::new(48, 5).with_level(0.90));

    let _sink = forecast.with_time().for_each(|(time, forecast)| {
        let point: Vec<String> = forecast.point.iter().map(|v| format!("{v:.1}")).collect();
        println!("  {time}  next 5: [{}]", point.join(", "));
        Ok(())
    });

    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(36))?;
    Ok(())
}

fn outlier_detection() -> anyhow::Result<()> {
    println!("\n== outlier detection (MAD over 4 series) ==");

    let g = GraphBuilder::new();

    // Per tick: four readings. Series 3 jumps away from the pack after tick 20.
    let outliers = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|n| {
            let base = 100.0 + (*n as f64 * 0.4).sin();
            let diverging = if *n > 20 { base + 75.0 } else { base + 0.3 };
            vec![base, base + 0.1, base - 0.1, diverging]
        })
        .augurs_outlier(AugursOutlierConfig::new(40, 0.5));

    let _sink = outliers.with_time().for_each(|(time, outliers)| {
        if !outliers.outlying.is_empty() {
            println!("  {time}  outlying series: {:?}", outliers.outlying);
        }
        Ok(())
    });

    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(36))?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    forecasting()?;
    outlier_detection()?;
    Ok(())
}
