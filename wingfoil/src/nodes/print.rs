use crate::types::*;

use std::ops::Drop;
use std::rc::Rc;

/// Propagates input and also pushes into buffer which is printed
/// on Drop.
pub struct PrintStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    buffer: Vec<T>,
    value: T,
}

impl<T: Element> PrintStream<T> {
    pub fn new(upstream: Rc<dyn Stream<T>>) -> PrintStream<T> {
        PrintStream {
            upstream,
            buffer: Vec::with_capacity(1000),
            value: T::default(),
        }
    }
}

#[node(active = [upstream], output = value: T)]
impl<T: Element> MutableNode for PrintStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = self.upstream.peek_value();
        self.buffer.push(self.value.clone());
        Ok(true)
    }
}

impl<T: Element> Drop for PrintStream<T> {
    fn drop(&mut self) {
        for val in self.buffer.iter() {
            println!("{val:?}");
        }
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
    fn print_passes_through_values() {
        let src: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        src.borrow_mut().push(ValueAt::new(1, NanoTime::new(100)));
        src.borrow_mut().push(ValueAt::new(2, NanoTime::new(200)));
        src.borrow_mut().push(ValueAt::new(3, NanoTime::new(300)));

        let collected = src.clone().as_stream().print().collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let vals: Vec<u64> = collected.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(vals, vec![1, 2, 3]);
    }
}
