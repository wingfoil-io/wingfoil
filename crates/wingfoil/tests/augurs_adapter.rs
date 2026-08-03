//! augurs adapter parity tests — ports the legacy
//! `wingfoil::adapters::augurs` unit tests for all six operators to the
//! wingfoil Op-pattern engine. augurs models are deterministic given their
//! input (AutoETS/MSTL are optimizers with no RNG; MAD/DBSCAN, BOCPD, the
//! periodogram and DTW are deterministic), so the same known input series yields
//! the same output on every run — the assertions match the legacy ones
//! (thresholds on the forecast / flagged-series, not brittle bit-exact values).
//! Everything runs in historical mode for reproducibility.

#![cfg(feature = "augurs")]

use std::time::Duration;

use wingfoil::adapters::augurs::{
    AugursChangepointConfig, AugursChangepointOps, AugursClusterConfig, AugursClusterOps,
    AugursDtwConfig, AugursDtwOps, AugursForecastConfig, AugursForecastOps, AugursOutlierConfig,
    AugursOutlierOps, AugursSeasonsConfig, AugursSeasonsOps,
};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

// -------------------------------------------------------------------------
// Forecasting.
// -------------------------------------------------------------------------

/// Legacy `forecast_ramp_predicts_ahead`: a clean upward ramp with mild
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

/// Legacy `forecast_waits_for_min_points`: the node stays silent until
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

/// Legacy `forecast_mstl_captures_season`: MSTL with a seasonal period
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

/// Legacy `forecast_window_below_floor_still_emits`: a `window` smaller than
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

/// Legacy `forecast_mstl_rejects_invalid_period`: an MSTL period below 2 is
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

/// Legacy `forecast_accepts_tuple_config`: `(window, horizon)` tuples convert
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

/// Legacy `outlier_mad_flags_diverging_series`: three series move together
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

/// Legacy `outlier_dbscan_flags_diverging_series`: DBSCAN flags a series
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

/// Legacy `outlier_quiet_when_aligned`: with all series moving together,
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

/// Legacy `outlier_waits_for_two_samples`: the node stays silent until it has
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

// -------------------------------------------------------------------------
// Changepoint detection.
// -------------------------------------------------------------------------

/// Legacy `changepoint_detects_level_shift`: a series that jumps from a low
/// mean to a high mean partway through produces a changepoint after the jump.
#[test]
fn changepoint_detects_level_shift() {
    let g = GraphBuilder::new();
    // First ~20 samples near 0, then a jump to near 50.
    let series = g.ticker(Duration::from_secs(1)).count().map(|&n| {
        let noise = (n as f64 * 0.7).sin() * 0.5;
        if n > 20 { 50.0 + noise } else { noise }
    });
    let changes = series.augurs_changepoint(AugursChangepointConfig::new(60));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(50))
        .unwrap();

    let last = r.value(&changes);
    assert!(
        last.any(),
        "expected a changepoint after the level shift, got {last:?}"
    );
    // The shift happens around window index 20; a detected changepoint should
    // land after the first few points.
    assert!(
        last.indices.iter().any(|&i| i >= 10),
        "expected a changepoint past the initial regime, got {:?}",
        last.indices
    );
}

/// Legacy `changepoint_quiet_when_steady`: a perfectly steady series yields no
/// changepoints.
#[test]
fn changepoint_quiet_when_steady() {
    let g = GraphBuilder::new();
    let series = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|&n| 10.0 + (n as f64 * 0.5).sin() * 0.1);
    let changes = series.augurs_changepoint(40);
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(40))
        .unwrap();
    let last = r.value(&changes);
    assert!(
        !last.any(),
        "steady series should have no changepoints, got {:?}",
        last.indices
    );
}

/// The changepoint op stays silent until `min_points` have arrived, and once it
/// starts ticking it ticks on every upstream tick (one per ticker period).
#[test]
fn changepoint_waits_for_min_points() {
    let g = GraphBuilder::new();
    let series = g.ticker(Duration::from_secs(1)).count().map(|&n| n as f64);
    let changes = series.augurs_changepoint(AugursChangepointConfig::new(40).with_min_points(10));
    let ticks = changes.with_time().accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(12))
        .unwrap();

    // The ticker's first tick is at t=0, so the 10th sample lands at t=9s and
    // the remaining three cycles each tick: one value per second, no gaps.
    let times: Vec<NanoTime> = r.value(&ticks).iter().map(|(t, _)| *t).collect();
    assert_eq!(
        times,
        vec![
            NanoTime::new(9_000_000_000),
            NanoTime::new(10_000_000_000),
            NanoTime::new(11_000_000_000),
        ]
    );
}

// -------------------------------------------------------------------------
// Seasonality detection.
// -------------------------------------------------------------------------

/// Legacy `seasons_detects_known_period`: a clean period-12 sine wave is
/// detected as having a ~12-sample season.
#[test]
fn seasons_detects_known_period() {
    let g = GraphBuilder::new();
    let series = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|&n| (n as f64 * std::f64::consts::TAU / 12.0).sin());
    let seasons = series.augurs_seasons(AugursSeasonsConfig::new(96));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(96))
        .unwrap();

    let last = r.value(&seasons);
    assert!(
        last.periods.iter().any(|&p| (10..=14).contains(&p)),
        "expected a period near 12, got {:?}",
        last.periods
    );
    let dominant = last.dominant().expect("a season was detected");
    assert!((10..=14).contains(&dominant), "dominant was {dominant}");
}

/// Legacy `seasons_window_below_floor_still_emits`: a `window` below the
/// default `min_points` floor still warms up and emits rather than never
/// ticking (the effective window grows to the floor).
#[test]
fn seasons_window_below_floor_still_emits() {
    let g = GraphBuilder::new();
    let series = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|&n| (n as f64 * std::f64::consts::TAU / 6.0).sin());
    // window 12 is below the default min_points floor of 16.
    let seasons = series.augurs_seasons(AugursSeasonsConfig::new(12));
    let ticks = seasons.accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(40))
        .unwrap();
    assert!(
        !r.value(&ticks).is_empty(),
        "should emit despite window < floor"
    );
}

/// Legacy `seasons_waits_for_min_points`: the op stays silent until
/// `min_points` have arrived.
#[test]
fn seasons_waits_for_min_points() {
    let g = GraphBuilder::new();
    let series = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|&n| (n as f64 * std::f64::consts::TAU / 12.0).sin());
    let seasons = series.augurs_seasons(AugursSeasonsConfig::new(96).with_min_points(50));
    let ticks = seasons.accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(20))
        .unwrap();
    assert!(r.value(&ticks).is_empty());
}

// -------------------------------------------------------------------------
// Dynamic time warping.
// -------------------------------------------------------------------------

/// Legacy `dtw_distances_reflect_similarity`: two similar series and one
/// dissimilar one — the distance from the odd series out exceeds the distance
/// between the similar pair.
#[test]
fn dtw_distances_reflect_similarity() {
    let g = GraphBuilder::new();
    // series 0 and 1 track each other; series 2 is scaled up and offset.
    let readings = g.ticker(Duration::from_secs(1)).count().map(|&n| {
        let t = n as f64;
        let a = (t * 0.3).sin();
        vec![a, a + 0.02, 5.0 * (t * 0.3).sin() + 10.0]
    });
    let dists = readings.augurs_dtw(AugursDtwConfig::new(30));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
        .unwrap();

    let m = r.value(&dists);
    assert_eq!(m.rows.len(), 3, "3x3 distance matrix");
    let d01 = m.get(0, 1).expect("in range");
    let d02 = m.get(0, 2).expect("in range");
    assert!(
        m.get(0, 0).expect("in range") < 1e-9,
        "self-distance is zero"
    );
    assert!(
        d02 > d01,
        "dissimilar series should be farther: d02={d02}, d01={d01}"
    );
}

/// Legacy `dtw_waits_for_two_samples`: with two series but only a single
/// sample the op stays silent — a DTW distance over length-1 columns is not a
/// windowed-history distance.
#[test]
fn dtw_waits_for_two_samples() {
    let g = GraphBuilder::new();
    let readings = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|&n| vec![n as f64, n as f64 + 1.0]);
    let dists = readings.augurs_dtw(8);
    let ticks = dists.accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
        .unwrap();
    assert!(r.value(&ticks).is_empty());
}

/// Legacy `dtw_waits_for_two_series`: the op stays silent until it has at
/// least two series.
#[test]
fn dtw_waits_for_two_series() {
    let g = GraphBuilder::new();
    let readings = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|&n| vec![n as f64]);
    let dists = readings.augurs_dtw(8);
    let ticks = dists.accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(10))
        .unwrap();
    assert!(r.value(&ticks).is_empty());
}

/// The Manhattan metric is selectable and still ranks a dissimilar series
/// farther away than a near-identical one.
#[test]
fn dtw_manhattan_metric_ranks_similarity() {
    use wingfoil::adapters::augurs::AugursDtwMetric;

    let g = GraphBuilder::new();
    let readings = g.ticker(Duration::from_secs(1)).count().map(|&n| {
        let t = n as f64;
        let a = (t * 0.3).sin();
        vec![a, a + 0.02, 5.0 * (t * 0.3).sin() + 10.0]
    });
    let dists =
        readings.augurs_dtw(AugursDtwConfig::new(30).with_metric(AugursDtwMetric::Manhattan));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
        .unwrap();

    let m = r.value(&dists);
    assert_eq!(m.rows.len(), 3);
    assert!(m.get(0, 2).expect("in range") > m.get(0, 1).expect("in range"));
}

// -------------------------------------------------------------------------
// Clustering.
// -------------------------------------------------------------------------

/// Legacy `cluster_groups_similar_series`: two tight groups of series plus one
/// outlier form two clusters, with the outlier labelled as noise.
#[test]
fn cluster_groups_similar_series() {
    let g = GraphBuilder::new();
    // series 0,1 near a low sine; series 2,3 near a high sine; series 4 wild.
    let readings = g.ticker(Duration::from_secs(1)).count().map(|&n| {
        let t = n as f64;
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
    let clusters = readings.augurs_cluster(AugursClusterConfig::new(30, 1.0, 2));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
        .unwrap();

    let last = r.value(&clusters);
    assert_eq!(last.labels.len(), 5, "one label per series");
    // series 0 and 1 belong to the same (non-noise) cluster.
    assert!(last.labels[0] >= 0 && last.labels[0] == last.labels[1]);
    // series 2 and 3 belong to the same cluster, distinct from 0/1.
    assert!(last.labels[2] >= 0 && last.labels[2] == last.labels[3]);
    assert_ne!(last.labels[0], last.labels[2]);
    assert_eq!(last.cluster_count(), 2, "expected two clusters");
    // the wild series is noise.
    assert_eq!(last.labels[4], -1);
}

/// Legacy `cluster_waits_for_two_series`: the op stays silent until it has at
/// least two series.
#[test]
fn cluster_waits_for_two_series() {
    let g = GraphBuilder::new();
    let readings = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|&n| vec![n as f64]);
    let clusters = readings.augurs_cluster(AugursClusterConfig::new(8, 1.0, 2));
    let ticks = clusters.accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(10))
        .unwrap();
    assert!(r.value(&ticks).is_empty());
}

/// Legacy `cluster_waits_for_two_samples`: with two series but only a single
/// sample the op stays silent — clustering length-1 columns compares single
/// points, not histories.
#[test]
fn cluster_waits_for_two_samples() {
    let g = GraphBuilder::new();
    let readings = g
        .ticker(Duration::from_secs(1))
        .count()
        .map(|&n| vec![n as f64, n as f64 + 1.0]);
    let clusters = readings.augurs_cluster(AugursClusterConfig::new(8, 1.0, 2));
    let ticks = clusters.accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
        .unwrap();
    assert!(r.value(&ticks).is_empty());
}

/// Legacy `cluster_accepts_tuple_config`: `(window, epsilon,
/// min_cluster_size)` tuples convert into a config.
#[test]
fn cluster_accepts_tuple_config() {
    let g = GraphBuilder::new();
    let readings = g.ticker(Duration::from_secs(1)).count().map(|&n| {
        let t = n as f64;
        let low = (t * 0.3).sin();
        vec![low, low + 0.02, low + 20.0, low + 20.02]
    });
    let clusters = readings.augurs_cluster((30, 1.0, 2));
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
        .unwrap();
    assert_eq!(r.value(&clusters).labels.len(), 4);
}

/// Next-specific: a `window` of 1 still reaches the two-sample warm-up floor
/// and emits (legacy's cluster node would never tick — see the adapter's
/// `# Deviations from legacy`).
#[test]
fn cluster_window_below_floor_still_emits() {
    let g = GraphBuilder::new();
    let readings = g.ticker(Duration::from_secs(1)).count().map(|&n| {
        let t = n as f64;
        vec![t, t + 0.01, t + 20.0]
    });
    let clusters = readings.augurs_cluster(AugursClusterConfig::new(1, 1.0, 2));
    let ticks = clusters.accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
        .unwrap();
    assert!(
        !r.value(&ticks).is_empty(),
        "should emit despite window < the two-sample floor"
    );
}
