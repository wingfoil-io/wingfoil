use crate::graph::{GraphState, RunMode};
use crate::types::*;
use formato::Formato;
use log::info;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Passes through its upstream value unchanged, recording start-up times and
/// printing a performance summary on shutdown.
///
/// In historical mode the summary includes both the wall-clock duration and the
/// engine-time duration so you can see the replay speedup factor.
pub struct TimedStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    value: T,
    cycles: u64,
    wall_start: Option<Instant>,
}

impl<T: Element> TimedStream<T> {
    pub fn new(upstream: Rc<dyn Stream<T>>) -> Self {
        Self {
            upstream,
            value: T::default(),
            cycles: 0,
            wall_start: None,
        }
    }
}

impl<T: Element> MutableNode for TimedStream<T> {
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }

    fn start(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
        self.wall_start = Some(Instant::now());
        Ok(())
    }

    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = self.upstream.peek_value();
        self.cycles += 1;
        Ok(true)
    }

    fn stop(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        let wall_elapsed = self.wall_start.map(|s| s.elapsed());
        let engine_elapsed = Duration::from(state.elapsed());
        let cycles_fmt = (self.cycles as f64).formato("#,###");

        let wall = wall_elapsed.unwrap_or(engine_elapsed);
        let avg_nanos = wall.as_nanos() / self.cycles.max(1) as u128;
        let avg = Duration::from_nanos(avg_nanos as u64);

        match (state.run_mode(), wall_elapsed) {
            (RunMode::HistoricalFrom(_), Some(_)) => {
                let speedup = if wall.as_secs_f64() > 0.0 {
                    engine_elapsed.as_secs_f64() / wall.as_secs_f64()
                } else {
                    f64::INFINITY
                };
                info!(
                    "{} ticks processed in {:?}, {:?} average.   \
                     Covered {:?} of historical data (x{:.1}).",
                    cycles_fmt, wall, avg, engine_elapsed, speedup,
                );
            }
            _ => {
                info!(
                    "{} ticks processed in {:?}, {:?} average.",
                    cycles_fmt, wall, avg,
                );
            }
        }
        Ok(())
    }
}

impl<T: Element> StreamPeekRef<T> for TimedStream<T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;
    use std::time::Duration;

    #[test]
    fn timed_historical() {
        let _ = env_logger::try_init();
        let result = ticker(Duration::from_millis(1)).count().timed();
        result
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert_eq!(result.peek_value(), 5);
    }

    #[test]
    fn timed_realtime() {
        let _ = env_logger::try_init();
        let result = ticker(Duration::from_millis(10)).count().timed();
        result
            .run(
                RunMode::RealTime,
                RunFor::Duration(Duration::from_millis(55)),
            )
            .unwrap();
        assert!(result.peek_value() >= 5);
    }
}
