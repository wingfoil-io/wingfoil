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
/// let writer = sum.for_each_with_state(move |val, state| {
///     tx.send(val, state);
/// });
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

        let writer = src.for_each_with_state(move |val, state| {
            tx.send(val, state);
        });

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

        let writer = sum.for_each_with_state(move |val, state| {
            tx.send(val, state);
        });

        let mut graph = Graph::new(
            vec![collected.clone().as_node(), writer],
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(Duration::from_nanos(300)),
        );
        graph.run().unwrap();

        let values: Vec<u64> = collected.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(values, vec![1, 3, 6, 10]);
    }
}
