//! augurs adapter example — on-graph forecasting and outlier detection.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example augurs --features augurs
//! ```
//!
//! augurs is a pure-Rust time-series toolkit, so there is no service to start.
//! This example drives two synthetic streams through the adapter:
//!
//! 1. a noisy upward ramp fed to [`augurs_forecast`], printing the 5-step-ahead
//!    forecast and its 90% prediction interval each tick; and
//! 2. four monitored series — three moving together and one that diverges
//!    half-way through — fed to [`augurs_outlier`], printing which series the
//!    MAD detector flags.

use std::time::Duration;

use wingfoil::adapters::augurs::*;
use wingfoil::*;

fn forecasting() {
    println!("== forecasting (ETS, 5 steps ahead, 90% interval) ==");

    // A rising series with mild seasonality: n + sin(n/2).
    ticker(Duration::from_secs(1))
        .count()
        .map(|n| n as f64 + (n as f64 * 0.5).sin())
        .augurs_forecast(AugursForecastConfig::new(48, 5).with_level(0.90))
        .for_each(|forecast, time| {
            let point: Vec<String> = forecast.point.iter().map(|v| format!("{v:.1}")).collect();
            println!("  {time}  next 5: [{}]", point.join(", "));
        })
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(36))
        .unwrap();
}

fn outlier_detection() {
    println!("\n== outlier detection (MAD over 4 series) ==");

    // Per tick: four readings. Series 3 jumps away from the pack after tick 20.
    ticker(Duration::from_secs(1))
        .count()
        .map(|n| {
            let base = 100.0 + (n as f64 * 0.4).sin();
            let diverging = if n > 20 { base + 75.0 } else { base + 0.3 };
            vec![base, base + 0.1, base - 0.1, diverging]
        })
        .augurs_outlier(AugursOutlierConfig::new(40, 0.5))
        .for_each(|outliers, time| {
            if !outliers.outlying.is_empty() {
                println!("  {time}  outlying series: {:?}", outliers.outlying);
            }
        })
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(36))
        .unwrap();
}

fn main() {
    env_logger::init();
    forecasting();
    outlier_detection();
}
