use std::cmp::Eq;
use std::hash::Hash;
use std::rc::Rc;

use crate::queue::TimeQueue;
use crate::types::*;
use derive_new::new;

/// Shared delay machinery used by both [`DelayStream`] and
/// [`DelayWithResetStream`](super::delay_with_reset::DelayWithResetStream).
///
/// Holds the delayed output value, the pending queue and the delay window, and
/// implements the core "push on upstream tick, pop when due" advance logic.
#[derive(new)]
pub(crate) struct DelayCore<T: Element + Hash + Eq> {
    #[new(default)]
    pub(crate) value: T,
    #[new(default)]
    queue: TimeQueue<T>,
    #[new(default)]
    initialized: bool,
    upstream: Rc<dyn Stream<T>>,
    delay: NanoTime,
}

impl<T: Element + Hash + Eq> DelayCore<T> {
    /// The upstream stream this core delays, as a node (for `upstreams()`).
    pub(crate) fn upstream_node(&self) -> Rc<dyn Node> {
        self.upstream.clone().as_node()
    }

    /// Reset the core: snap output to the current upstream value and clear the
    /// pending queue.
    pub(crate) fn reset(&mut self) {
        self.value = self.upstream.peek_value();
        self.queue.clear();
        self.initialized = true;
    }

    /// Advance the delay by one cycle (no reset handling). Returns whether the
    /// output value ticked.
    pub(crate) fn advance(&mut self, state: &mut GraphState) -> bool {
        if self.delay == NanoTime::ZERO {
            // just tick on this cycle
            self.value = self.upstream.peek_value();
            true
        } else {
            let current_time = state.time();
            let mut ticked = false;
            if state.ticked(self.upstream.clone().as_node()) {
                if !self.initialized {
                    self.value = self.upstream.peek_value();
                    self.initialized = true;
                }
                let next_time = current_time + self.delay;
                state.add_callback(next_time);
                self.queue.push(self.upstream.peek_value(), next_time)
            }
            while let Some(value) = self.queue.pop_if_pending(current_time) {
                self.value = value;
                ticked = true;
            }
            ticked
        }
    }
}

/// Emits it's source delayed by the specified time
pub(crate) struct DelayStream<T: Element + Hash + Eq> {
    core: DelayCore<T>,
}

impl<T: Element + Hash + Eq> DelayStream<T> {
    pub(crate) fn new(upstream: Rc<dyn Stream<T>>, delay: NanoTime) -> Self {
        DelayStream {
            core: DelayCore::new(upstream, delay),
        }
    }
}

impl<T: Element + Hash + Eq> MutableNode for DelayStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        Ok(self.core.advance(state))
    }
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.core.upstream_node()], vec![])
    }
}

impl<T: Element + Hash + Eq> StreamPeekRef<T> for DelayStream<T> {
    fn peek_ref(&self) -> &T {
        &self.core.value
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::graph::*;
    use crate::nodes::*;
    use std::time::Duration;

    #[test]
    fn delay_works() {
        //env_logger::init();
        let source = ticker(Duration::from_nanos(100))
            .count()
            .logged("source", log::Level::Info);
        let delayed = source
            .delay(Duration::from_nanos(10))
            .logged("delayed", log::Level::Info);
        let captured_source = source.collect();
        let captured_delayed = delayed.collect();
        let run_mode = RunMode::HistoricalFrom(NanoTime::ZERO);
        let run_for = RunFor::Cycles(6);
        let mut graph = Graph::new(
            vec![
                captured_source.clone().as_node(),
                captured_delayed.clone().as_node(),
            ],
            run_mode,
            run_for,
        );
        let expected_source = vec![
            ValueAt {
                value: 1,
                time: NanoTime::new(0),
            },
            ValueAt {
                value: 2,
                time: NanoTime::new(100),
            },
            ValueAt {
                value: 3,
                time: NanoTime::new(200),
            },
        ];
        let expected_delayed = vec![
            ValueAt {
                value: 1,
                time: NanoTime::new(10),
            },
            ValueAt {
                value: 2,
                time: NanoTime::new(110),
            },
            ValueAt {
                value: 3,
                time: NanoTime::new(210),
            },
        ];
        graph.run().unwrap();
        assert_eq!(expected_source, captured_source.peek_value());
        assert_eq!(expected_delayed, captured_delayed.peek_value());
    }

    #[test]
    fn long_delay_works() {
        //env_logger::init();
        let delayed = ticker(Duration::from_nanos(10))
            .count()
            .delay(Duration::from_nanos(100))
            .collect();
        delayed
            .run(
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Duration(Duration::from_nanos(120)),
            )
            .unwrap();
        let expected = vec![
            ValueAt {
                value: 1,
                time: NanoTime::new(100),
            },
            ValueAt {
                value: 2,
                time: NanoTime::new(110),
            },
            ValueAt {
                value: 3,
                time: NanoTime::new(120),
            },
            ValueAt {
                value: 4,
                time: NanoTime::new(130),
            },
        ];
        assert_eq!(expected, delayed.peek_value());
    }

    #[test]
    fn delay_initializes_to_first_value() {
        // Passive reads of a delayed stream return the first upstream value
        // (not T::default()) before the delay elapses.
        let source = ticker(Duration::from_secs(1)).count().map(|x| x as i64 + 4); // 5, 6, 7, 8, 9, ...
        let delayed = source.delay(Duration::from_secs(5));
        let diff = bimap(Dep::Active(source), Dep::Passive(delayed), |a, b| a - b);
        diff.accumulate()
            .finally(|res, _| {
                assert_eq!(res, vec![0, 1, 2, 3, 4, 5, 5, 5, 5, 5]);
                Ok(())
            })
            .run(
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Duration(Duration::from_secs(8)),
            )
            .unwrap();
    }

    #[test]
    fn zero_delay_works() {
        let delayed = ticker(Duration::from_nanos(10))
            .count()
            .delay(Duration::from_nanos(0))
            .collect();
        delayed
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
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
            ValueAt {
                value: 4,
                time: NanoTime::new(30),
            },
        ];
        assert_eq!(expected, delayed.peek_value());
    }
}
