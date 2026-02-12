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
    use crate::Dep::*;
    use std::time::Duration;


    #[test]
    fn feedback_works() {
        // Example of circuit breaker observing to spot change
        // relative to delayed valued. 
        // If trigger fires, the lookback resets to avoid 
        // excessively repeating, whilst still enforcing the control
        // 
        let period = Duration::from_nanos(100);
        let lookback = 5;
        let level: i64 = 3;

        let source = ticker(period).count();
        let (tx, rx) = feedback::<bool>();

        let delayed = source.delay_with_reset(period * lookback, rx.as_node());

        let diff = bimap(
            Active(source), 
            Passive(delayed), 
            |a, b| a as i64 - b as i64
        );

        let trigger = diff
            .filter_value(move |p| p.abs() > level)
            .map(|_| true)
            .feedback(tx);

        let res = diff.accumulate().finally(|value, _| {
            let expected = vec![0, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3];
            assert_eq!(expected, value);
            Ok(())
        });

        Graph::new(
            vec![trigger, res], 
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(period * 14),
        )
        .run()
        .unwrap();

    }
}
