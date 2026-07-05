use std::collections::VecDeque;
use std::rc::Rc;

use augurs::outlier::{DbscanDetector, MADDetector, OutlierDetector, OutlierOutput};

use super::AugursOutliers;
use crate::types::*;

/// Which outlier-detection algorithm [`AugursOutlierOperators::augurs_outlier`]
/// uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AugursOutlierDetector {
    /// Median absolute deviation. Flags series whose values sit far from the
    /// rolling median of the group. Works with any number of series (including
    /// one) and is robust to a few out-of-sync members.
    #[default]
    Mad,
    /// DBSCAN density clustering. Flags series that fall outside the main
    /// cluster of similarly-behaving series. Needs **at least three** series to
    /// form a cluster; with fewer it reports nothing.
    Dbscan,
}

/// Configuration for [`AugursOutlierOperators::augurs_outlier`].
#[derive(Debug, Clone)]
pub struct AugursOutlierConfig {
    /// Number of recent samples retained as the detection window.
    pub window: usize,
    /// Detector sensitivity, strictly between 0 and 1. Higher is more
    /// sensitive. Used to derive a detection threshold from the scale of the
    /// data.
    pub sensitivity: f64,
    /// Which detector to run.
    pub detector: AugursOutlierDetector,
}

impl AugursOutlierConfig {
    /// Detect over the last `window` samples at the given `sensitivity` (must be
    /// in `(0, 1)`) using the default [MAD](AugursOutlierDetector::Mad)
    /// detector.
    #[must_use]
    pub fn new(window: usize, sensitivity: f64) -> Self {
        Self {
            window,
            sensitivity,
            detector: AugursOutlierDetector::Mad,
        }
    }

    /// A MAD-detector config (same as [`new`](Self::new); reads clearer at call
    /// sites that also use [`dbscan`](Self::dbscan)).
    #[must_use]
    pub fn mad(window: usize, sensitivity: f64) -> Self {
        Self::new(window, sensitivity)
    }

    /// A [DBSCAN](AugursOutlierDetector::Dbscan)-detector config. Needs at least
    /// three series to report anything.
    #[must_use]
    pub fn dbscan(window: usize, sensitivity: f64) -> Self {
        Self {
            window,
            sensitivity,
            detector: AugursOutlierDetector::Dbscan,
        }
    }
}

impl From<(usize, f64)> for AugursOutlierConfig {
    /// `(window, sensitivity)` using the default MAD detector.
    fn from((window, sensitivity): (usize, f64)) -> Self {
        Self::new(window, sensitivity)
    }
}

/// A constructed detector. `OutlierDetector` has an associated `PreprocessedData`
/// type so it is not object-safe; this enum dispatches over the two concrete
/// implementations instead.
enum Detector {
    Mad(MADDetector),
    Dbscan(DbscanDetector),
}

impl Detector {
    fn run(&self, series: &[&[f64]]) -> anyhow::Result<OutlierOutput> {
        // augurs' outlier `Error` is not `Send + Sync`, so it cannot flow
        // through `anyhow::Context`; render it into an `anyhow::Error` instead.
        match self {
            Detector::Mad(d) => {
                let pre = d
                    .preprocess(series)
                    .map_err(|e| anyhow::anyhow!("augurs_outlier: MAD preprocess failed: {e}"))?;
                d.detect(&pre)
                    .map_err(|e| anyhow::anyhow!("augurs_outlier: MAD detect failed: {e}"))
            }
            Detector::Dbscan(d) => {
                let pre = d.preprocess(series).map_err(|e| {
                    anyhow::anyhow!("augurs_outlier: DBSCAN preprocess failed: {e}")
                })?;
                d.detect(&pre)
                    .map_err(|e| anyhow::anyhow!("augurs_outlier: DBSCAN detect failed: {e}"))
            }
        }
    }
}

pub(crate) struct AugursOutlierNode {
    upstream: Rc<dyn Stream<Vec<f64>>>,
    detector: Detector,
    window: usize,
    buffer: VecDeque<Vec<f64>>,
    value: AugursOutliers,
}

impl AugursOutlierNode {
    fn new(upstream: Rc<dyn Stream<Vec<f64>>>, detector: Detector, window: usize) -> Self {
        Self {
            upstream,
            detector,
            window: window.max(2),
            buffer: VecDeque::with_capacity(window.max(2)),
            value: AugursOutliers::default(),
        }
    }
}

#[node(active = [upstream], output = value: AugursOutliers)]
impl MutableNode for AugursOutlierNode {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.buffer.push_back(self.upstream.peek_value());
        while self.buffer.len() > self.window {
            self.buffer.pop_front();
        }
        // Need at least two timestamps to have any spread to measure.
        if self.buffer.len() < 2 {
            return Ok(false);
        }

        // Transpose the buffered samples (one value per series per tick) into
        // aligned per-series time series.
        let series = super::transpose_window(&self.buffer);
        if series.is_empty() {
            return Ok(false);
        }
        let refs: Vec<&[f64]> = series.iter().map(Vec::as_slice).collect();
        let output = self.detector.run(&refs)?;

        self.value = AugursOutliers {
            outlying: output.outlying_series.iter().copied().collect(),
            scores: output
                .series_results
                .iter()
                .map(|s| s.scores.last().copied().unwrap_or(0.0))
                .collect(),
        };
        Ok(true)
    }
}

/// Adds the [`augurs_outlier`](AugursOutlierOperators::augurs_outlier) operator
/// to streams of per-series readings.
pub trait AugursOutlierOperators {
    /// Maintain a sliding window of per-series readings (one `f64` per series
    /// per tick) and emit an [`AugursOutliers`] each tick once at least two
    /// samples have arrived, flagging series that deviate from the group via the
    /// configured detector.
    ///
    /// # Panics
    ///
    /// Panics at wiring time if `sensitivity` is not in `(0, 1)`.
    #[must_use]
    fn augurs_outlier(
        self: &Rc<Self>,
        config: impl Into<AugursOutlierConfig>,
    ) -> Rc<dyn Stream<AugursOutliers>>;
}

impl AugursOutlierOperators for dyn Stream<Vec<f64>> {
    fn augurs_outlier(
        self: &Rc<Self>,
        config: impl Into<AugursOutlierConfig>,
    ) -> Rc<dyn Stream<AugursOutliers>> {
        let config = config.into();
        let detector = match config.detector {
            AugursOutlierDetector::Mad => Detector::Mad(
                MADDetector::with_sensitivity(config.sensitivity).unwrap_or_else(|e| {
                    panic!(
                        "augurs_outlier: invalid sensitivity {}: {e}",
                        config.sensitivity
                    )
                }),
            ),
            AugursOutlierDetector::Dbscan => Detector::Dbscan(
                DbscanDetector::with_sensitivity(config.sensitivity).unwrap_or_else(|e| {
                    panic!(
                        "augurs_outlier: invalid sensitivity {}: {e}",
                        config.sensitivity
                    )
                }),
            ),
        };
        AugursOutlierNode::new(self.clone(), detector, config.window).into_stream()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::*;
    use crate::nodes::*;
    use std::time::Duration;

    /// Three series move together except one, which jumps away part-way
    /// through. The diverging series should be flagged by MAD.
    #[test]
    fn outlier_mad_flags_diverging_series() {
        // Per tick: [series0, series1, series2]. Series 2 spikes after a while.
        let readings = ticker(Duration::from_secs(1)).count().map(|n| {
            let base = 100.0 + (n as f64 * 0.4).sin();
            let series2 = if n > 20 { base + 80.0 } else { base + 0.2 };
            vec![base, base + 0.1, series2]
        });
        let outliers = readings.augurs_outlier(AugursOutlierConfig::new(40, 0.5));
        let captured = outliers.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(40))
            .unwrap();

        let last = outliers.peek_value();
        assert_eq!(last.scores.len(), 3, "one score per series");
        assert!(
            last.is_outlier(2),
            "series 2 diverged and should be flagged, got {last:?}"
        );
        assert!(!last.is_outlier(0));
        assert!(!last.is_outlier(1));
    }

    /// DBSCAN flags a series diverging from a cluster of similar series.
    #[test]
    fn outlier_dbscan_flags_diverging_series() {
        // Four series: three cluster together, series 3 diverges.
        let readings = ticker(Duration::from_secs(1)).count().map(|n| {
            let base = 100.0 + (n as f64 * 0.4).sin();
            let series3 = if n > 15 { base + 90.0 } else { base + 0.3 };
            vec![base, base + 0.1, base - 0.1, series3]
        });
        let outliers = readings.augurs_outlier(AugursOutlierConfig::dbscan(40, 0.5));
        let captured = outliers.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(40))
            .unwrap();

        let last = outliers.peek_value();
        assert_eq!(last.scores.len(), 4);
        assert!(
            last.is_outlier(3),
            "series 3 diverged and should be flagged by DBSCAN, got {last:?}"
        );
    }

    /// With all series moving together, nothing is flagged.
    #[test]
    fn outlier_quiet_when_aligned() {
        let readings = ticker(Duration::from_secs(1)).count().map(|n| {
            let base = 50.0 + (n as f64 * 0.3).sin();
            vec![base, base + 0.05, base - 0.05]
        });
        let outliers = readings.augurs_outlier((30, 0.5));
        let captured = outliers.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
            .unwrap();
        assert!(
            outliers.peek_value().outlying.is_empty(),
            "aligned series should produce no outliers, got {:?}",
            outliers.peek_value()
        );
    }

    /// The node stays silent until it has at least two samples.
    #[test]
    fn outlier_waits_for_two_samples() {
        let readings = ticker(Duration::from_secs(1))
            .count()
            .map(|n| vec![n as f64, n as f64 + 1.0]);
        let outliers = readings.augurs_outlier((8, 0.5));
        let captured = outliers.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert!(captured.peek_value().is_empty());
    }
}
