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

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    #[test]
    fn inspect_observes_and_passes_through() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let seen_clone = seen.clone();
        let result = ticker(Duration::from_millis(1))
            .count()
            .inspect(move |v| seen_clone.borrow_mut().push(*v));
        result
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();
        assert_eq!(result.peek_value(), 3);
        assert_eq!(*seen.borrow(), vec![1, 2, 3]);
    }
}
