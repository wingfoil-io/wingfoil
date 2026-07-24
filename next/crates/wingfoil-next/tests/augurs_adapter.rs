//! augurs adapter parity tests — ports the classic
//! `wingfoil::adapters::augurs` forecast + outlier unit tests to the
//! wingfoil-next Op-pattern engine. augurs models are deterministic given their
//! input (AutoETS/MSTL are optimizers with no RNG; MAD/DBSCAN are deterministic
//! detectors), so the same known input series yields the same output on every
//! run — the assertions match the classic ones (thresholds on the forecast /
//! flagged-series, not brittle bit-exact values). Everything runs in historical
//! mode for reproducibility.

#![cfg(feature = "augurs")]

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::augurs::{
    AugursForecastConfig, AugursForecastOps, AugursOutlierConfig, AugursOutlierOps,
};
use wingfoil_next::prelude::*;

// -------------------------------------------------------------------------
// Forecasting.
// -------------------------------------------------------------------------

/// Classic `forecast_ramp_predicts_ahead`: a clean upward ramp with mild
/// seasonality forecasts values above the last observed point, and the
/// prediction intervals bracket the point forecast.
#[test]
fn forecast_ramp_predicts_ahead() {
    let g = GraphBuilder::new();
    let source = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|&n| n as f64 + (n as f64 * 0.5).sin());
    let forecast = source.augurs_forecast(AugursForecastConfig::new(48, 3).with_level(0.9));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(40))
        .unwrap();

    let last = r.value(&forecast);
    assert_eq!(last.point.len(), 3, "horizon == 3 point forecasts");
    assert_eq!(last.lower.len(), 3);
    assert_eq!(last.upper.len(), 3);
    // The ramp is still rising, so the next forecast should exceed ~30.
    assert!(
        last.next().unwrap() > 30.0,
        "expected forecast to continue the ramp, got {:?}",
        last.point
    );
    // Intervals must bracket the point forecast.
    for i in 0..3 {
        assert!(last.lower[i] <= last.point[i]);
        assert!(last.upper[i] >= last.point[i]);
    }
}

/// Classic `forecast_waits_for_min_points`: the node stays silent until
/// `min_points` have arrived.
#[test]
fn forecast_waits_for_min_points() {
    let g = GraphBuilder::new();
    let source = g.ticker(Duration::from_secs(1)).count().map(|&n| n as f64);
    let forecast = source.augurs_forecast(AugursForecastConfig::new(64, 1).with_min_points(20));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(15))
        .unwrap();
    // 15 cycles < 20 min_points → never ticked, slot holds the default.
    assert!(r.value(&forecast).point.is_empty());
}

/// Classic `forecast_mstl_captures_season`: MSTL with a seasonal period
/// reproduces the seasonal swing rather than a flat line.
#[test]
fn forecast_mstl_captures_season() {
    let g = GraphBuilder::new();
    // Period-12 sine wave riding on a gentle ramp.
    let source = g.ticker(Duration::from_secs(1)).count().map(|&n| {
        let t = n as f64;
        0.1 * t + 5.0 * (t * std::f64::consts::TAU / 12.0).sin()
    });
    let forecast = source.augurs_forecast(AugursForecastConfig::new(120, 12).mstl(vec![12]));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(80))
        .unwrap();

    let last = r.value(&forecast);
    assert_eq!(last.point.len(), 12, "horizon == 12 point forecasts");
    // A seasonal forecast should swing by a meaningful fraction of the
    // amplitude (10.0 peak-to-peak).
    let max = last.point.iter().cloned().fold(f64::MIN, f64::max);
    let min = last.point.iter().cloned().fold(f64::MAX, f64::min);
    assert!(
        max - min > 2.0,
        "expected a seasonal swing, got range {:.3} for {:?}",
        max - min,
        last.point
    );
}

/// Classic `forecast_window_below_floor_still_emits`: a `window` smaller than
/// the model's warm-up floor still warms up and emits.
#[test]
fn forecast_window_below_floor_still_emits() {
    let g = GraphBuilder::new();
    let source = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|&n| n as f64 + (n as f64 * 0.5).sin());
    // window 10 is below the ETS floor of 12.
    let forecast = source.augurs_forecast(AugursForecastConfig::new(10, 2));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
        .unwrap();
    assert_eq!(
        r.value(&forecast).point.len(),
        2,
        "should emit despite window < floor"
    );
}

/// Classic `forecast_mstl_rejects_invalid_period`: an MSTL period below 2 is
/// rejected with a clear error at run time.
#[test]
fn forecast_mstl_rejects_invalid_period() {
    let g = GraphBuilder::new();
    let source = g.ticker(Duration::from_secs(1)).count().map(|&n| n as f64);
    let _forecast = source.augurs_forecast(AugursForecastConfig::new(64, 2).mstl(vec![1]));
    let mut r = g.build();
    let result = r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30));
    let err = result.expect_err("period < 2 should fail the run");
    assert!(
        format!("{err:#}").contains("MSTL periods"),
        "expected a clear MSTL period error, got {err:#}"
    );
}

/// Classic `forecast_accepts_tuple_config`: `(window, horizon)` tuples convert
/// into a config.
#[test]
fn forecast_accepts_tuple_config() {
    let g = GraphBuilder::new();
    let source = g.ticker(Duration::from_secs(1)).count().map(|&n| n as f64);
    let forecast = source.augurs_forecast((32, 2));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
        .unwrap();
    assert_eq!(r.value(&forecast).point.len(), 2);
}

// -------------------------------------------------------------------------
// Outlier detection.
// -------------------------------------------------------------------------

/// Classic `outlier_mad_flags_diverging_series`: three series move together
/// except one, which jumps away part-way through — flagged by MAD.
#[test]
fn outlier_mad_flags_diverging_series() {
    let g = GraphBuilder::new();
    // Per tick: [series0, series1, series2]. Series 2 spikes after a while.
    let readings = g.ticker(Duration::from_secs(1)).count().map(|&n| {
        let base = 100.0 + (n as f64 * 0.4).sin();
        let series2 = if n > 20 { base + 80.0 } else { base + 0.2 };
        vec![base, base + 0.1, series2]
    });
    let outliers = readings.augurs_outlier(AugursOutlierConfig::new(40, 0.5));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(40))
        .unwrap();

    let last = r.value(&outliers);
    assert_eq!(last.scores.len(), 3, "one score per series");
    assert!(
        last.is_outlier(2),
        "series 2 diverged and should be flagged, got {last:?}"
    );
    assert!(!last.is_outlier(0));
    assert!(!last.is_outlier(1));
}

/// Classic `outlier_dbscan_flags_diverging_series`: DBSCAN flags a series
/// diverging from a cluster of similar series.
#[test]
fn outlier_dbscan_flags_diverging_series() {
    let g = GraphBuilder::new();
    // Four series: three cluster together, series 3 diverges.
    let readings = g.ticker(Duration::from_secs(1)).count().map(|&n| {
        let base = 100.0 + (n as f64 * 0.4).sin();
        let series3 = if n > 15 { base + 90.0 } else { base + 0.3 };
        vec![base, base + 0.1, base - 0.1, series3]
    });
    let outliers = readings.augurs_outlier(AugursOutlierConfig::dbscan(40, 0.5));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(40))
        .unwrap();

    let last = r.value(&outliers);
    assert_eq!(last.scores.len(), 4);
    assert!(
        last.is_outlier(3),
        "series 3 diverged and should be flagged by DBSCAN, got {last:?}"
    );
}

/// Classic `outlier_quiet_when_aligned`: with all series moving together,
/// nothing is flagged.
#[test]
fn outlier_quiet_when_aligned() {
    let g = GraphBuilder::new();
    let readings = g.ticker(Duration::from_secs(1)).count().map(|&n| {
        let base = 50.0 + (n as f64 * 0.3).sin();
        vec![base, base + 0.05, base - 0.05]
    });
    let outliers = readings.augurs_outlier((30, 0.5));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
        .unwrap();
    let last = r.value(&outliers);
    assert!(
        last.outlying.is_empty(),
        "aligned series should produce no outliers, got {last:?}"
    );
}

/// Classic `outlier_waits_for_two_samples`: the node stays silent until it has
/// at least two samples.
#[test]
fn outlier_waits_for_two_samples() {
    let g = GraphBuilder::new();
    let readings = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|&n| vec![n as f64, n as f64 + 1.0]);
    let outliers = readings.augurs_outlier((8, 0.5));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
        .unwrap();
    // One cycle → fewer than two samples → never ticked, slot holds default.
    let last = r.value(&outliers);
    assert!(last.outlying.is_empty() && last.scores.is_empty());
}
