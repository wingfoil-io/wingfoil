use std::collections::VecDeque;
use std::rc::Rc;

use anyhow::Context;
use augurs::ets::AutoETS;
use augurs::{Fit, Predict};

use super::AugursForecast;
use crate::types::*;

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
        }
    }

    /// Request prediction intervals at the given confidence `level` (0..1).
    #[must_use]
    pub fn with_level(mut self, level: f64) -> Self {
        self.level = Some(level);
        self
    }

    /// Set the minimum number of points required before fitting begins.
    #[must_use]
    pub fn with_min_points(mut self, min_points: usize) -> Self {
        self.min_points = min_points;
        self
    }

    /// The effective minimum, never below the `AutoETS` floor.
    fn effective_min_points(&self) -> usize {
        self.min_points.max(12)
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
    buffer: VecDeque<f64>,
    value: AugursForecast,
}

impl AugursForecastNode {
    fn new(upstream: Rc<dyn Stream<f64>>, config: AugursForecastConfig) -> Self {
        let window = config.window.max(config.effective_min_points());
        Self {
            upstream,
            config,
            buffer: VecDeque::with_capacity(window),
            value: AugursForecast::default(),
        }
    }
}

#[node(active = [upstream], output = value: AugursForecast)]
impl MutableNode for AugursForecastNode {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.buffer.push_back(self.upstream.peek_value());
        while self.buffer.len() > self.config.window {
            self.buffer.pop_front();
        }
        if self.buffer.len() < self.config.effective_min_points() {
            return Ok(false);
        }

        let data: Vec<f64> = self.buffer.iter().copied().collect();
        let fitted = AutoETS::non_seasonal()
            .fit(&data)
            .context("augurs_forecast: ETS fit failed")?;
        let forecast = fitted
            .predict(self.config.horizon, self.config.level)
            .context("augurs_forecast: ETS predict failed")?;

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
