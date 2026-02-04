use crate::types::*;

use std::ops::Drop;
use std::rc::Rc;
use std::fmt::Debug;

/// Propagates input and also pushes into buffer which is printed
/// on Drop.
pub struct PrintStream<'a, T: Debug + Clone + 'a> {
    upstream: Rc<dyn Stream<'a, T> + 'a>,
    buffer: Vec<T>,
    value: T,
}

impl<'a, T: Debug + Clone + Default + 'a> PrintStream<'a, T> {
    pub fn new(upstream: Rc<dyn Stream<'a, T> + 'a>) -> PrintStream<'a, T> {
        PrintStream {
            upstream,
            buffer: Vec::with_capacity(1000),
            value: T::default(),
        }
    }
}

impl<'a, T: Debug + Clone + 'a> MutableNode<'a> for PrintStream<'a, T> {
    fn cycle(&mut self, _state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        self.value = self.upstream.peek_value();
        self.buffer.push(self.value.clone());
        Ok(true)
    }
    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<'a, T: Debug + Clone + 'a> StreamPeekRef<'a, T> for PrintStream<'a, T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}

impl<'a, T: Debug + Clone + 'a> Drop for PrintStream<'a, T> {
    fn drop(&mut self) {
        for val in self.buffer.iter() {
            println!("{:?}", val);
        }
    }
}
