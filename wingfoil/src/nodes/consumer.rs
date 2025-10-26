use derive_new::new;

use std::boxed::Box;
use std::rc::Rc;

use crate::types::*;

/// Applies function to it's source.  It is a [Node] - it
/// doesn't produce anything.  Used by [consume](crate::nodes::StreamOperators::for_each).
#[derive(new)]
pub(crate) struct ConsumerNode<IN> {
    upstream: Rc<dyn Stream<IN>>,
    func: Box<dyn Fn(IN, NanoTime)>,
}

impl<IN> MutableNode for ConsumerNode<IN> {
    fn cycle(&mut self, state: &mut GraphState) -> bool {
        (self.func)(self.upstream.peek_value(), state.time());
        true
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}
