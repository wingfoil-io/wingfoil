use crate::types::*;
use derive_new::new;
use std::rc::Rc;

/// Suppresses upstream values that arrive faster than a specified interval.
/// Passes the first value through, then ignores subsequent values until the
/// interval elapses.
#[derive(new)]
pub(crate) struct ThrottleStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    interval: NanoTime,
    #[new(default)]
    last_emit_time: Option<NanoTime>,
    #[new(default)]
    value: T,
}

impl<T: Element> MutableNode for ThrottleStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let now = state.time();
        let should_emit = match self.last_emit_time {
            None => true,
            Some(last) => now - last >= self.interval,
        };
        if should_emit {
            self.value = self.upstream.peek_value();
            self.last_emit_time = Some(now);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<T: Element> StreamPeekRef<T> for ThrottleStream<T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;
    use crate::queue::ValueAt;

    #[test]
    fn throttle_suppresses_fast_ticks() {
        // Source ticks every 10ns, throttle interval is 25ns
        // At t=0: emit (first tick)
        // At t=10: suppress (10 < 25)
        // At t=20: suppress (20 < 25)
        // At t=30: emit (30 >= 25)
        // At t=40: suppress (10 < 25)
        // At t=50: suppress (20 < 25)
        // At t=60: emit (30 >= 25)
        let throttled = ticker(Duration::from_nanos(10))
            .count()
            .throttle(Duration::from_nanos(25))
            .collect();
        throttled
            .run(
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Duration(Duration::from_nanos(60)),
            )
            .unwrap();
        let expected = vec![
            ValueAt {
                value: 1,
                time: NanoTime::new(0),
            },
            ValueAt {
                value: 4,
                time: NanoTime::new(30),
            },
            ValueAt {
                value: 7,
                time: NanoTime::new(60),
            },
        ];
        assert_eq!(expected, throttled.peek_value());
    }

    #[test]
    fn throttle_zero_interval_passes_all() {
        let throttled = ticker(Duration::from_nanos(10))
            .count()
            .throttle(Duration::from_nanos(0))
            .collect();
        throttled
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();
        let expected = vec![
            ValueAt {
                value: 1,
                time: NanoTime::new(0),
            },
            ValueAt {
                value: 2,
                time: NanoTime::new(10),
            },
            ValueAt {
                value: 3,
                time: NanoTime::new(20),
            },
        ];
        assert_eq!(expected, throttled.peek_value());
    }

    #[test]
    fn throttle_exact_interval_emits() {
        // Source ticks every 10ns, throttle interval is 10ns
        // Every tick should emit since interval matches exactly
        let throttled = ticker(Duration::from_nanos(10))
            .count()
            .throttle(Duration::from_nanos(10))
            .collect();
        throttled
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();
        let expected = vec![
            ValueAt {
                value: 1,
                time: NanoTime::new(0),
            },
            ValueAt {
                value: 2,
                time: NanoTime::new(10),
            },
            ValueAt {
                value: 3,
                time: NanoTime::new(20),
            },
        ];
        assert_eq!(expected, throttled.peek_value());
    }
}
