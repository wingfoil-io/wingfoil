//! augurs adapter example — on-graph forecasting and outlier detection.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example augurs --features augurs
//! ```
//!
//! augurs is a pure-Rust time-series toolkit, so there is no service to start.
//! This example drives synthetic streams through four of the adapter's
//! operators:
//!
//! 1. a noisy upward ramp fed to `augurs_forecast`, printing the 5-step-ahead
//!    forecast and its 90% prediction interval each tick;
//! 2. four monitored series — three moving together and one that diverges
//!    half-way through — fed to `augurs_outlier`, printing which series the MAD
//!    detector flags;
//! 3. a seasonal signal fed to `augurs_seasons`, printing the detected period;
//!    and
//! 4. a series with a regime shift fed to `augurs_changepoint`, printing where
//!    the changepoint lands.

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

fn seasonality() {
    println!("\n== seasonality detection (periodogram) ==");

    // A period-24 sine wave. The periodogram gives a coarse (approximate)
    // estimate, so it reports the season is present at roughly this scale.
    ticker(Duration::from_secs(1))
        .count()
        .map(|n| (n as f64 * std::f64::consts::TAU / 24.0).sin())
        .augurs_seasons(AugursSeasonsConfig::new(120))
        .for_each(|seasons, time| {
            if let Some(period) = seasons.dominant() {
                println!("  {time}  seasonal period ~= {period} samples (true 24)");
            }
        })
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(120))
        .unwrap();
}

fn changepoints() {
    println!("\n== changepoint detection (BOCPD) ==");

    // A series that jumps from ~0 to ~50 after tick 30.
    ticker(Duration::from_secs(1))
        .count()
        .map(|n| if n > 30 { 50.0 } else { 0.0 })
        .augurs_changepoint(AugursChangepointConfig::new(60))
        .for_each(|changes, time| {
            if changes.any() {
                println!(
                    "  {time}  changepoint at window index {:?}",
                    changes.indices
                );
            }
        })
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(50))
        .unwrap();
}

fn main() {
    env_logger::init();
    forecasting();
    outlier_detection();
    seasonality();
    changepoints();
}
