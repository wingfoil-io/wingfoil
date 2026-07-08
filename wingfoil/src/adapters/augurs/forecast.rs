use std::collections::VecDeque;
use std::rc::Rc;

use anyhow::Context;
use augurs::ets::AutoETS;
use augurs::ets::trend::AutoETSTrendModel;
use augurs::mstl::MSTLModel;
use augurs::{Fit, Predict};

use super::AugursForecast;
use crate::types::*;

/// The forecasting model used by [`AugursForecastOperators::augurs_forecast`].
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

/// Configuration for [`AugursForecastOperators::augurs_forecast`].
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

pub(crate) struct AugursForecastNode {
    upstream: Rc<dyn Stream<f64>>,
    config: AugursForecastConfig,
    /// Effective window: the configured `window` grown to the model's warm-up
    /// floor so a `window` smaller than the floor still lets the node fill up
    /// and emit rather than never ticking.
    window: usize,
    /// Resolved warm-up floor, computed once (`effective_min_points`).
    min_points: usize,
    buffer: VecDeque<f64>,
    value: AugursForecast,
}

impl AugursForecastNode {
    fn new(upstream: Rc<dyn Stream<f64>>, config: AugursForecastConfig) -> Self {
        let min_points = config.effective_min_points();
        let window = config.window.max(min_points);
        Self {
            upstream,
            config,
            window,
            min_points,
            buffer: VecDeque::with_capacity(window),
            value: AugursForecast::default(),
        }
    }
}

#[node(active = [upstream], output = value: AugursForecast)]
impl MutableNode for AugursForecastNode {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        super::push_windowed(&mut self.buffer, self.upstream.peek_value(), self.window);
        if self.buffer.len() < self.min_points {
            return Ok(false);
        }

        let data: Vec<f64> = self.buffer.iter().copied().collect();
        let forecast = match &self.config.model {
            AugursForecastModel::Ets => {
                let fitted = AutoETS::non_seasonal()
                    .fit(&data)
                    .context("augurs_forecast: ETS fit failed")?;
                fitted
                    .predict(self.config.horizon, self.config.level)
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
                    .predict(self.config.horizon, self.config.level)
                    .context("augurs_forecast: MSTL predict failed")?
            }
        };

        let (lower, upper) = match forecast.intervals {
            Some(intervals) => (intervals.lower, intervals.upper),
            None => (Vec::new(), Vec::new()),
        };
        self.value = AugursForecast {
            point: forecast.point,
            lower,
            upper,
        };
        Ok(true)
    }
}

/// Adds the [`augurs_forecast`](AugursForecastOperators::augurs_forecast)
/// operator to `f64` streams.
pub trait AugursForecastOperators {
    /// Maintain a sliding window of this stream and, once `min_points` have
    /// arrived, emit an [`AugursForecast`] each tick by fitting an `AutoETS`
    /// model and predicting `horizon` steps ahead.
    #[must_use]
    fn augurs_forecast(
        self: &Rc<Self>,
        config: impl Into<AugursForecastConfig>,
    ) -> Rc<dyn Stream<AugursForecast>>;
}

impl AugursForecastOperators for dyn Stream<f64> {
    fn augurs_forecast(
        self: &Rc<Self>,
        config: impl Into<AugursForecastConfig>,
    ) -> Rc<dyn Stream<AugursForecast>> {
        AugursForecastNode::new(self.clone(), config.into()).into_stream()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::*;
    use crate::nodes::*;
    use std::time::Duration;

    /// A clean upward ramp with mild seasonality should forecast values above
    /// the last observed point, and intervals should bracket the point forecast.
    #[test]
    fn forecast_ramp_predicts_ahead() {
        let source = ticker(Duration::from_secs(1))
            .count()
            .map(|n| n as f64 + (n as f64 * 0.5).sin());
        let forecast = source.augurs_forecast(AugursForecastConfig::new(48, 3).with_level(0.9));
        let captured = forecast.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(40))
            .unwrap();

        let last = forecast.peek_value();
        assert_eq!(last.point.len(), 3, "horizon == 3 point forecasts");
        assert_eq!(last.lower.len(), 3);
        assert_eq!(last.upper.len(), 3);
        // The ramp is still rising, so the next forecast should exceed the
        // ~40th observed value (which is around 40).
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

    /// The node stays silent until `min_points` have arrived.
    #[test]
    fn forecast_waits_for_min_points() {
        let source = ticker(Duration::from_secs(1)).count().map(|n| n as f64);
        let forecast = source.augurs_forecast(AugursForecastConfig::new(64, 1).with_min_points(20));
        let captured = forecast.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(15))
            .unwrap();
        // 15 cycles < 20 min_points → never ticked.
        assert!(captured.peek_value().is_empty());
    }

    /// MSTL with a seasonal period forecasts a strongly seasonal series and
    /// reproduces its seasonal swing rather than a flat line.
    #[test]
    fn forecast_mstl_captures_season() {
        // Period-12 sine wave riding on a gentle ramp.
        let source = ticker(Duration::from_secs(1)).count().map(|n| {
            let t = n as f64;
            0.1 * t + 5.0 * (t * std::f64::consts::TAU / 12.0).sin()
        });
        let forecast = source.augurs_forecast(AugursForecastConfig::new(120, 12).mstl(vec![12]));
        let captured = forecast.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(80))
            .unwrap();

        let last = forecast.peek_value();
        assert_eq!(last.point.len(), 12, "horizon == 12 point forecasts");
        // A seasonal forecast should swing: max and min of the 12-step forecast
        // must differ by a meaningful fraction of the amplitude (10.0 pk-pk).
        let max = last.point.iter().cloned().fold(f64::MIN, f64::max);
        let min = last.point.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            max - min > 2.0,
            "expected a seasonal swing, got range {:.3} for {:?}",
            max - min,
            last.point
        );
    }

    /// A `window` smaller than the model's warm-up floor still warms up and
    /// emits — the effective window grows to the floor — rather than silently
    /// never ticking.
    #[test]
    fn forecast_window_below_floor_still_emits() {
        let source = ticker(Duration::from_secs(1))
            .count()
            .map(|n| n as f64 + (n as f64 * 0.5).sin());
        // window 10 is below the ETS floor of 12.
        let forecast = source.augurs_forecast(AugursForecastConfig::new(10, 2));
        let captured = forecast.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
            .unwrap();
        assert_eq!(
            forecast.peek_value().point.len(),
            2,
            "should emit despite window < floor"
        );
    }

    /// An MSTL period below 2 (or an empty period set) is rejected with a clear
    /// error at run time rather than an opaque augurs-internal fit failure.
    #[test]
    fn forecast_mstl_rejects_invalid_period() {
        let source = ticker(Duration::from_secs(1)).count().map(|n| n as f64);
        let forecast = source.augurs_forecast(AugursForecastConfig::new(64, 2).mstl(vec![1]));
        let captured = forecast.clone().collect();
        let result = captured.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30));
        let err = result.expect_err("period < 2 should fail the run");
        assert!(
            format!("{err:#}").contains("MSTL periods"),
            "expected a clear MSTL period error, got {err:#}"
        );
    }

    /// `(window, horizon)` tuples convert into a config.
    #[test]
    fn forecast_accepts_tuple_config() {
        let source = ticker(Duration::from_secs(1)).count().map(|n| n as f64);
        let forecast = source.augurs_forecast((32, 2));
        let captured = forecast.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
            .unwrap();
        assert_eq!(forecast.peek_value().point.len(), 2);
    }
}
