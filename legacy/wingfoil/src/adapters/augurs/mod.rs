//! augurs adapter — on-graph time-series analysis powered by the
//! [`augurs`](https://docs.rs/augurs) toolkit.
//!
//! Unlike the messaging adapters in this crate, augurs is a pure-Rust compute
//! library rather than an external service — there is nothing to connect to and
//! no Docker container to run. The adapter therefore exposes **transform nodes**
//! that maintain a sliding window of an input stream and apply augurs models on
//! the graph thread each cycle:
//!
//! - [`AugursForecastOperators::augurs_forecast`] — buffers a window of an
//!   `f64` stream, fits a forecasting model ([`AutoETS`](augurs::ets::AutoETS)
//!   or [MSTL](augurs::mstl)) and emits an [`AugursForecast`] (point forecast +
//!   optional prediction intervals).
//! - [`AugursOutlierOperators::augurs_outlier`] — buffers a window of a
//!   `Vec<f64>` stream (one value per series per tick), runs an outlier detector
//!   ([MAD](augurs::outlier::MADDetector) or
//!   [DBSCAN](augurs::outlier::DbscanDetector)) and emits an [`AugursOutliers`]
//!   (which series are outlying + their latest scores).
//! - [`AugursChangepointOperators::augurs_changepoint`] — Bayesian online
//!   changepoint detection over a window of an `f64` stream, emitting the
//!   [`AugursChangepoints`] indices within the window.
//! - [`AugursSeasonsOperators::augurs_seasons`] — periodogram seasonality
//!   detection over a window of an `f64` stream, emitting the detected
//!   [`AugursSeasons`] period lengths.
//! - [`AugursDtwOperators::augurs_dtw`] — dynamic time warping distance matrix
//!   over a window of a `Vec<f64>` (multi-series) stream, emitting an
//!   [`AugursDistanceMatrix`].
//! - [`AugursClusterOperators::augurs_cluster`] — DBSCAN clustering of the
//!   series in a `Vec<f64>` window using their DTW distances, emitting the
//!   per-series [`AugursClusters`] labels.
//!
//! Models are fitted inside `cycle()`. This is pure CPU work on the
//! single-threaded graph engine (no locks, no I/O), but refitting is not free —
//! throttle the input with [`sample`](crate::StreamOperators) /
//! [`throttle`](crate::StreamOperators) upstream if you do not need a fresh fit
//! on every tick.
//!
//! # Forecasting
//!
//! ```ignore
//! use wingfoil::adapters::augurs::*;
//! use wingfoil::*;
//!
//! // A noisy upward ramp; forecast 5 steps ahead with 95% intervals.
//! ticker(std::time::Duration::from_secs(1))
//!     .count()
//!     .map(|n| n as f64 + (n as f64 * 0.3).sin())
//!     .augurs_forecast(AugursForecastConfig::new(64, 5).with_level(0.95))
//!     .for_each(|f, _| println!("next 5: {:?}", f.point))
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//!
//! // Seasonal data: fit MSTL with a period-24 season instead of plain ETS.
//! # use wingfoil::adapters::augurs::*;
//! # use wingfoil::*;
//! # let hourly: std::rc::Rc<dyn Stream<f64>> = constant(0.0);
//! hourly.augurs_forecast(AugursForecastConfig::new(240, 24).mstl(vec![24]));
//! ```
//!
//! # Outlier detection
//!
//! ```ignore
//! use wingfoil::adapters::augurs::*;
//! use wingfoil::*;
//!
//! // Each tick carries one reading per monitored series.
//! some_stream_of_readings // Rc<dyn Stream<Vec<f64>>>
//!     .augurs_outlier(AugursOutlierConfig::mad(32, 0.5))
//!     .for_each(|o, _| println!("outlying series: {:?}", o.outlying))
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```

mod changepoint;
mod cluster;
mod dtw;
mod forecast;
mod outlier;
mod seasons;

pub use changepoint::*;
pub use cluster::*;
pub use dtw::*;
pub use forecast::*;
pub use outlier::*;
pub use seasons::*;

use std::collections::VecDeque;

/// Push `value` onto a sliding-window buffer, evicting the oldest samples until
/// at most `window` remain. Shared by every augurs node's `cycle()` so the
/// ring-buffer maintenance lives in one place.
pub(crate) fn push_windowed<T>(buffer: &mut VecDeque<T>, value: T, window: usize) {
    buffer.push_back(value);
    while buffer.len() > window {
        buffer.pop_front();
    }
}

/// Transpose a window of per-tick multi-series samples (one value per series per
/// tick) into aligned per-series columns of equal length. Short samples are
/// forward-filled with the series' previous value so every column spans the full
/// window. A series that first appears part-way through the window has its
/// leading gap back-filled with its own first observed value — never a
/// fabricated `0.0`, which would read as a large deviation to the outlier / DTW
/// operators for the rest of the window.
///
/// Shared by the multi-series operators ([`augurs_outlier`](AugursOutlierOperators),
/// [`augurs_dtw`](AugursDtwOperators), [`augurs_cluster`](AugursClusterOperators)).
pub(crate) fn transpose_window(buffer: &VecDeque<Vec<f64>>) -> Vec<Vec<f64>> {
    let n_series = buffer.iter().map(Vec::len).max().unwrap_or(0);
    let mut series: Vec<Vec<Option<f64>>> = vec![Vec::with_capacity(buffer.len()); n_series];
    for sample in buffer {
        for (j, col) in series.iter_mut().enumerate() {
            // Forward-fill a short sample from the series' previous value; a
            // series that has not appeared yet stays `None` and is back-filled
            // below.
            let value = sample
                .get(j)
                .copied()
                .or_else(|| col.last().copied().flatten());
            col.push(value);
        }
    }
    series
        .into_iter()
        .map(|col| {
            let first = col.iter().copied().flatten().next().unwrap_or(0.0);
            col.into_iter().map(|v| v.unwrap_or(first)).collect()
        })
        .collect()
}

/// A forecast produced by [`AugursForecastOperators::augurs_forecast`].
///
/// `point` holds the n-ahead point forecasts (length = `horizon`). `lower` /
/// `upper` hold the prediction-interval bounds when a confidence level was
/// requested, and are empty otherwise.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AugursForecast {
    /// Point forecasts, one per step of the horizon.
    pub point: Vec<f64>,
    /// Lower prediction-interval bounds (empty when no level was requested).
    pub lower: Vec<f64>,
    /// Upper prediction-interval bounds (empty when no level was requested).
    pub upper: Vec<f64>,
}

impl AugursForecast {
    /// The one-step-ahead point forecast, if any.
    #[must_use]
    pub fn next(&self) -> Option<f64> {
        self.point.first().copied()
    }
}

/// The result of [`AugursOutlierOperators::augurs_outlier`] for one cycle.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AugursOutliers {
    /// Indices of the series flagged as outlying over the current window.
    pub outlying: Vec<usize>,
    /// The most recent outlier score for each series (higher = more outlying).
    pub scores: Vec<f64>,
}

impl AugursOutliers {
    /// Whether the series at `index` is currently flagged as an outlier.
    #[must_use]
    pub fn is_outlier(&self, index: usize) -> bool {
        self.outlying.contains(&index)
    }
}

/// The result of [`AugursChangepointOperators::augurs_changepoint`] for one
/// cycle — the indices, within the current window, at which a changepoint was
/// detected.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AugursChangepoints {
    /// Changepoint indices relative to the start of the current window.
    pub indices: Vec<usize>,
}

impl AugursChangepoints {
    /// Whether any changepoint was detected in the current window.
    #[must_use]
    pub fn any(&self) -> bool {
        !self.indices.is_empty()
    }
}

/// The result of [`AugursSeasonsOperators::augurs_seasons`] for one cycle — the
/// seasonal period lengths (in samples) detected in the current window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AugursSeasons {
    /// Detected seasonal period lengths, longest signal first.
    pub periods: Vec<u32>,
}

impl AugursSeasons {
    /// The dominant (first-reported) seasonal period, if any.
    #[must_use]
    pub fn dominant(&self) -> Option<u32> {
        self.periods.first().copied()
    }
}

/// A row-major square distance matrix produced by
/// [`AugursDtwOperators::augurs_dtw`]. `rows[i][j]` is the DTW distance between
/// series `i` and series `j` over the current window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AugursDistanceMatrix {
    /// Row-major `n × n` distances, where `n` is the number of series.
    pub rows: Vec<Vec<f64>>,
}

impl AugursDistanceMatrix {
    /// The distance between series `i` and series `j`, if both are in range.
    #[must_use]
    pub fn get(&self, i: usize, j: usize) -> Option<f64> {
        self.rows.get(i).and_then(|row| row.get(j)).copied()
    }
}

/// The result of [`AugursClusterOperators::augurs_cluster`] for one cycle — a
/// cluster label per series. A label of `-1` marks a noise point (unclustered);
/// non-negative labels group series into clusters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AugursClusters {
    /// Cluster label for each series (`-1` = noise).
    pub labels: Vec<i32>,
}

impl AugursClusters {
    /// The number of distinct (non-noise) clusters found.
    #[must_use]
    pub fn cluster_count(&self) -> usize {
        let mut seen: Vec<i32> = self.labels.iter().copied().filter(|&l| l >= 0).collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A series that only appears part-way through the window has its leading
    /// gap back-filled with its own first value, not a fabricated `0.0` (which
    /// would read as a large deviation to the outlier / DTW operators).
    #[test]
    fn transpose_backfills_late_series_with_first_value() {
        let mut buffer = VecDeque::new();
        buffer.push_back(vec![1.0, 2.0]);
        buffer.push_back(vec![1.0, 2.0]);
        buffer.push_back(vec![1.0, 2.0, 3.0]);
        let cols = transpose_window(&buffer);
        assert_eq!(
            cols,
            vec![
                vec![1.0, 1.0, 1.0],
                vec![2.0, 2.0, 2.0],
                vec![3.0, 3.0, 3.0],
            ]
        );
    }

    /// A short sample forward-fills each missing series from its previous value.
    #[test]
    fn transpose_forward_fills_short_samples() {
        let mut buffer = VecDeque::new();
        buffer.push_back(vec![1.0, 2.0]);
        buffer.push_back(vec![5.0]);
        let cols = transpose_window(&buffer);
        assert_eq!(cols, vec![vec![1.0, 5.0], vec![2.0, 2.0]]);
    }
}
