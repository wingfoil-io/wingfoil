use derive_new::new;

use std::boxed::Box;
use std::rc::Rc;

use crate::types::*;

/// Applies function to it's source.  It is a [Node] - it
/// doesn't produce anything.  Used by [consume](crate::nodes::StreamOperators::for_each).
#[derive(new)]
pub(crate) struct ConsumerNode<'a, IN> {
    upstream: Rc<dyn Stream<'a, IN> + 'a>,
    func: Box<dyn Fn(IN, NanoTime) + 'a>,
}

impl<'a, IN: Clone> MutableNode<'a> for ConsumerNode<'a, IN> {
    fn cycle(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        (self.func)(self.upstream.peek_value(), state.time());
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}
