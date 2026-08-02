use std::collections::VecDeque;
use std::rc::Rc;

use augurs::dtw::Dtw;

use super::AugursDistanceMatrix;
use crate::types::*;

/// The pointwise distance metric used by [`Dtw`] when warping two series.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AugursDtwMetric {
    /// Euclidean (L2) distance between aligned points.
    #[default]
    Euclidean,
    /// Manhattan (L1) distance between aligned points.
    Manhattan,
}

/// Configuration for [`AugursDtwOperators::augurs_dtw`].
#[derive(Debug, Clone)]
pub struct AugursDtwConfig {
    /// Number of recent samples retained as the window over which each series'
    /// history is compared.
    pub window: usize,
    /// Pointwise distance metric used inside the DTW alignment.
    pub metric: AugursDtwMetric,
}

impl AugursDtwConfig {
    /// Compute the DTW distance matrix over the last `window` samples using the
    /// Euclidean metric.
    #[must_use]
    pub fn new(window: usize) -> Self {
        Self {
            window,
            metric: AugursDtwMetric::Euclidean,
        }
    }

    /// Use the given pointwise distance metric.
    #[must_use]
    pub fn with_metric(mut self, metric: AugursDtwMetric) -> Self {
        self.metric = metric;
        self
    }
}

impl From<usize> for AugursDtwConfig {
    /// `window`, with the Euclidean metric.
    fn from(window: usize) -> Self {
        Self::new(window)
    }
}

/// Compute the DTW distance matrix for `series` using `metric`. Returned as the
/// augurs [`DistanceMatrix`](augurs::DistanceMatrix) so callers that need the
/// raw rows can call [`into_inner`](augurs::DistanceMatrix::into_inner) and
/// those feeding clustering can pass it straight through.
pub(crate) fn distance_matrix(
    metric: AugursDtwMetric,
    series: &[&[f64]],
) -> augurs::DistanceMatrix {
    match metric {
        AugursDtwMetric::Euclidean => Dtw::euclidean().distance_matrix(series),
        AugursDtwMetric::Manhattan => Dtw::manhattan().distance_matrix(series),
    }
}

pub(crate) struct AugursDtwNode {
    upstream: Rc<dyn Stream<Vec<f64>>>,
    metric: AugursDtwMetric,
    window: usize,
    buffer: VecDeque<Vec<f64>>,
    value: AugursDistanceMatrix,
}

impl AugursDtwNode {
    fn new(upstream: Rc<dyn Stream<Vec<f64>>>, config: AugursDtwConfig) -> Self {
        Self {
            upstream,
            metric: config.metric,
            window: config.window.max(2),
            buffer: VecDeque::with_capacity(config.window.max(2)),
            value: AugursDistanceMatrix::default(),
        }
    }
}

#[node(active = [upstream], output = value: AugursDistanceMatrix)]
impl MutableNode for AugursDtwNode {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        super::push_windowed(&mut self.buffer, self.upstream.peek_value(), self.window);
        // Warm up: a DTW distance over length-1 columns is just |x - y|, not a
        // windowed-history distance, so wait for at least two samples before
        // emitting.
        if self.buffer.len() < 2 {
            return Ok(false);
        }

        let series = super::transpose_window(&self.buffer);
        // Need at least two series for a distance matrix to be meaningful.
        if series.len() < 2 {
            return Ok(false);
        }
        let refs: Vec<&[f64]> = series.iter().map(Vec::as_slice).collect();
        self.value = AugursDistanceMatrix {
            rows: distance_matrix(self.metric, &refs).into_inner(),
        };
        Ok(true)
    }
}

/// Adds the [`augurs_dtw`](AugursDtwOperators::augurs_dtw) operator to streams
/// of per-series readings.
pub trait AugursDtwOperators {
    /// Maintain a sliding window of per-series readings (one `f64` per series
    /// per tick) and emit an [`AugursDistanceMatrix`] each tick with the
    /// pairwise dynamic time warping distances between the series' windowed
    /// histories.
    #[must_use]
    fn augurs_dtw(
        self: &Rc<Self>,
        config: impl Into<AugursDtwConfig>,
    ) -> Rc<dyn Stream<AugursDistanceMatrix>>;
}

impl AugursDtwOperators for dyn Stream<Vec<f64>> {
    fn augurs_dtw(
        self: &Rc<Self>,
        config: impl Into<AugursDtwConfig>,
    ) -> Rc<dyn Stream<AugursDistanceMatrix>> {
        AugursDtwNode::new(self.clone(), config.into()).into_stream()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::*;
    use crate::nodes::*;
    use std::time::Duration;

    /// Two similar series and one dissimilar one: the distance from the odd
    /// series out should exceed the distance between the similar pair.
    #[test]
    fn dtw_distances_reflect_similarity() {
        // series 0 and 1 track each other; series 2 is scaled up and offset.
        let readings = ticker(Duration::from_secs(1)).count().map(|n| {
            let t = n as f64;
            let a = (t * 0.3).sin();
            vec![a, a + 0.02, 5.0 * (t * 0.3).sin() + 10.0]
        });
        let dists = readings.augurs_dtw(AugursDtwConfig::new(30));
        let captured = dists.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
            .unwrap();

        let m = dists.peek_value();
        assert_eq!(m.rows.len(), 3, "3x3 distance matrix");
        let d01 = m.get(0, 1).unwrap();
        let d02 = m.get(0, 2).unwrap();
        assert!(m.get(0, 0).unwrap() < 1e-9, "self-distance is zero");
        assert!(
            d02 > d01,
            "dissimilar series should be farther: d02={d02}, d01={d01}"
        );
    }

    /// With two series but only a single sample, the node stays silent — a DTW
    /// distance over length-1 columns is not a windowed-history distance.
    #[test]
    fn dtw_waits_for_two_samples() {
        let readings = ticker(Duration::from_secs(1))
            .count()
            .map(|n| vec![n as f64, n as f64 + 1.0]);
        let dists = readings.augurs_dtw(8);
        let captured = dists.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert!(captured.peek_value().is_empty());
    }

    /// The node stays silent until it has at least two series.
    #[test]
    fn dtw_waits_for_two_series() {
        let readings = ticker(Duration::from_secs(1))
            .count()
            .map(|n| vec![n as f64]);
        let dists = readings.augurs_dtw(8);
        let captured = dists.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(10))
            .unwrap();
        assert!(captured.peek_value().is_empty());
    }
}
