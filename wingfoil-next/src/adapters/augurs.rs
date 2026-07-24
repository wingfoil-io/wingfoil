//! augurs adapter — on-graph time-series analysis powered by the
//! [`augurs`](https://docs.rs/augurs) toolkit, ported to the Op pattern.
//!
//! Unlike a messaging adapter, augurs is a pure-Rust compute library: there is
//! nothing to connect to and no service lifecycle. The adapter therefore
//! exposes **transform ops** that maintain a sliding window of an input stream
//! and apply augurs models on the graph thread each cycle — the same shape as
//! the [`stats`](crate::stats) rolling-window ops, just with a heavier kernel.
//!
//! - [`AugursForecastOps::augurs_forecast`] — buffers a window of an `f64`
//!   stream, fits a forecasting model ([`AutoETS`](augurs::ets::AutoETS) or
//!   [MSTL](augurs::mstl)) and emits an [`AugursForecast`] (point forecast +
//!   optional prediction intervals).
//! - [`AugursOutlierOps::augurs_outlier`] — buffers a window of a `Vec<f64>`
//!   stream (one value per series per tick), runs an outlier detector
//!   ([MAD](augurs::outlier::MADDetector) or
//!   [DBSCAN](augurs::outlier::DbscanDetector)) and emits an [`AugursOutliers`]
//!   (which series are outlying + their latest scores).
//!
//! # Layering
//!
//! Following the [`stats`](crate::stats) module's pattern, the ops are *not*
//! in the [`prelude`](crate::prelude): bring in the extension traits explicitly
//! with `use wingfoil_next::adapters::augurs::{AugursForecastOps, AugursOutlierOps};`.
//!
//! Models are fitted inside `cycle()`. This is pure CPU work on the
//! single-threaded engine (no locks, no I/O), but refitting is not free —
//! throttle the input with [`throttle`](crate::fluent::StreamOps::throttle)
//! upstream if you do not need a fresh fit on every tick.

use std::collections::VecDeque;

use anyhow::{Context, Result};
use augurs::ets::AutoETS;
use augurs::ets::trend::AutoETSTrendModel;
use augurs::mstl::MSTLModel;
use augurs::outlier::{DbscanDetector, MADDetector, OutlierDetector, OutlierOutput};
use augurs::{Fit, Predict};
use wingfoil_next_macros::op;

use crate::fluent::Stream;
use crate::op::{Activation, Ctx, Op, Tick};

// -------------------------------------------------------------------------
// Shared windowing helpers (ported from the classic adapter's `mod.rs`).
// -------------------------------------------------------------------------

/// Push `value` onto a sliding-window buffer, evicting the oldest samples until
/// at most `window` remain.
fn push_windowed<T>(buffer: &mut VecDeque<T>, value: T, window: usize) {
    buffer.push_back(value);
    while buffer.len() > window {
        buffer.pop_front();
    }
}

/// Transpose a window of per-tick multi-series samples (one value per series
/// per tick) into aligned per-series columns of equal length. Short samples are
/// forward-filled with the series' previous value so every column spans the
/// full window. A series that first appears part-way through the window has its
/// leading gap back-filled with its own first observed value — never a
/// fabricated `0.0`, which would read as a large deviation to the outlier
/// detector for the rest of the window.
fn transpose_window(buffer: &VecDeque<Vec<f64>>) -> Vec<Vec<f64>> {
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

// -------------------------------------------------------------------------
// Value types.
// -------------------------------------------------------------------------

/// A forecast produced by [`AugursForecastOps::augurs_forecast`].
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

/// The result of [`AugursOutlierOps::augurs_outlier`] for one cycle.
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

// -------------------------------------------------------------------------
// Forecasting.
// -------------------------------------------------------------------------

/// The forecasting model used by [`AugursForecastOps::augurs_forecast`].
#[derive(Debug, Clone, Default, PartialEq)]
pub enum AugursForecastModel {
    /// Non-seasonal [`AutoETS`]. Fast and a good default for data without a
    /// fixed seasonal period.
    #[default]
    Ets,
    /// [MSTL](augurs::mstl) seasonal-trend decomposition with an [`AutoETS`]
    /// trend, using the given seasonal `periods` (in samples). Choose this when
    /// the data has one or more known seasonalities (e.g. `[24]` for hourly
    /// data with a daily cycle). Needs a window of at least a couple of full
    /// periods.
    Mstl {
        /// Seasonal period lengths, in samples.
        periods: Vec<usize>,
    },
}

/// Configuration for [`AugursForecastOps::augurs_forecast`].
#[derive(Debug, Clone)]
pub struct AugursForecastConfig {
    /// Maximum number of recent points retained as training history. Older
    /// points are dropped once the window is full.
    pub window: usize,
    /// Minimum number of buffered points before the model is fitted. Until this
    /// many points have arrived the node does not tick. `AutoETS` needs more
    /// than ~8 points, so values below `12` are raised to `12`.
    pub min_points: usize,
    /// Number of steps to forecast ahead.
    pub horizon: usize,
    /// Confidence level for prediction intervals (between 0 and 1). When `None`,
    /// only point forecasts are produced and `lower`/`upper` stay empty.
    pub level: Option<f64>,
    /// Which forecasting model to fit each cycle.
    pub model: AugursForecastModel,
}

impl AugursForecastConfig {
    /// A config that keeps the last `window` points and forecasts `horizon`
    /// steps ahead with no prediction intervals.
    #[must_use]
    pub fn new(window: usize, horizon: usize) -> Self {
        Self {
            window,
            min_points: 12,
            horizon,
            level: None,
            model: AugursForecastModel::Ets,
        }
    }

    /// Request prediction intervals at the given confidence `level` (0..1).
    #[must_use]
    pub fn with_level(mut self, level: f64) -> Self {
        self.level = Some(level);
        self
    }

    /// Use MSTL seasonal-trend forecasting with the given seasonal `periods`
    /// (in samples) instead of the default non-seasonal ETS model.
    #[must_use]
    pub fn mstl(mut self, periods: Vec<usize>) -> Self {
        self.model = AugursForecastModel::Mstl { periods };
        self
    }

    /// Select the forecasting model explicitly.
    #[must_use]
    pub fn with_model(mut self, model: AugursForecastModel) -> Self {
        self.model = model;
        self
    }

    /// Set the minimum number of points required before fitting begins.
    #[must_use]
    pub fn with_min_points(mut self, min_points: usize) -> Self {
        self.min_points = min_points;
        self
    }

    /// The effective minimum. Never below the `AutoETS` floor (~12 points), and
    /// for MSTL never below two full seasonal periods (STL cannot decompose
    /// fewer than two periods).
    fn effective_min_points(&self) -> usize {
        let floor = match &self.model {
            AugursForecastModel::Ets => 12,
            AugursForecastModel::Mstl { periods } => {
                let max_period = periods.iter().copied().max().unwrap_or(1);
                (2 * max_period + 1).max(12)
            }
        };
        self.min_points.max(floor)
    }
}

impl From<(usize, usize)> for AugursForecastConfig {
    /// `(window, horizon)`.
    fn from((window, horizon): (usize, usize)) -> Self {
        Self::new(window, horizon)
    }
}

/// Resolved forecast config: the user config plus the warm-up floor and
/// effective window, computed once at wiring time and held as the op's `Cfg`.
#[derive(Debug, Clone)]
pub struct ForecastCfg {
    config: AugursForecastConfig,
    /// Effective window: the configured `window` grown to the model's warm-up
    /// floor so a `window` smaller than the floor still lets the node fill up
    /// and emit rather than never ticking.
    window: usize,
    /// Resolved warm-up floor (`effective_min_points`).
    min_points: usize,
}

impl ForecastCfg {
    fn resolve(config: AugursForecastConfig) -> Self {
        let min_points = config.effective_min_points();
        let window = config.window.max(min_points);
        Self {
            config,
            window,
            min_points,
        }
    }
}

/// Forecast op: buffers a window of an `f64` stream and fits an augurs
/// forecasting model each cycle. `Cfg` = resolved config, `State` = the sliding
/// window buffer.
pub struct AugursForecastOp;

#[op(build = augurs_forecast)]
impl Op for AugursForecastOp {
    type Cfg = ForecastCfg;
    type State = VecDeque<f64>;
    type In<'a> = (&'a f64,);
    type Out = AugursForecast;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut ForecastCfg,
        buffer: &mut VecDeque<f64>,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<AugursForecast>> {
        push_windowed(buffer, *input.0, cfg.window);
        if buffer.len() < cfg.min_points {
            return Ok(Tick::Quiet);
        }

        let data: Vec<f64> = buffer.iter().copied().collect();
        let forecast = match &cfg.config.model {
            AugursForecastModel::Ets => {
                let fitted = AutoETS::non_seasonal()
                    .fit(&data)
                    .context("augurs_forecast: ETS fit failed")?;
                fitted
                    .predict(cfg.config.horizon, cfg.config.level)
                    .context("augurs_forecast: ETS predict failed")?
            }
            AugursForecastModel::Mstl { periods } => {
                // STL cannot decompose an empty period set or a period below 2;
                // catch it here with a clear message rather than letting augurs
                // fail deep inside the fit with an opaque error.
                if periods.is_empty() || periods.iter().any(|&p| p < 2) {
                    anyhow::bail!(
                        "augurs_forecast: MSTL periods must be non-empty and each >= 2, got {periods:?}"
                    );
                }
                let trend = AutoETSTrendModel::from(AutoETS::non_seasonal());
                let fitted = MSTLModel::new(periods.clone(), trend)
                    .fit(&data)
                    .context("augurs_forecast: MSTL fit failed")?;
                fitted
                    .predict(cfg.config.horizon, cfg.config.level)
                    .context("augurs_forecast: MSTL predict failed")?
            }
        };

        let (lower, upper) = match forecast.intervals {
            Some(intervals) => (intervals.lower, intervals.upper),
            None => (Vec::new(), Vec::new()),
        };
        Ok(Tick::Value(AugursForecast {
            point: forecast.point,
            lower,
            upper,
        }))
    }
}

/// Adds the [`augurs_forecast`](AugursForecastOps::augurs_forecast) op to
/// `f64` streams. Bring it into scope with
/// `use wingfoil_next::adapters::augurs::AugursForecastOps;`.
pub trait AugursForecastOps {
    /// Maintain a sliding window of this stream and, once `min_points` have
    /// arrived, emit an [`AugursForecast`] each tick by fitting a forecasting
    /// model and predicting `horizon` steps ahead.
    fn augurs_forecast(&self, config: impl Into<AugursForecastConfig>) -> Stream<AugursForecast>;
}

impl AugursForecastOps for Stream<f64> {
    fn augurs_forecast(&self, config: impl Into<AugursForecastConfig>) -> Stream<AugursForecast> {
        let cfg = ForecastCfg::resolve(config.into());
        self.wire(|b, h| b.augurs_forecast(h, cfg))
    }
}

// -------------------------------------------------------------------------
// Outlier detection.
// -------------------------------------------------------------------------

/// Which outlier-detection algorithm [`AugursOutlierOps::augurs_outlier`] uses.
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

/// Configuration for [`AugursOutlierOps::augurs_outlier`].
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

/// A constructed detector. `OutlierDetector` has an associated
/// `PreprocessedData` type so it is not object-safe; this enum dispatches over
/// the two concrete implementations instead.
enum Detector {
    Mad(MADDetector),
    Dbscan(DbscanDetector),
}

impl Detector {
    /// Build the detector for the given choice and `sensitivity`.
    /// `with_sensitivity` fails on a sensitivity outside `(0, 1)`; that error is
    /// surfaced through `anyhow` (aborting the run on the first cycle with a
    /// clear message) rather than panicking at wiring time. The construction is
    /// cheap and deterministic, so doing it per cycle does not change results.
    fn build(detector: AugursOutlierDetector, sensitivity: f64) -> Result<Self> {
        match detector {
            AugursOutlierDetector::Mad => {
                let d = MADDetector::with_sensitivity(sensitivity).map_err(|e| {
                    anyhow::anyhow!("augurs_outlier: invalid sensitivity {sensitivity}: {e}")
                })?;
                Ok(Detector::Mad(d))
            }
            AugursOutlierDetector::Dbscan => {
                let d = DbscanDetector::with_sensitivity(sensitivity).map_err(|e| {
                    anyhow::anyhow!("augurs_outlier: invalid sensitivity {sensitivity}: {e}")
                })?;
                Ok(Detector::Dbscan(d))
            }
        }
    }

    fn run(&self, series: &[&[f64]]) -> Result<OutlierOutput> {
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

/// Resolved outlier config: the detector choice, sensitivity, and effective
/// window (floored at 2), held as the op's `Cfg`.
#[derive(Debug, Clone)]
pub struct OutlierCfg {
    detector: AugursOutlierDetector,
    sensitivity: f64,
    window: usize,
}

/// Outlier op: buffers a window of per-series readings and runs an augurs
/// outlier detector each cycle. `Cfg` = resolved config, `State` = the sliding
/// window of per-tick multi-series samples.
pub struct AugursOutlierOp;

#[op(build = augurs_outlier)]
impl Op for AugursOutlierOp {
    type Cfg = OutlierCfg;
    type State = VecDeque<Vec<f64>>;
    type In<'a> = (&'a Vec<f64>,);
    type Out = AugursOutliers;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut OutlierCfg,
        buffer: &mut VecDeque<Vec<f64>>,
        input: (&Vec<f64>,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<AugursOutliers>> {
        push_windowed(buffer, input.0.clone(), cfg.window);
        // Need at least two timestamps to have any spread to measure.
        if buffer.len() < 2 {
            return Ok(Tick::Quiet);
        }

        // Transpose the buffered samples (one value per series per tick) into
        // aligned per-series time series.
        let series = transpose_window(buffer);
        if series.is_empty() {
            return Ok(Tick::Quiet);
        }
        let refs: Vec<&[f64]> = series.iter().map(Vec::as_slice).collect();
        let detector = Detector::build(cfg.detector, cfg.sensitivity)?;
        let output = detector.run(&refs)?;

        Ok(Tick::Value(AugursOutliers {
            outlying: output.outlying_series.iter().copied().collect(),
            scores: output
                .series_results
                .iter()
                .map(|s| s.scores.last().copied().unwrap_or(0.0))
                .collect(),
        }))
    }
}

/// Adds the [`augurs_outlier`](AugursOutlierOps::augurs_outlier) op to streams
/// of per-series readings. Bring it into scope with
/// `use wingfoil_next::adapters::augurs::AugursOutlierOps;`.
pub trait AugursOutlierOps {
    /// Maintain a sliding window of per-series readings (one `f64` per series
    /// per tick) and emit an [`AugursOutliers`] each tick once at least two
    /// samples have arrived, flagging series that deviate from the group via the
    /// configured detector.
    ///
    /// A `sensitivity` outside `(0, 1)` aborts the run on the first cycle with a
    /// clear error (rather than panicking at wiring time).
    fn augurs_outlier(&self, config: impl Into<AugursOutlierConfig>) -> Stream<AugursOutliers>;
}

impl AugursOutlierOps for Stream<Vec<f64>> {
    fn augurs_outlier(&self, config: impl Into<AugursOutlierConfig>) -> Stream<AugursOutliers> {
        let config = config.into();
        let cfg = OutlierCfg {
            detector: config.detector,
            sensitivity: config.sensitivity,
            window: config.window.max(2),
        };
        self.wire(|b, h| b.augurs_outlier(h, cfg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A series that only appears part-way through the window has its leading
    /// gap back-filled with its own first value, not a fabricated `0.0`.
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
