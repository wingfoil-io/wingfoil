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
        self.buffer.push_back(self.upstream.peek_value());
        while self.buffer.len() > self.config.window {
            self.buffer.pop_front();
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
        config: AugursClusterConfig,
    ) -> Rc<dyn Stream<AugursClusters>>;
}

impl AugursClusterOperators for dyn Stream<Vec<f64>> {
    fn augurs_cluster(
        self: &Rc<Self>,
        config: AugursClusterConfig,
    ) -> Rc<dyn Stream<AugursClusters>> {
        AugursClusterNode::new(self.clone(), config).into_stream()
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
}
