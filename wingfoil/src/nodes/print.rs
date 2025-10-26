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

impl<T: Element> MutableNode for PrintStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        self.value = self.upstream.peek_value();
        self.buffer.push(self.value.clone());
        true
    }
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<T: Element> StreamPeekRef<T> for PrintStream<T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}

impl<T: Element> Drop for PrintStream<T> {
    fn drop(&mut self) {
        for val in self.buffer.iter() {
            println!("{val:?}");
        }
    }
}
