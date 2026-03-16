use derive_new::new;

use std::boxed::Box;
use std::rc::Rc;

use crate::types::*;

/// Applies function to it's source.  It is a [Node] - it
/// doesn't produce anything.  Used by [consume](crate::nodes::StreamOperators::for_each).
#[derive(new, Upstreams)]
pub(crate) struct ConsumerNode<IN> {
    #[active]
    upstream: Rc<dyn Stream<IN>>,
    func: Box<dyn Fn(IN, NanoTime)>,
}

impl<IN> MutableNode for ConsumerNode<IN> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        (self.func)(self.upstream.peek_value(), state.time());
        Ok(true)
    }
}

/// Like [ConsumerNode] but accepts a fallible closure.
/// Errors propagate to graph execution.
#[derive(new, Upstreams)]
pub(crate) struct TryConsumerNode<IN> {
    #[active]
    upstream: Rc<dyn Stream<IN>>,
    func: Box<dyn Fn(IN, NanoTime) -> anyhow::Result<()>>,
}

impl<IN> MutableNode for TryConsumerNode<IN> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        (self.func)(self.upstream.peek_value(), state.time())?;
        Ok(true)
    }
}
