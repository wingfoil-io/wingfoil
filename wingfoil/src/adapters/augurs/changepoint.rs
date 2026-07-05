use std::collections::VecDeque;
use std::rc::Rc;

use augurs::changepoint::{Detector, NormalGammaDetector, dist::NormalGamma};

use super::AugursChangepoints;
use crate::types::*;

/// Configuration for [`AugursChangepointOperators::augurs_changepoint`].
#[derive(Debug, Clone)]
pub struct AugursChangepointConfig {
    /// Number of recent points retained as the detection window.
    pub window: usize,
    /// Minimum number of buffered points before detection runs. Until this many
    /// points have arrived the node does not tick.
    pub min_points: usize,
    /// Hazard rate for the Bayesian online changepoint detector: the prior
    /// expected run length between changepoints. Larger values make the
    /// detector more conservative (fewer changepoints). Defaults to `250`.
    pub hazard_lambda: f64,
}

impl AugursChangepointConfig {
    /// Detect changepoints over the last `window` points with the default
    /// hazard rate.
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
    /// `window`, with default hazard rate.
    fn from(window: usize) -> Self {
        Self::new(window)
    }
}

pub(crate) struct AugursChangepointNode {
    upstream: Rc<dyn Stream<f64>>,
    config: AugursChangepointConfig,
    buffer: VecDeque<f64>,
    value: AugursChangepoints,
}

impl AugursChangepointNode {
    fn new(upstream: Rc<dyn Stream<f64>>, config: AugursChangepointConfig) -> Self {
        let cap = config.window.max(config.min_points);
        Self {
            upstream,
            config,
            buffer: VecDeque::with_capacity(cap),
            value: AugursChangepoints::default(),
        }
    }
}

#[node(active = [upstream], output = value: AugursChangepoints)]
impl MutableNode for AugursChangepointNode {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.buffer.push_back(self.upstream.peek_value());
        while self.buffer.len() > self.config.window {
            self.buffer.pop_front();
        }
        if self.buffer.len() < self.config.min_points {
            return Ok(false);
        }

        let data: Vec<f64> = self.buffer.iter().copied().collect();
        // BOCPD is stateful (it steps through the series), so build a fresh
        // detector each cycle and re-scan the current window.
        let mut detector = NormalGammaDetector::normal_gamma(
            self.config.hazard_lambda,
            NormalGamma::new_unchecked(0.0, 1.0, 1.0, 1.0),
        );
        // BOCPD always reports index 0 (the start of the first run) as a
        // changepoint; it is an artifact of the window start, not a real
        // regime change, so drop it.
        let indices = detector
            .detect_changepoints(&data)
            .into_iter()
            .filter(|&i| i != 0)
            .collect();
        self.value = AugursChangepoints { indices };
        Ok(true)
    }
}

/// Adds the [`augurs_changepoint`](AugursChangepointOperators::augurs_changepoint)
/// operator to `f64` streams.
pub trait AugursChangepointOperators {
    /// Maintain a sliding window of this stream and, once `min_points` have
    /// arrived, emit an [`AugursChangepoints`] each tick with the indices,
    /// within the window, at which Bayesian online changepoint detection found a
    /// changepoint. The window-start index (`0`), which BOCPD always reports, is
    /// excluded — only genuine internal regime changes are returned.
    #[must_use]
    fn augurs_changepoint(
        self: &Rc<Self>,
        config: impl Into<AugursChangepointConfig>,
    ) -> Rc<dyn Stream<AugursChangepoints>>;
}

impl AugursChangepointOperators for dyn Stream<f64> {
    fn augurs_changepoint(
        self: &Rc<Self>,
        config: impl Into<AugursChangepointConfig>,
    ) -> Rc<dyn Stream<AugursChangepoints>> {
        AugursChangepointNode::new(self.clone(), config.into()).into_stream()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::*;
    use crate::nodes::*;
    use std::time::Duration;

    /// A series that jumps from a low mean to a high mean partway through should
    /// produce a changepoint somewhere after the jump.
    #[test]
    fn changepoint_detects_level_shift() {
        // First ~20 samples near 0, then a jump to near 50.
        let series = ticker(Duration::from_secs(1)).count().map(|n| {
            let noise = (n as f64 * 0.7).sin() * 0.5;
            if n > 20 { 50.0 + noise } else { noise }
        });
        let changes = series.augurs_changepoint(AugursChangepointConfig::new(60));
        let captured = changes.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(50))
            .unwrap();

        let last = changes.peek_value();
        assert!(
            last.any(),
            "expected a changepoint after the level shift, got {last:?}"
        );
        // The shift happens around window index 20; a detected changepoint
        // should land after the first few points.
        assert!(
            last.indices.iter().any(|&i| i >= 10),
            "expected a changepoint past the initial regime, got {:?}",
            last.indices
        );
    }

    /// A perfectly steady series yields no changepoints.
    #[test]
    fn changepoint_quiet_when_steady() {
        let series = ticker(Duration::from_secs(1))
            .count()
            .map(|n| 10.0 + (n as f64 * 0.5).sin() * 0.1);
        let changes = series.augurs_changepoint(40);
        let captured = changes.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(40))
            .unwrap();
        assert!(
            !changes.peek_value().any(),
            "steady series should have no changepoints, got {:?}",
            changes.peek_value().indices
        );
    }
}
