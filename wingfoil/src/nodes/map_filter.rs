use std::boxed::Box;
use std::rc::Rc;

use crate::types::*;
use std::fmt::Debug;

/// Map's it's source into a new [Stream] using the supplied closure.
/// Used by [map](crate::nodes::StreamOperators::map).
pub struct MapFilterStream<'a, IN, OUT> {
    upstream: Rc<dyn Stream<'a, IN> + 'a>,
    value: OUT,
    func: Box<dyn Fn(IN) -> (OUT, bool) + 'a>,
}

impl<'a, IN, OUT: Debug + Clone + Default + 'a> MapFilterStream<'a, IN, OUT> {
    pub fn new(upstream: Rc<dyn Stream<'a, IN> + 'a>, func: Box<dyn Fn(IN) -> (OUT, bool) + 'a>) -> Self {
        Self {
            upstream,
            value: OUT::default(),
            func,
        }
    }
}

impl<'a, IN: Clone, OUT: Debug + Clone + 'a> MutableNode<'a> for MapFilterStream<'a, IN, OUT> {
    fn cycle(&mut self, _state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        let (val, ticked) = (self.func)(self.upstream.peek_value());
        if ticked {
            self.value = val;
        }
        Ok(ticked)
    }

    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<'a, IN: Clone, OUT: Debug + Clone + 'a> StreamPeekRef<'a, OUT> for MapFilterStream<'a, IN, OUT> {
    fn peek_ref(&self) -> &OUT {
        &self.value
    }
}
