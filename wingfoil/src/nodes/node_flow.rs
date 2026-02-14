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
