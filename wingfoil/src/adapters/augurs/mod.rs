//! augurs adapter — on-graph time-series analysis powered by the
//! [`augurs`](https://docs.rs/augurs) toolkit (forecasting and outlier detection).
//!
//! Unlike the messaging adapters in this crate, augurs is a pure-Rust compute
//! library rather than an external service — there is nothing to connect to and
//! no Docker container to run. The adapter therefore exposes **transform nodes**
//! that maintain a sliding window of an input stream and apply augurs models on
//! the graph thread each cycle:
//!
//! - [`AugursForecastOperators::augurs_forecast`] — buffers a window of an
//!   `f64` stream, fits an [`AutoETS`](augurs::ets::AutoETS) model and emits an
//!   [`AugursForecast`] (point forecast + optional prediction intervals).
//! - [`AugursOutlierOperators::augurs_outlier`] — buffers a window of a
//!   `Vec<f64>` stream (one value per series per tick), runs the
//!   [`MADDetector`](augurs::outlier::MADDetector) and emits an
//!   [`AugursOutliers`] (which series are outlying + their latest scores).
//!
//! Both models are fitted inside `cycle()`. This is pure CPU work on the
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
//!     .augurs_outlier(AugursOutlierConfig::new(32, 0.5))
//!     .for_each(|o, _| println!("outlying series: {:?}", o.outlying))
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```

mod forecast;
mod outlier;

pub use forecast::*;
pub use outlier::*;

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
