use crate::types::*;

use std::ops::Drop;
use std::rc::Rc;

/// Propagates input and also pushes into buffer which is printed
/// on Drop.
#[derive(StreamPeekRef, WiringPoint)]
pub struct PrintStream<T: Element> {
    #[active]
    upstream: Rc<dyn Stream<T>>,
    buffer: Vec<T>,
    #[output]
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
