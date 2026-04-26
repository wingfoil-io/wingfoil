use std::cmp::Eq;
use std::hash::Hash;

use crate::queue::{TimeQueue, ValueAt};
use crate::types::*;

use derive_new::new;

/// A queue of values that are emitted at specified times.  Useful for
/// unit tests.  Can also be used to feed stream output back into
/// the [Graph](crate::graph::Graph) as input on later cycles.
#[derive(new)]
pub struct CallBackStream<T: Element + Hash + Eq> {
    #[new(default)]
    value: T,
    #[new(default)]
    queue: TimeQueue<T>,
}

#[node(output = value: T)]
impl<T: Element + Hash + Eq> MutableNode for CallBackStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let mut ticked = false;
        while let Some(value) = self.queue.pop_if_pending(state.time()) {
            self.value = value;
            ticked = true;
        }
        if let Some(callback_time) = self.queue.next_time() {
            state.add_callback(callback_time);
        }
        Ok(ticked)
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        if let Some(time) = self.queue.next_time() {
            state.add_callback(time);
        }
        Ok(())
    }
}

impl<T: Element + Hash + Eq> CallBackStream<T> {
    pub fn push(&mut self, value_at: ValueAt<T>) {
        self.queue.push(value_at.value, value_at.time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::*;
    use crate::nodes::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn callback_stream_default_value_before_run() {
        let src: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        assert_eq!(src.peek_value(), 0u64);
    }

    #[test]
    fn callback_stream_emits_pushed_values_in_order() {
        let src: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        src.borrow_mut().push(ValueAt::new(10, NanoTime::new(100)));
        src.borrow_mut().push(ValueAt::new(20, NanoTime::new(200)));
        src.borrow_mut().push(ValueAt::new(30, NanoTime::new(300)));
        let collected = src.clone().as_stream().collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let vals: Vec<u64> = collected.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(vals, vec![10, 20, 30]);
    }

    #[test]
    fn callback_stream_emits_correct_timestamps() {
        let src: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        src.borrow_mut().push(ValueAt::new(1, NanoTime::new(50)));
        src.borrow_mut().push(ValueAt::new(2, NanoTime::new(150)));
        let collected = src.clone().as_stream().collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let times: Vec<NanoTime> = collected.peek_value().iter().map(|v| v.time).collect();
        assert_eq!(times, vec![NanoTime::new(50), NanoTime::new(150)]);
    }
}
