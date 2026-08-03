//! augurs adapter example — on-graph time-series analysis.
//!
//! Run with the `augurs` feature:
//!
//! ```sh
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --features augurs --example augurs_adapter
//! ```
//!
//! augurs is a pure-Rust time-series toolkit, so there is no service to start.
//! This example drives synthetic streams through each of the adapter's six ops,
//! on the graph clock:
//!
//! 1. a noisy upward ramp fed to `augurs_forecast`, printing the 5-step-ahead
//!    forecast and its 90% prediction interval each tick;
//! 2. four monitored series — three moving together and one that diverges
//!    half-way through — fed to `augurs_outlier`, printing which series the MAD
//!    detector flags;
//! 3. a seasonal signal fed to `augurs_seasons`, printing the detected period;
//! 4. a series with a regime shift fed to `augurs_changepoint`, printing where
//!    the changepoint lands; and
//! 5. five series — two tight pairs plus a wild one — fed to `augurs_dtw` and
//!    `augurs_cluster`, printing the pairwise warping distances and the DBSCAN
//!    cluster labels they produce.

use std::time::Duration;

use wingfoil::adapters::augurs::{
    AugursChangepointConfig, AugursChangepointOps, AugursClusterConfig, AugursClusterOps,
    AugursDtwConfig, AugursDtwOps, AugursForecastConfig, AugursForecastOps, AugursOutlierConfig,
    AugursOutlierOps, AugursSeasonsConfig, AugursSeasonsOps,
};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

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

fn seasonality() -> anyhow::Result<()> {
    println!("\n== seasonality detection (periodogram) ==");

    let g = GraphBuilder::new();

    // A period-24 sine wave. The periodogram gives a coarse (approximate)
    // estimate, so it reports the season is present at roughly this scale.
    let seasons = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|n| (*n as f64 * std::f64::consts::TAU / 24.0).sin())
        .augurs_seasons(AugursSeasonsConfig::new(120));

    let _sink = seasons.with_time().for_each(|(time, seasons)| {
        if let Some(period) = seasons.dominant() {
            println!("  {time}  seasonal period ~= {period} samples (true 24)");
        }
        Ok(())
    });

    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(120))?;
    Ok(())
}

fn changepoints() -> anyhow::Result<()> {
    println!("\n== changepoint detection (BOCPD) ==");

    let g = GraphBuilder::new();

    // A series that jumps from ~0 to ~50 after tick 30.
    let changes = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|n| if *n > 30 { 50.0 } else { 0.0 })
        .augurs_changepoint(AugursChangepointConfig::new(60));

    let _sink = changes.with_time().for_each(|(time, changes)| {
        if changes.any() {
            println!(
                "  {time}  changepoint at window index {:?}",
                changes.indices
            );
        }
        Ok(())
    });

    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(50))?;
    Ok(())
}

fn similarity() -> anyhow::Result<()> {
    println!("\n== DTW distances + DBSCAN clustering (5 series) ==");

    let g = GraphBuilder::new();

    // Series 0/1 track a low sine, series 2/3 a high one, series 4 is wild.
    let readings = g.ticker(Duration::from_secs(1)).count().map(|n| {
        let t = *n as f64;
        let low = (t * 0.3).sin();
        let high = (t * 0.3).sin() + 20.0;
        vec![
            low,
            low + 0.02,
            high,
            high + 0.02,
            50.0 * (t * 0.9).cos() + 100.0,
        ]
    });

    let distances = readings.augurs_dtw(AugursDtwConfig::new(30));
    let clusters = readings.augurs_cluster(AugursClusterConfig::new(30, 1.0, 2));

    // Report once, on the last cycle, when the window is full.
    let _sink = distances
        .join(&clusters, |d, c| (d.clone(), c.clone()))
        .with_time()
        .for_each(|(time, (distances, clusters))| {
            if *time == NanoTime::new(29_000_000_000) {
                println!("  {time}  distance from series 0:");
                for (j, d) in distances.rows[0].iter().enumerate() {
                    println!("    -> series {j}: {d:.2}");
                }
                println!(
                    "  {time}  cluster labels (-1 = noise): {:?}",
                    clusters.labels
                );
            }
            Ok(())
        });

    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    forecasting()?;
    outlier_detection()?;
    seasonality()?;
    changepoints()?;
    similarity()?;
    Ok(())
}
