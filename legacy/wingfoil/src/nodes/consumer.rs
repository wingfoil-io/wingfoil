use derive_new::new;

use std::boxed::Box;
use std::rc::Rc;

use crate::types::*;

/// Applies function to it's source.  It is a [Node] - it
/// doesn't produce anything.  Used by [consume](crate::nodes::StreamOperators::for_each).
#[derive(new)]
pub(crate) struct ConsumerNode<IN> {
    upstream: Rc<dyn Stream<IN>>,
    func: Box<dyn Fn(IN, NanoTime)>,
}

#[node(active = [upstream])]
impl<IN> MutableNode for ConsumerNode<IN> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        (self.func)(self.upstream.peek_value(), state.time());
        Ok(true)
    }
}

/// Like [ConsumerNode] but accepts a fallible closure.
/// Errors propagate to graph execution.
#[derive(new)]
pub(crate) struct TryConsumerNode<IN> {
    upstream: Rc<dyn Stream<IN>>,
    func: Box<dyn Fn(IN, NanoTime) -> anyhow::Result<()>>,
}

#[node(active = [upstream])]
impl<IN> MutableNode for TryConsumerNode<IN> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        (self.func)(self.upstream.peek_value(), state.time())?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;
    use crate::types::NanoTime;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn for_each_called_once_per_tick() {
        let src: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        src.borrow_mut().push(ValueAt::new(10, NanoTime::new(100)));
        src.borrow_mut().push(ValueAt::new(20, NanoTime::new(200)));

        let seen = Rc::new(RefCell::new(vec![]));
        let seen2 = seen.clone();
        let consumer = src
            .clone()
            .as_stream()
            .for_each(move |v, _t| seen2.borrow_mut().push(v));
        consumer
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        assert_eq!(*seen.borrow(), vec![10u64, 20]);
    }

    #[test]
    fn try_for_each_success_path() {
        let src: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        src.borrow_mut().push(ValueAt::new(5, NanoTime::new(100)));

        let seen = Rc::new(RefCell::new(vec![]));
        let seen2 = seen.clone();
        let consumer = src.clone().as_stream().try_for_each(move |v, _t| {
            seen2.borrow_mut().push(v);
            Ok(())
        });
        consumer
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        assert_eq!(*seen.borrow(), vec![5u64]);
    }

    #[test]
    fn try_for_each_error_propagates() {
        let src: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        src.borrow_mut().push(ValueAt::new(1, NanoTime::new(100)));

        let consumer = src
            .clone()
            .as_stream()
            .try_for_each(|_v, _t| Err(anyhow::anyhow!("expected error")));
        let result = consumer.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
        assert!(result.is_err());
    }
}
