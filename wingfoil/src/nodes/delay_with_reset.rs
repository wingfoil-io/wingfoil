use std::cmp::Eq;
use std::hash::Hash;
use std::rc::Rc;

use crate::queue::TimeQueue;
use crate::types::*;
use derive_new::new;

/// Like [`DelayStream`](super::delay::DelayStream) but with a reset trigger.
/// When the trigger fires, the output snaps to the current upstream value and
/// the pending queue is cleared.
#[derive(new)]
pub(crate) struct DelayWithResetStream<T: Element + Hash + Eq> {
    #[new(default)]
    value: T,
    #[new(default)]
    queue: TimeQueue<T>,
    #[new(default)]
    initialized: bool,
    upstream: Rc<dyn Stream<T>>,
    trigger: Rc<dyn Node>,
    delay: NanoTime,
}

impl<T: Element + Hash + Eq> MutableNode for DelayWithResetStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        if state.ticked(self.trigger.clone()) {
            // Reset: snap to current upstream value, clear pending queue
            self.value = self.upstream.peek_value();
            self.queue.clear();
            self.initialized = true;
            return Ok(true);
        }

        if self.delay == NanoTime::ZERO {
            self.value = self.upstream.peek_value();
            Ok(true)
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
            while self.queue.pending(current_time) {
                self.value = self.queue.pop();
                ticked = true;
            }
            Ok(ticked)
        }
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(
            vec![self.upstream.clone().as_node(), self.trigger.clone()],
            vec![],
        )
    }
}

impl<T: Element + Hash + Eq> StreamPeekRef<T> for DelayWithResetStream<T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::Dep::*;
    use crate::graph::*;
    use crate::nodes::*;
    use std::time::Duration;

    #[test]
    fn delay_never_reset() {
        let period = Duration::from_nanos(100);
        let src = ticker(period).count();
        bimap(
            Active(src.delay_with_reset(period * 3, never())),
            Active(src.delay(period * 3)),
            |a, b| {
                assert_eq!(a, b);
                ()
            },
        )
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(20))
        .unwrap();
    }

    fn assert_snaps_on_trigger(trigger_period: Duration, expected: Vec<(u64, u64, u64)>) {
        let period = Duration::from_nanos(100);
        let source = ticker(period).count();
        let trigger = ticker(trigger_period);
        trimap(
            Active(source.clone()),
            Active(source.delay(period * 5)),
            Active(source.delay_with_reset(period * 5, trigger)),
            |a, b, c| (a, b, c),
        )
        .accumulate()
        .finally(move |x, _| {
            assert_eq!(x, expected);
            Ok(())
        })
        .run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(period * 20),
        )
        .unwrap();
    }

    #[test]
    fn delay_with_reset_snaps_on_trigger() {
        assert_snaps_on_trigger(
            Duration::from_nanos(1000),
            vec![
                (1, 1, 1),
                (2, 1, 1),
                (3, 1, 1),
                (4, 1, 1),
                (5, 1, 1),
                (6, 1, 1),
                (7, 2, 2),
                (8, 3, 3),
                (9, 4, 4),
                (10, 5, 5),
                (11, 6, 11),
                (12, 7, 11),
                (13, 8, 11),
                (14, 9, 11),
                (15, 10, 11),
                (16, 11, 11),
                (17, 12, 12),
                (18, 13, 13),
                (19, 14, 14),
                (20, 15, 15),
                (21, 16, 21),
                (22, 17, 21),
            ],
        );
    }

    #[test]
    fn delay_with_reset_snaps_on_trigger_2() {
        assert_snaps_on_trigger(
            Duration::from_nanos(750),
            vec![
                (1, 1, 1),
                (2, 1, 1),
                (3, 1, 1),
                (4, 1, 1),
                (5, 1, 1),
                (6, 1, 1),
                (7, 2, 2),
                (8, 3, 3),
                (8, 3, 8),
                (9, 4, 8),
                (10, 5, 8),
                (11, 6, 8),
                (12, 7, 8),
                (13, 8, 8),
                (14, 9, 9),
                (15, 10, 10),
                (16, 11, 16),
                (17, 12, 16),
                (18, 13, 16),
                (19, 14, 16),
                (20, 15, 16),
                (21, 16, 16),
                (22, 17, 17),
            ],
        );
    }
}
