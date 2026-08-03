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
//! - [`AugursChangepointOps::augurs_changepoint`] — Bayesian online changepoint
//!   detection over a window of an `f64` stream, emitting the
//!   [`AugursChangepoints`] indices within the window.
//! - [`AugursSeasonsOps::augurs_seasons`] — periodogram seasonality detection
//!   over a window of an `f64` stream, emitting the detected [`AugursSeasons`]
//!   period lengths.
//! - [`AugursDtwOps::augurs_dtw`] — dynamic time warping distance matrix over a
//!   window of a `Vec<f64>` (multi-series) stream, emitting an
//!   [`AugursDistanceMatrix`].
//! - [`AugursClusterOps::augurs_cluster`] — DBSCAN clustering of the series in a
//!   `Vec<f64>` window using their DTW distances, emitting the per-series
//!   [`AugursClusters`] labels.
//!
//! # Layering
//!
//! Following the [`stats`](crate::stats) module's pattern, the ops are *not*
//! in the [`prelude`](crate::prelude): bring in the extension traits explicitly
//! with `use wingfoil::adapters::augurs::AugursForecastOps;` (and the
//! `AugursOutlierOps` / `AugursChangepointOps` / `AugursSeasonsOps` /
//! `AugursDtwOps` / `AugursClusterOps` siblings).
//!
//! Models are fitted inside `cycle()`. This is pure CPU work on the
//! single-threaded engine (no locks, no I/O), but it is not free: each cycle
//! recomputes over the whole window — the ETS model search, a BOCPD re-scan of
//! the window, and DTW at `O(n² · window²)` for `n` series. Throttle the input
//! with [`throttle`](crate::fluent::StreamOps::throttle) upstream if you do not
//! need a fresh result on every tick.
//!
//! # Deviations from legacy
//!
//! - **Config is validated inside `cycle` (fallibly), not at wiring (by panic).**
//!   Legacy's outlier detector construction panics at wiring on a bad
//!   sensitivity (`MADDetector::with_sensitivity(...).unwrap_or_else(|e|
//!   panic!(...))`); next builds the detector inside `cycle` and returns an
//!   `anyhow` error (`.map_err(...)` / `anyhow::bail!`) so a bad config aborts
//!   the run with a clear message rather than panicking during graph
//!   construction — a deliberate improvement over the legacy behaviour.
//! - **`augurs_cluster` floors its effective window at 2, as `augurs_dtw`
//!   already does.** Legacy's cluster node sizes its buffer for two samples but
//!   still evicts against the raw `window`, so a `window` of 1 never reaches the
//!   two-sample warm-up and the node never ticks. Next grows the effective
//!   window to the warm-up floor for both ops — the same "grow the window to the
//!   floor so the node still emits" rule the forecast/seasons ops follow.

use std::collections::VecDeque;

use anyhow::{Context, Result};
use augurs::changepoint::{
    Detector as ChangepointDetector, NormalGammaDetector, dist::NormalGamma,
};
use augurs::clustering::DbscanClusterer;
use augurs::dtw::Dtw;
use augurs::ets::AutoETS;
use augurs::ets::trend::AutoETSTrendModel;
use augurs::mstl::MSTLModel;
use augurs::outlier::{DbscanDetector, MADDetector, OutlierDetector, OutlierOutput};
use augurs::seasons::{Detector as SeasonsDetector, PeriodogramDetector};
use augurs::{Fit, Predict};
use wingfoil_macros::op;

use crate::fluent::Stream;
use crate::op::{Activation, Ctx, Op, Tick};

// -------------------------------------------------------------------------
// Shared windowing helpers (ported from the legacy adapter's `mod.rs`).
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
/// fabricated `0.0`, which would read as a large deviation to the outlier / DTW
/// ops for the rest of the window.
///
/// Shared by the multi-series ops ([`augurs_outlier`](AugursOutlierOps),
/// [`augurs_dtw`](AugursDtwOps), [`augurs_cluster`](AugursClusterOps)).
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

/// The result of [`AugursChangepointOps::augurs_changepoint`] for one cycle —
/// the indices, within the current window, at which a changepoint was detected.
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

/// The result of [`AugursSeasonsOps::augurs_seasons`] for one cycle — the
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
/// [`AugursDtwOps::augurs_dtw`]. `rows[i][j]` is the DTW distance between series
/// `i` and series `j` over the current window.
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

/// The result of [`AugursClusterOps::augurs_cluster`] for one cycle — a cluster
/// label per series. A label of `-1` marks a noise point (unclustered);
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
/// `use wingfoil::adapters::augurs::AugursForecastOps;`.
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
/// `use wingfoil::adapters::augurs::AugursOutlierOps;`.
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

// -------------------------------------------------------------------------
// Changepoint detection.
// -------------------------------------------------------------------------

/// Configuration for [`AugursChangepointOps::augurs_changepoint`].
#[derive(Debug, Clone)]
pub struct AugursChangepointConfig {
    /// Number of recent points retained as the detection window.
    pub window: usize,
    /// Minimum number of buffered points before detection runs. Until this many
    /// points have arrived the node does not tick.
    pub min_points: usize,
    /// Hazard rate for the Bayesian online changepoint detector: the prior
    /// expected run length between changepoints. Larger values make the detector
    /// more conservative (fewer changepoints). Defaults to `250`.
    pub hazard_lambda: f64,
}

impl AugursChangepointConfig {
    /// Detect changepoints over the last `window` points with the default hazard
    /// rate.
    #[must_use]
    pub fn new(window: usize) -> Self {
        Self {
            window,
            min_points: 8,
            hazard_lambda: 250.0,
        }
    }

    /// Set the hazard rate (prior expected run length between changepoints).
    #[must_use]
    pub fn with_hazard(mut self, hazard_lambda: f64) -> Self {
        self.hazard_lambda = hazard_lambda;
        self
    }

    /// Set the minimum number of points required before detection begins.
    #[must_use]
    pub fn with_min_points(mut self, min_points: usize) -> Self {
        self.min_points = min_points;
        self
    }
}

impl From<usize> for AugursChangepointConfig {
    /// `window`, with the default hazard rate.
    fn from(window: usize) -> Self {
        Self::new(window)
    }
}

/// Resolved changepoint config: the hazard rate, the warm-up floor, and the
/// effective window (the configured `window` grown to `min_points` so a window
/// below the floor still lets the node fill up and emit rather than never
/// ticking).
#[derive(Debug, Clone)]
pub struct ChangepointCfg {
    hazard_lambda: f64,
    min_points: usize,
    window: usize,
}

/// Changepoint op: buffers a window of an `f64` stream and re-scans it with
/// Bayesian online changepoint detection each cycle. `Cfg` = resolved config,
/// `State` = the sliding window buffer.
pub struct AugursChangepointOp;

#[op(build = augurs_changepoint)]
impl Op for AugursChangepointOp {
    type Cfg = ChangepointCfg;
    type State = VecDeque<f64>;
    type In<'a> = (&'a f64,);
    type Out = AugursChangepoints;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut ChangepointCfg,
        buffer: &mut VecDeque<f64>,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<AugursChangepoints>> {
        push_windowed(buffer, *input.0, cfg.window);
        if buffer.len() < cfg.min_points {
            return Ok(Tick::Quiet);
        }

        let data: Vec<f64> = buffer.iter().copied().collect();
        // BOCPD is stateful (it steps through the series), so build a fresh
        // detector each cycle and re-scan the current window.
        let mut detector = NormalGammaDetector::normal_gamma(
            cfg.hazard_lambda,
            NormalGamma::new_unchecked(0.0, 1.0, 1.0, 1.0),
        );
        // BOCPD always reports index 0 (the start of the first run) as a
        // changepoint; it is an artifact of the window start, not a real regime
        // change, so drop it.
        let indices = detector
            .detect_changepoints(&data)
            .into_iter()
            .filter(|&i| i != 0)
            .collect();
        Ok(Tick::Value(AugursChangepoints { indices }))
    }
}

/// Adds the [`augurs_changepoint`](AugursChangepointOps::augurs_changepoint) op
/// to `f64` streams. Bring it into scope with
/// `use wingfoil::adapters::augurs::AugursChangepointOps;`.
pub trait AugursChangepointOps {
    /// Maintain a sliding window of this stream and, once `min_points` have
    /// arrived, emit an [`AugursChangepoints`] each tick with the indices,
    /// within the window, at which Bayesian online changepoint detection found a
    /// changepoint. The window-start index (`0`), which BOCPD always reports, is
    /// excluded — only genuine internal regime changes are returned.
    fn augurs_changepoint(
        &self,
        config: impl Into<AugursChangepointConfig>,
    ) -> Stream<AugursChangepoints>;
}

impl AugursChangepointOps for Stream<f64> {
    fn augurs_changepoint(
        &self,
        config: impl Into<AugursChangepointConfig>,
    ) -> Stream<AugursChangepoints> {
        let config = config.into();
        let cfg = ChangepointCfg {
            hazard_lambda: config.hazard_lambda,
            min_points: config.min_points,
            window: config.window.max(config.min_points),
        };
        self.wire(|b, h| b.augurs_changepoint(h, cfg))
    }
}

// -------------------------------------------------------------------------
// Seasonality detection.
// -------------------------------------------------------------------------

/// Configuration for [`AugursSeasonsOps::augurs_seasons`].
#[derive(Debug, Clone)]
pub struct AugursSeasonsConfig {
    /// Number of recent points retained as the detection window.
    pub window: usize,
    /// Minimum number of buffered points before detection runs.
    pub min_points: usize,
    /// Shortest seasonal period to consider (in samples). `None` uses the augurs
    /// default.
    pub min_period: Option<u32>,
    /// Longest seasonal period to consider (in samples). `None` uses the augurs
    /// default (derived from the window length).
    pub max_period: Option<u32>,
}

impl AugursSeasonsConfig {
    /// Detect seasonal periods over the last `window` points using the default
    /// period bounds.
    #[must_use]
    pub fn new(window: usize) -> Self {
        Self {
            window,
            min_points: (window / 2).max(16),
            min_period: None,
            max_period: None,
        }
    }

    /// Restrict detection to periods within `[min_period, max_period]` samples.
    #[must_use]
    pub fn with_period_range(mut self, min_period: u32, max_period: u32) -> Self {
        self.min_period = Some(min_period);
        self.max_period = Some(max_period);
        self
    }

    /// Set the minimum number of points required before detection begins.
    #[must_use]
    pub fn with_min_points(mut self, min_points: usize) -> Self {
        self.min_points = min_points;
        self
    }
}

impl From<usize> for AugursSeasonsConfig {
    /// `window`, with the default period bounds.
    fn from(window: usize) -> Self {
        Self::new(window)
    }
}

/// Resolved seasons config: the periodogram detector (built once at wiring from
/// the requested period bounds), the warm-up floor, and the effective window.
#[derive(Debug)]
pub struct SeasonsCfg {
    detector: PeriodogramDetector,
    min_points: usize,
    window: usize,
}

/// Seasons op: buffers a window of an `f64` stream and runs periodogram
/// seasonality detection over it each cycle. `Cfg` = the built detector plus the
/// resolved window, `State` = the sliding window buffer.
pub struct AugursSeasonsOp;

#[op(build = augurs_seasons)]
impl Op for AugursSeasonsOp {
    type Cfg = SeasonsCfg;
    type State = VecDeque<f64>;
    type In<'a> = (&'a f64,);
    type Out = AugursSeasons;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut SeasonsCfg,
        buffer: &mut VecDeque<f64>,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<AugursSeasons>> {
        push_windowed(buffer, *input.0, cfg.window);
        if buffer.len() < cfg.min_points {
            return Ok(Tick::Quiet);
        }

        let data: Vec<f64> = buffer.iter().copied().collect();
        Ok(Tick::Value(AugursSeasons {
            periods: cfg.detector.detect(&data),
        }))
    }
}

/// Adds the [`augurs_seasons`](AugursSeasonsOps::augurs_seasons) op to `f64`
/// streams. Bring it into scope with
/// `use wingfoil::adapters::augurs::AugursSeasonsOps;`.
pub trait AugursSeasonsOps {
    /// Maintain a sliding window of this stream and, once `min_points` have
    /// arrived, emit an [`AugursSeasons`] each tick with the seasonal period
    /// lengths detected by a periodogram.
    ///
    /// augurs estimates periods with a Welch periodogram over power-of-two FFT
    /// segments, so the reported lengths are **approximate** (coarsely binned) —
    /// good for spotting *that* a season exists and its rough scale, not for
    /// exact period recovery.
    fn augurs_seasons(&self, config: impl Into<AugursSeasonsConfig>) -> Stream<AugursSeasons>;
}

impl AugursSeasonsOps for Stream<f64> {
    fn augurs_seasons(&self, config: impl Into<AugursSeasonsConfig>) -> Stream<AugursSeasons> {
        let config = config.into();
        let mut builder = PeriodogramDetector::builder();
        if let Some(min_period) = config.min_period {
            builder = builder.min_period(min_period);
        }
        if let Some(max_period) = config.max_period {
            builder = builder.max_period(max_period);
        }
        let cfg = SeasonsCfg {
            detector: builder.build(),
            min_points: config.min_points,
            // Grow the window to at least `min_points` so a `window` below the
            // warm-up floor still lets the node fill up and emit rather than
            // never ticking.
            window: config.window.max(config.min_points),
        };
        self.wire(|b, h| b.augurs_seasons(h, cfg))
    }
}

// -------------------------------------------------------------------------
// Dynamic time warping.
// -------------------------------------------------------------------------

/// The pointwise distance metric used by [`Dtw`] when warping two series.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AugursDtwMetric {
    /// Euclidean (L2) distance between aligned points.
    #[default]
    Euclidean,
    /// Manhattan (L1) distance between aligned points.
    Manhattan,
}

/// Configuration for [`AugursDtwOps::augurs_dtw`].
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
fn distance_matrix(metric: AugursDtwMetric, series: &[&[f64]]) -> augurs::DistanceMatrix {
    match metric {
        AugursDtwMetric::Euclidean => Dtw::euclidean().distance_matrix(series),
        AugursDtwMetric::Manhattan => Dtw::manhattan().distance_matrix(series),
    }
}

/// Resolved DTW config: the metric and the effective window (floored at 2 —
/// a DTW distance over length-1 columns is just `|x - y|`, not a
/// windowed-history distance).
#[derive(Debug, Clone)]
pub struct DtwCfg {
    metric: AugursDtwMetric,
    window: usize,
}

/// DTW op: buffers a window of per-series readings and computes their pairwise
/// dynamic time warping distances each cycle. `Cfg` = resolved config,
/// `State` = the sliding window of per-tick multi-series samples.
pub struct AugursDtwOp;

#[op(build = augurs_dtw)]
impl Op for AugursDtwOp {
    type Cfg = DtwCfg;
    type State = VecDeque<Vec<f64>>;
    type In<'a> = (&'a Vec<f64>,);
    type Out = AugursDistanceMatrix;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut DtwCfg,
        buffer: &mut VecDeque<Vec<f64>>,
        input: (&Vec<f64>,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<AugursDistanceMatrix>> {
        push_windowed(buffer, input.0.clone(), cfg.window);
        // Warm up: a DTW distance over length-1 columns is just |x - y|, not a
        // windowed-history distance, so wait for at least two samples.
        if buffer.len() < 2 {
            return Ok(Tick::Quiet);
        }

        let series = transpose_window(buffer);
        // Need at least two series for a distance matrix to be meaningful.
        if series.len() < 2 {
            return Ok(Tick::Quiet);
        }
        let refs: Vec<&[f64]> = series.iter().map(Vec::as_slice).collect();
        Ok(Tick::Value(AugursDistanceMatrix {
            rows: distance_matrix(cfg.metric, &refs).into_inner(),
        }))
    }
}

/// Adds the [`augurs_dtw`](AugursDtwOps::augurs_dtw) op to streams of per-series
/// readings. Bring it into scope with
/// `use wingfoil::adapters::augurs::AugursDtwOps;`.
pub trait AugursDtwOps {
    /// Maintain a sliding window of per-series readings (one `f64` per series
    /// per tick) and emit an [`AugursDistanceMatrix`] each tick with the
    /// pairwise dynamic time warping distances between the series' windowed
    /// histories.
    fn augurs_dtw(&self, config: impl Into<AugursDtwConfig>) -> Stream<AugursDistanceMatrix>;
}

impl AugursDtwOps for Stream<Vec<f64>> {
    fn augurs_dtw(&self, config: impl Into<AugursDtwConfig>) -> Stream<AugursDistanceMatrix> {
        let config = config.into();
        let cfg = DtwCfg {
            metric: config.metric,
            window: config.window.max(2),
        };
        self.wire(|b, h| b.augurs_dtw(h, cfg))
    }
}

// -------------------------------------------------------------------------
// Clustering.
// -------------------------------------------------------------------------

/// Configuration for [`AugursClusterOps::augurs_cluster`].
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

/// Resolved cluster config: the DBSCAN parameters, the DTW metric, and the
/// effective window (floored at 2, as for [`DtwCfg`]).
#[derive(Debug, Clone)]
pub struct ClusterCfg {
    epsilon: f64,
    min_cluster_size: usize,
    metric: AugursDtwMetric,
    window: usize,
}

/// Cluster op: buffers a window of per-series readings and DBSCAN-clusters the
/// series by their pairwise DTW distances each cycle. `Cfg` = resolved config,
/// `State` = the sliding window of per-tick multi-series samples.
pub struct AugursClusterOp;

#[op(build = augurs_cluster)]
impl Op for AugursClusterOp {
    type Cfg = ClusterCfg;
    type State = VecDeque<Vec<f64>>;
    type In<'a> = (&'a Vec<f64>,);
    type Out = AugursClusters;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut ClusterCfg,
        buffer: &mut VecDeque<Vec<f64>>,
        input: (&Vec<f64>,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<AugursClusters>> {
        push_windowed(buffer, input.0.clone(), cfg.window);
        // Warm up: clustering over length-1 columns compares single points, not
        // windowed histories, so wait for at least two samples.
        if buffer.len() < 2 {
            return Ok(Tick::Quiet);
        }

        let series = transpose_window(buffer);
        // Need at least two series to cluster.
        if series.len() < 2 {
            return Ok(Tick::Quiet);
        }
        let refs: Vec<&[f64]> = series.iter().map(Vec::as_slice).collect();
        let matrix = distance_matrix(cfg.metric, &refs);
        let clusters = DbscanClusterer::new(cfg.epsilon, cfg.min_cluster_size).fit(&matrix);

        Ok(Tick::Value(AugursClusters {
            labels: clusters.iter().map(|c| c.as_i32()).collect(),
        }))
    }
}

/// Adds the [`augurs_cluster`](AugursClusterOps::augurs_cluster) op to streams
/// of per-series readings. Bring it into scope with
/// `use wingfoil::adapters::augurs::AugursClusterOps;`.
pub trait AugursClusterOps {
    /// Maintain a sliding window of per-series readings (one `f64` per series
    /// per tick) and emit an [`AugursClusters`] each tick: a DBSCAN cluster
    /// label per series (`-1` = noise), computed from the pairwise DTW distances
    /// between the series' windowed histories.
    fn augurs_cluster(&self, config: impl Into<AugursClusterConfig>) -> Stream<AugursClusters>;
}

impl AugursClusterOps for Stream<Vec<f64>> {
    fn augurs_cluster(&self, config: impl Into<AugursClusterConfig>) -> Stream<AugursClusters> {
        let config = config.into();
        let cfg = ClusterCfg {
            epsilon: config.epsilon,
            min_cluster_size: config.min_cluster_size,
            metric: config.metric,
            window: config.window.max(2),
        };
        self.wire(|b, h| b.augurs_cluster(h, cfg))
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
