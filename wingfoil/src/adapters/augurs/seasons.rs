use std::collections::VecDeque;
use std::rc::Rc;

use augurs::seasons::{Detector, PeriodogramDetector};

use super::AugursSeasons;
use crate::types::*;

/// Configuration for [`AugursSeasonsOperators::augurs_seasons`].
#[derive(Debug, Clone)]
pub struct AugursSeasonsConfig {
    /// Number of recent points retained as the detection window.
    pub window: usize,
    /// Minimum number of buffered points before detection runs.
    pub min_points: usize,
    /// Shortest seasonal period to consider (in samples). `None` uses the
    /// augurs default.
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
    /// `window`, with default period bounds.
    fn from(window: usize) -> Self {
        Self::new(window)
    }
}

pub(crate) struct AugursSeasonsNode {
    upstream: Rc<dyn Stream<f64>>,
    detector: PeriodogramDetector,
    window: usize,
    min_points: usize,
    buffer: VecDeque<f64>,
    value: AugursSeasons,
}

impl AugursSeasonsNode {
    fn new(upstream: Rc<dyn Stream<f64>>, config: AugursSeasonsConfig) -> Self {
        let mut builder = PeriodogramDetector::builder();
        if let Some(min_period) = config.min_period {
            builder = builder.min_period(min_period);
        }
        if let Some(max_period) = config.max_period {
            builder = builder.max_period(max_period);
        }
        let cap = config.window.max(config.min_points);
        Self {
            upstream,
            detector: builder.build(),
            window: config.window,
            min_points: config.min_points,
            buffer: VecDeque::with_capacity(cap),
            value: AugursSeasons::default(),
        }
    }
}

#[node(active = [upstream], output = value: AugursSeasons)]
impl MutableNode for AugursSeasonsNode {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.buffer.push_back(self.upstream.peek_value());
        while self.buffer.len() > self.window {
            self.buffer.pop_front();
        }
        if self.buffer.len() < self.min_points {
            return Ok(false);
        }

        let data: Vec<f64> = self.buffer.iter().copied().collect();
        self.value = AugursSeasons {
            periods: self.detector.detect(&data),
        };
        Ok(true)
    }
}

/// Adds the [`augurs_seasons`](AugursSeasonsOperators::augurs_seasons) operator
/// to `f64` streams.
pub trait AugursSeasonsOperators {
    /// Maintain a sliding window of this stream and, once `min_points` have
    /// arrived, emit an [`AugursSeasons`] each tick with the seasonal period
    /// lengths detected by a periodogram.
    ///
    /// augurs estimates periods with a Welch periodogram over power-of-two FFT
    /// segments, so the reported lengths are **approximate** (coarsely binned) —
    /// good for spotting *that* a season exists and its rough scale, not for
    /// exact period recovery.
    #[must_use]
    fn augurs_seasons(
        self: &Rc<Self>,
        config: impl Into<AugursSeasonsConfig>,
    ) -> Rc<dyn Stream<AugursSeasons>>;
}

impl AugursSeasonsOperators for dyn Stream<f64> {
    fn augurs_seasons(
        self: &Rc<Self>,
        config: impl Into<AugursSeasonsConfig>,
    ) -> Rc<dyn Stream<AugursSeasons>> {
        AugursSeasonsNode::new(self.clone(), config.into()).into_stream()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::*;
    use crate::nodes::*;
    use std::time::Duration;

    /// A clean period-12 sine wave should be detected as having a ~12-sample
    /// season.
    #[test]
    fn seasons_detects_known_period() {
        let series = ticker(Duration::from_secs(1))
            .count()
            .map(|n| (n as f64 * std::f64::consts::TAU / 12.0).sin());
        let seasons = series.augurs_seasons(AugursSeasonsConfig::new(96));
        let captured = seasons.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(96))
            .unwrap();

        let last = seasons.peek_value();
        assert!(
            last.periods.iter().any(|&p| (10..=14).contains(&p)),
            "expected a period near 12, got {:?}",
            last.periods
        );
        let dominant = last.dominant().unwrap();
        assert!((10..=14).contains(&dominant), "dominant was {dominant}");
    }

    /// The node stays silent until `min_points` have arrived.
    #[test]
    fn seasons_waits_for_min_points() {
        let series = ticker(Duration::from_secs(1))
            .count()
            .map(|n| (n as f64 * std::f64::consts::TAU / 12.0).sin());
        let seasons = series.augurs_seasons(AugursSeasonsConfig::new(96).with_min_points(50));
        let captured = seasons.clone().collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(20))
            .unwrap();
        assert!(captured.peek_value().is_empty());
    }
}
