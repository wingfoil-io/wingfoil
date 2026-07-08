use std::collections::VecDeque;
use std::rc::Rc;

use augurs::clustering::DbscanClusterer;

use super::dtw::{AugursDtwMetric, distance_matrix};
use super::{AugursClusters, transpose_window};
use crate::types::*;

/// Configuration for [`AugursClusterOperators::augurs_cluster`].
#[derive(Debug, Clone)]
pub struct AugursClusterConfig {
    /// Number of recent samples retained as the window over which each series'
    /// history is compared.
    pub window: usize,
    /// DBSCAN neighbourhood radius: the maximum DTW distance between two series
    /// for them to be considered neighbours.
    pub epsilon: f64,
    /// DBSCAN minimum cluster size: the number of neighbours (including itself)
    /// a series needs to be a core point.
    pub min_cluster_size: usize,
    /// Pointwise distance metric used inside the DTW alignment.
    pub metric: AugursDtwMetric,
}

impl AugursClusterConfig {
    /// Cluster the series over the last `window` samples with the given DBSCAN
    /// `epsilon` radius and `min_cluster_size`, using DTW/Euclidean distances.
    #[must_use]
    pub fn new(window: usize, epsilon: f64, min_cluster_size: usize) -> Self {
        Self {
            window,
            epsilon,
            min_cluster_size,
            metric: AugursDtwMetric::Euclidean,
        }
    }

    /// Use the given pointwise distance metric inside DTW.
    #[must_use]
    pub fn with_metric(mut self, metric: AugursDtwMetric) -> Self {
        self.metric = metric;
        self
    }
}

impl From<(usize, f64, usize)> for AugursClusterConfig {
    /// `(window, epsilon, min_cluster_size)` using the Euclidean metric.
    fn from((window, epsilon, min_cluster_size): (usize, f64, usize)) -> Self {
        Self::new(window, epsilon, min_cluster_size)
    }
}

pub(crate) struct AugursClusterNode {
    upstream: Rc<dyn Stream<Vec<f64>>>,
    config: AugursClusterConfig,
    buffer: VecDeque<Vec<f64>>,
    value: AugursClusters,
}

impl AugursClusterNode {
    fn new(upstream: Rc<dyn Stream<Vec<f64>>>, config: AugursClusterConfig) -> Self {
        let cap = config.window.max(2);
        Self {
            upstream,
            config,
            buffer: VecDeque::with_capacity(cap),
            value: AugursClusters::default(),
        }
    }
}

#[node(active = [upstream], output = value: AugursClusters)]
impl MutableNode for AugursClusterNode {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        super::push_windowed(
            &mut self.buffer,
            self.upstream.peek_value(),
            self.config.window,
        );
        // Warm up: clustering over length-1 columns compares single points, not
        // windowed histories, so wait for at least two samples before emitting.
        if self.buffer.len() < 2 {
            return Ok(false);
        }

        let series = transpose_window(&self.buffer);
        // Need at least two series to cluster.
        if series.len() < 2 {
            return Ok(false);
        }
        let refs: Vec<&[f64]> = series.iter().map(Vec::as_slice).collect();
        let matrix = distance_matrix(self.config.metric, &refs);
        let clusters =
            DbscanClusterer::new(self.config.epsilon, self.config.min_cluster_size).fit(&matrix);

        self.value = AugursClusters {
            labels: clusters.iter().map(|c| c.as_i32()).collect(),
        };
        Ok(true)
    }
}

/// Adds the [`augurs_cluster`](AugursClusterOperators::augurs_cluster) operator
/// to streams of per-series readings.
pub trait AugursClusterOperators {
    /// Maintain a sliding window of per-series readings (one `f64` per series
    /// per tick) and emit an [`AugursClusters`] each tick: a DBSCAN cluster
    /// label per series (`-1` = noise), computed from the pairwise DTW distances
    /// between the series' windowed histories.
    #[must_use]
    fn augurs_cluster(
        self: &Rc<Self>,
        config: impl Into<AugursClusterConfig>,
    ) -> Rc<dyn Stream<AugursClusters>>;
}

impl AugursClusterOperators for dyn Stream<Vec<f64>> {
    fn augurs_cluster(
        self: &Rc<Self>,
        config: impl Into<AugursClusterConfig>,
    ) -> Rc<dyn Stream<AugursClusters>> {
        AugursClusterNode::new(self.clone(), config.into()).into_stream()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::*;
    use crate::nodes::*;
    use std::time::Duration;

    /// Two tight groups of series plus one outlier should form two clusters,
    /// with the outlier labelled as noise.
    #[test]
    fn cluster_groups_similar_series() {
        // series 0,1 near a low sine; series 2,3 near a high sine; series 4 wild.
        let readings = ticker(Duration::from_secs(1)).count().map(|n| {
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
        let captured = clusters.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
            .unwrap();

        let last = clusters.peek_value();
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

    /// The node stays silent until it has at least two series.
    #[test]
    fn cluster_waits_for_two_series() {
        let readings = ticker(Duration::from_secs(1))
            .count()
            .map(|n| vec![n as f64]);
        let clusters = readings.augurs_cluster(AugursClusterConfig::new(8, 1.0, 2));
        let captured = clusters.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(10))
            .unwrap();
        assert!(captured.peek_value().is_empty());
    }

    /// With two series but only a single sample, the node stays silent —
    /// clustering length-1 columns compares single points, not histories.
    #[test]
    fn cluster_waits_for_two_samples() {
        let readings = ticker(Duration::from_secs(1))
            .count()
            .map(|n| vec![n as f64, n as f64 + 1.0]);
        let clusters = readings.augurs_cluster(AugursClusterConfig::new(8, 1.0, 2));
        let captured = clusters.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert!(captured.peek_value().is_empty());
    }

    /// `(window, epsilon, min_cluster_size)` tuples convert into a config, so
    /// `augurs_cluster` reads like the rest of the operator family.
    #[test]
    fn cluster_accepts_tuple_config() {
        let readings = ticker(Duration::from_secs(1)).count().map(|n| {
            let t = n as f64;
            let low = (t * 0.3).sin();
            vec![low, low + 0.02, low + 20.0, low + 20.02]
        });
        let clusters = readings.augurs_cluster((30, 1.0, 2));
        let captured = clusters.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
            .unwrap();
        assert_eq!(clusters.peek_value().labels.len(), 4);
    }
}
