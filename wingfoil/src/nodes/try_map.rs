use std::boxed::Box;
use std::rc::Rc;

use crate::types::*;

/// Maps source into a new Stream using a fallible closure.
/// Used by [try_map](crate::nodes::StreamOperators::try_map).
pub struct TryMapStream<IN, OUT: Element> {
    upstream: Rc<dyn Stream<IN>>,
    value: OUT,
    func: Box<dyn Fn(IN) -> anyhow::Result<OUT>>,
}

impl<IN, OUT: Element> TryMapStream<IN, OUT> {
    pub fn new(upstream: Rc<dyn Stream<IN>>, func: Box<dyn Fn(IN) -> anyhow::Result<OUT>>) -> Self {
        Self {
            upstream,
            value: OUT::default(),
            func,
        }
    }
}

impl<IN, OUT: Element> MutableNode for TryMapStream<IN, OUT> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = (self.func)(self.upstream.peek_value())?;
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<IN: 'static, OUT: Element> StreamPeekRef<OUT> for TryMapStream<IN, OUT> {
    fn peek_ref(&self) -> &OUT {
        &self.value
    }
}
