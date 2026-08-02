use derive_new::new;

use std::boxed::Box;
use std::rc::Rc;

use crate::types::*;

/// When triggered by it's source, it produces values
/// using the supplied closure.
#[derive(new)]
pub(crate) struct ProducerStream<T: Element> {
    upstream: Rc<dyn Node>,
    func: Box<dyn Fn() -> T>,
    #[new(default)]
    value: T,
}

#[node(active = [upstream], output = value: T)]
impl<T: Element> MutableNode for ProducerStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = (self.func)();
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
    fn produce_emits_closure_return_value() {
        let src: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        src.borrow_mut().push(ValueAt::new(0, NanoTime::new(100)));
        src.borrow_mut().push(ValueAt::new(0, NanoTime::new(200)));

        let produced = src.clone().as_node().produce(|| 42u64).collect();
        produced
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let vals: Vec<u64> = produced.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(vals, vec![42, 42]);
    }

    #[test]
    fn produce_calls_closure_each_tick() {
        let src: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        src.borrow_mut().push(ValueAt::new(0, NanoTime::new(100)));
        src.borrow_mut().push(ValueAt::new(0, NanoTime::new(200)));
        src.borrow_mut().push(ValueAt::new(0, NanoTime::new(300)));

        let counter = Rc::new(RefCell::new(0u64));
        let counter2 = counter.clone();
        let produced = src.clone().as_node().produce(move || {
            let mut c = counter2.borrow_mut();
            *c += 1;
            *c
        });
        let collected = produced.collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let vals: Vec<u64> = collected.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(vals, vec![1, 2, 3]);
    }
}
