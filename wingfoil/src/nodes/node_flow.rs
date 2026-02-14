use std::rc::Rc;

use crate::queue::TimeQueue;
use crate::types::*;

use super::FeedbackSink;

/// Suppresses upstream ticks that arrive faster than a specified interval.
pub(crate) struct ThrottleNode {
    upstream: Rc<dyn Node>,
    interval: NanoTime,
    last_emit_time: Option<NanoTime>,
}

impl ThrottleNode {
    pub fn new(upstream: Rc<dyn Node>, interval: NanoTime) -> Self {
        Self {
            upstream,
            interval,
            last_emit_time: None,
        }
    }
}

impl MutableNode for ThrottleNode {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let now = state.time();
        let should_emit = match self.last_emit_time {
            None => true,
            Some(last) => now - last >= self.interval,
        };
        if should_emit {
            self.last_emit_time = Some(now);
        }
        Ok(should_emit)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone()], vec![])
    }
}

/// Delays upstream ticks by a specified duration.
pub(crate) struct DelayNode {
    upstream: Rc<dyn Node>,
    delay: NanoTime,
    queue: TimeQueue<()>,
}

impl DelayNode {
    pub fn new(upstream: Rc<dyn Node>, delay: NanoTime) -> Self {
        Self {
            upstream,
            delay,
            queue: TimeQueue::new(),
        }
    }
}

impl MutableNode for DelayNode {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        if self.delay == NanoTime::ZERO {
            Ok(true)
        } else {
            let current_time = state.time();
            if state.ticked(self.upstream.clone()) {
                let next_time = current_time + self.delay;
                state.add_callback(next_time);
                self.queue.push((), next_time);
            }
            let mut ticked = false;
            while self.queue.pending(current_time) {
                self.queue.pop();
                ticked = true;
            }
            Ok(ticked)
        }
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone()], vec![])
    }
}

/// Propagates at most `limit` ticks from upstream.
pub(crate) struct LimitNode {
    upstream: Rc<dyn Node>,
    limit: u32,
    tick_count: u32,
}

impl LimitNode {
    pub fn new(upstream: Rc<dyn Node>, limit: u32) -> Self {
        Self {
            upstream,
            limit,
            tick_count: 0,
        }
    }
}

impl MutableNode for LimitNode {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        if self.tick_count >= self.limit {
            Ok(false)
        } else {
            self.tick_count += 1;
            Ok(true)
        }
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone()], vec![])
    }
}

/// Drops upstream ticks when `condition` is false.
pub(crate) struct FilterNode {
    upstream: Rc<dyn Node>,
    condition: Rc<dyn Stream<bool>>,
}

impl FilterNode {
    pub fn new(upstream: Rc<dyn Node>, condition: Rc<dyn Stream<bool>>) -> Self {
        Self {
            upstream,
            condition,
        }
    }
}

impl MutableNode for FilterNode {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        Ok(self.condition.peek_value())
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(
            vec![self.upstream.clone()],
            vec![self.condition.clone().as_node()],
        )
    }
}

/// Sends `()` to a [FeedbackSink] on each upstream tick.
pub(crate) struct FeedbackSendNode {
    upstream: Rc<dyn Node>,
    sink: FeedbackSink<()>,
}

impl FeedbackSendNode {
    pub fn new(upstream: Rc<dyn Node>, sink: FeedbackSink<()>) -> Self {
        Self { upstream, sink }
    }
}

impl MutableNode for FeedbackSendNode {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        self.sink.send((), state);
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone()], vec![])
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;
    use crate::queue::ValueAt;
    use std::time::Duration;

    #[test]
    fn node_throttle_suppresses_fast_ticks() {
        // Source ticks every 10ns, throttle interval is 25ns
        // Throttled node fires at t=0, t=30, t=60
        ticker(Duration::from_nanos(10))
            .throttle(Duration::from_nanos(25))
            .count()
            .collect()
            .finally(|values, _| {
                let expected = vec![
                    ValueAt::new(1, NanoTime::new(0)),
                    ValueAt::new(2, NanoTime::new(30)),
                    ValueAt::new(3, NanoTime::new(60)),
                ];
                assert_eq!(expected, values);
                Ok(())
            })
            .run(
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Duration(Duration::from_nanos(60)),
            )
            .unwrap();
    }

    #[test]
    fn node_throttle_zero_interval_passes_all() {
        ticker(Duration::from_nanos(10))
            .throttle(Duration::from_nanos(0))
            .count()
            .collect()
            .finally(|values, _| {
                let expected = vec![
                    ValueAt::new(1, NanoTime::new(0)),
                    ValueAt::new(2, NanoTime::new(10)),
                    ValueAt::new(3, NanoTime::new(20)),
                ];
                assert_eq!(expected, values);
                Ok(())
            })
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();
    }

    #[test]
    fn node_delay_shifts_ticks() {
        // Source ticks every 100ns, delay by 10ns
        // Upstream ticks at t=0,100,200; delayed ticks arrive at t=10,110,210
        ticker(Duration::from_nanos(100))
            .delay(Duration::from_nanos(10))
            .count()
            .collect()
            .finally(|values, _| {
                let expected = vec![
                    ValueAt::new(1, NanoTime::new(10)),
                    ValueAt::new(2, NanoTime::new(110)),
                    ValueAt::new(3, NanoTime::new(210)),
                ];
                assert_eq!(expected, values);
                Ok(())
            })
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
            .unwrap();
    }

    #[test]
    fn node_delay_zero_passes_immediately() {
        ticker(Duration::from_nanos(10))
            .delay(Duration::from_nanos(0))
            .count()
            .collect()
            .finally(|values, _| {
                let expected = vec![
                    ValueAt::new(1, NanoTime::new(0)),
                    ValueAt::new(2, NanoTime::new(10)),
                    ValueAt::new(3, NanoTime::new(20)),
                ];
                assert_eq!(expected, values);
                Ok(())
            })
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();
    }

    #[test]
    fn node_limit_caps_ticks() {
        // Source ticks 10 times, limit to 3
        ticker(Duration::from_nanos(10))
            .limit(3)
            .count()
            .collect()
            .finally(|values, _| {
                let expected = vec![
                    ValueAt::new(1, NanoTime::new(0)),
                    ValueAt::new(2, NanoTime::new(10)),
                    ValueAt::new(3, NanoTime::new(20)),
                ];
                assert_eq!(expected, values);
                Ok(())
            })
            .run(
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Duration(Duration::from_nanos(90)),
            )
            .unwrap();
    }

    #[test]
    fn node_filter_gates_ticks() {
        // Source ticks every 10ns producing 1,2,3,...
        // Filter passes only when count is even
        let src = ticker(Duration::from_nanos(10));
        let is_even = src.count().map(|i| i % 2 == 0);
        src.filter(is_even)
            .count()
            .collect()
            .finally(|values, _| {
                // Ticks at t=0(count=1,odd), t=10(2,even), t=20(3,odd),
                //         t=30(4,even), t=40(5,odd), t=50(6,even)
                let expected = vec![
                    ValueAt::new(1, NanoTime::new(10)),
                    ValueAt::new(2, NanoTime::new(30)),
                    ValueAt::new(3, NanoTime::new(50)),
                ];
                assert_eq!(expected, values);
                Ok(())
            })
            .run(
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Duration(Duration::from_nanos(50)),
            )
            .unwrap();
    }

    #[test]
    fn node_feedback_sends_signal() {
        // Verify that feedback on a node triggers the feedback source
        let period = Duration::from_nanos(100);
        let (tx, rx) = feedback_node();

        // fire feedback on every source tick
        let fb = ticker(period).feedback(tx);

        // rx ticks whenever the feedback fires; count those ticks
        let res = rx.count().collect().finally(|values, _| {
            // feedback delivers on next cycle, so rx_count lags by one tick
            let expected = vec![
                ValueAt::new(1, NanoTime::new(1)),
                ValueAt::new(2, NanoTime::new(101)),
                ValueAt::new(3, NanoTime::new(201)),
            ];
            assert_eq!(expected, values);
            Ok(())
        });

        Graph::new(
            vec![fb, res],
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(period * 2),
        )
        .run()
        .unwrap();
    }
}
