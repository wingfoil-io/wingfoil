use std::cmp::Eq;
use std::hash::Hash;
use std::rc::Rc;

use crate::queue::TimeQueue;
use crate::types::*;
use derive_new::new;

/// Emits it's source delayed by the specified time
#[derive(new)]
pub(crate) struct DelayStream<T: Element + Hash + Eq> {
    #[new(default)]
    value: T,
    #[new(default)]
    queue: TimeQueue<T>,
    upstream: Rc<dyn Stream<T>>,
    delay: NanoTime,
}

impl<T: Element + Hash + Eq> MutableNode for DelayStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> bool {
        if self.delay == NanoTime::ZERO {
            // just tick on this cycle
            self.value = self.upstream.peek_value();
            true
        } else {
            let current_time = state.time();
            let mut ticked = false;
            if state.ticked(self.upstream.clone().as_node()) {
                let next_time = current_time + self.delay;
                state.add_callback(next_time);
                self.queue.push(self.upstream.peek_value(), next_time)
            }
            while self.queue.pending(current_time) {
                self.value = self.queue.pop();
                ticked = true;
            }
            ticked
        }
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<T: Element + Hash + Eq> StreamPeekRef<T> for DelayStream<T> {
    fn peek_ref(&self) -> &T {
        &self.value
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
            vec![captured_source.clone().as_node(), captured_delayed.clone().as_node()],
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
