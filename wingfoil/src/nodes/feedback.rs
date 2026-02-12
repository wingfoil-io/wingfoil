use std::cell::{Cell, RefCell};
use std::cmp::Eq;
use std::hash::Hash;
use std::rc::Rc;

use crate::queue::TimeQueue;
use crate::types::*;

/// Source end of a [feedback] channel. Has no upstreams so the graph
/// sees no cycle. Values pushed by the paired [FeedbackSink] are
/// emitted on the next engine cycle.
pub(crate) struct FeedbackStream<T: Element + Hash + Eq> {
    value: T,
    queue: Rc<RefCell<TimeQueue<T>>>,
    node_id: Rc<Cell<Option<usize>>>,
}

impl<T: Element + Hash + Eq> StreamPeekRef<T> for FeedbackStream<T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}

impl<T: Element + Hash + Eq> MutableNode for FeedbackStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let mut ticked = false;
        while self.queue.borrow().pending(state.time()) {
            self.value = self.queue.borrow_mut().pop();
            ticked = true;
        }
        Ok(ticked)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::default()
    }

    fn setup(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        self.node_id.set(Some(state.current_node_id()));
        Ok(())
    }
}

/// Write end of a [feedback] channel. Clone-able and can be moved
/// into closures. Calling [send](FeedbackSink::send) pushes a value
/// onto the shared queue and schedules the paired source stream to
/// cycle.
pub struct FeedbackSink<T: Element + Hash + Eq> {
    queue: Rc<RefCell<TimeQueue<T>>>,
    node_id: Rc<Cell<Option<usize>>>,
}

impl<T: Element + Hash + Eq> Clone for FeedbackSink<T> {
    fn clone(&self) -> Self {
        Self {
            queue: self.queue.clone(),
            node_id: self.node_id.clone(),
        }
    }
}

impl<T: Element + Hash + Eq> FeedbackSink<T> {
    /// Push a value and schedule the paired source stream to cycle.
    pub fn send(&self, value: T, state: &mut GraphState) {
        let time = state.time();
        self.queue.borrow_mut().push(value, time);
        state.add_callback_for_node(self.node_id.get().unwrap(), time);
    }
}

/// Creates a feedback channel. Returns a ([FeedbackSink], [Stream])
/// pair. The sink pushes values onto a shared [TimeQueue]; the source
/// stream pops them on the next engine cycle.
///
/// ```
/// # use wingfoil::*;
/// # use std::time::Duration;
/// let src = ticker(Duration::from_nanos(100)).count();
/// let (tx, rx) = feedback::<u64>();
///
/// let sum = bimap(
///     Dep::Active(src.clone()),
///     Dep::Passive(rx),
///     |count, prev| count + prev,
/// );
///
/// let writer = sum.feedback(tx);
/// ```
pub fn feedback<T: Element + Hash + Eq>() -> (FeedbackSink<T>, Rc<dyn Stream<T>>) {
    let queue = Rc::new(RefCell::new(TimeQueue::new()));
    let node_id = Rc::new(Cell::new(None));
    let stream = FeedbackStream {
        value: T::default(),
        queue: queue.clone(),
        node_id: node_id.clone(),
    };
    let sink = FeedbackSink { queue, node_id };
    (sink, stream.into_stream())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::*;
    use crate::nodes::*;
    use std::time::Duration;

    #[test]
    fn feedback_delivers_on_next_cycle() {
        let src = ticker(Duration::from_nanos(100)).count();
        let (tx, rx) = feedback::<u64>();
        let collected = rx.collect();

        let writer = src.feedback(tx);

        let mut graph = Graph::new(
            vec![collected.clone().as_node(), writer],
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(4),
        );
        graph.run().unwrap();

        let values: Vec<u64> = collected.peek_value().iter().map(|v| v.value).collect();
        // Cycle 1 (t=0):   src=1, tx.send(1) → schedules rx at t=0
        // Cycle 2 (t=0):   rx emits 1
        // Cycle 3 (t=100): src=2, tx.send(2) → schedules rx at t=100
        // Cycle 4 (t=100): rx emits 2
        assert_eq!(values, vec![1, 2]);
    }

    #[test]
    fn feedback_loop_accumulates() {
        // Running sum via feedback:
        // ticker:  1, 2, 3, 4
        // fb:      0, 1, 3, 6  (previous sum)
        // sum:     1, 3, 6, 10
        //
        // Use Duration-based termination because each feedback
        // round-trip consumes an engine cycle at the same time.
        let src = ticker(Duration::from_nanos(100)).count();
        let (tx, rx) = feedback::<u64>();

        let sum = bimap(Dep::Active(src), Dep::Passive(rx), |count, prev| {
            count + prev
        });
        let collected = sum.collect();

        let writer = sum.feedback(tx);

        let mut graph = Graph::new(
            vec![collected.clone().as_node(), writer],
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(Duration::from_nanos(300)),
        );
        graph.run().unwrap();

        let values: Vec<u64> = collected.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(values, vec![1, 3, 6, 10]);
    }

    #[test]
    fn feedback_delay_reset_throttle() {
        // Generates 1, 2, 3, ... and looks back 5 periods via delay_with_reset.
        // Computes perf = source - delayed.  When |perf| > 3, feeds back a
        // signal that resets the delay, snapping the delayed value to current.
        //
        // Without feedback the perf would plateau at 5 (the lookback depth).
        // With feedback it follows a sawtooth: rises to 4 (level+1, because
        // the reset takes effect on the next engine cycle) then drops back.
        let period = Duration::from_nanos(100);
        let lookback = 5;
        let level: i64 = 3;

        let source = ticker(period).count().map(|x| x as i64);
        let (tx, rx) = feedback::<bool>();

        let delayed = source.delay_with_reset(period * lookback, rx.as_node());

        let perf = bimap(Dep::Active(source), Dep::Passive(delayed), |a, b| a - b);

        let collected = perf.collect();

        let writer = perf
            .filter_value(move |p| p.abs() > level)
            .map(|_| true)
            .feedback(tx);

        let mut graph = Graph::new(
            vec![collected.clone().as_node(), writer],
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(period * 14),
        );
        graph.run().unwrap();

        let values: Vec<i64> = collected.peek_value().iter().map(|v| v.value).collect();
        // Sawtooth: perf rises 0..4 then resets repeat.
        //  t=0:    src=1  delayed=1  perf=0
        //  t=100:  src=2  delayed=1  perf=1
        //  t=200:  src=3  delayed=1  perf=2
        //  t=300:  src=4  delayed=1  perf=3
        //  t=400:  src=5  delayed=1  perf=4  → reset, delayed snaps to 5
        //  t=500:  src=6  delayed=5  perf=1
        //  t=600:  src=7  delayed=5  perf=2
        //  t=700:  src=8  delayed=5  perf=3
        //  t=800:  src=9  delayed=5  perf=4  → reset, delayed snaps to 9
        //  t=900:  src=10 delayed=9  perf=1
        //  t=1000: src=11 delayed=9  perf=2
        //  t=1100: src=12 delayed=9  perf=3
        //  t=1200: src=13 delayed=9  perf=4  → reset, delayed snaps to 13
        //  t=1300: src=14 delayed=13 perf=1
        //  t=1400: src=15 delayed=13 perf=2
        //  t=1500: src=16 delayed=13 perf=3  (last cycle)
        assert_eq!(values, vec![0, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3]);
    }
}
