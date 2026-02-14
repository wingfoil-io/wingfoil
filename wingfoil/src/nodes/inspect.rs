use crate::types::*;

use std::rc::Rc;

/// Passes through upstream values unchanged while calling a user-supplied
/// closure on each value for side effects (debugging, logging, etc.).
pub struct InspectStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    value: T,
    func: Box<dyn Fn(&T)>,
}

impl<T: Element> InspectStream<T> {
    pub fn new(upstream: Rc<dyn Stream<T>>, func: Box<dyn Fn(&T)>) -> InspectStream<T> {
        InspectStream {
            upstream,
            value: T::default(),
            func,
        }
    }
}

impl<T: Element> MutableNode for InspectStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = self.upstream.peek_value();
        (self.func)(&self.value);
        Ok(true)
    }
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<T: Element> StreamPeekRef<T> for InspectStream<T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}
