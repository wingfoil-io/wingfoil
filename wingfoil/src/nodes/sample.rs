use std::rc::Rc;
use crate::types::*;
use std::fmt::Debug;

pub struct SampleStream<'a, T: Debug + Clone + 'a> {
    upstream: Rc<dyn Stream<'a, T> + 'a>,
    trigger: Rc<dyn Node<'a> + 'a>,
    value: T,
}

impl<'a, T: Debug + Clone + Default + 'a> SampleStream<'a, T> {
    pub fn new(upstream: Rc<dyn Stream<'a, T> + 'a>, trigger: Rc<dyn Node<'a> + 'a>) -> Self {
        Self {
            upstream,
            trigger,
            value: T::default(),
        }
    }
}

impl<'a, T: Debug + Clone + 'a> MutableNode<'a> for SampleStream<'a, T> {
    fn cycle(&mut self, _state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        self.value = self.upstream.peek_value();
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(vec![self.trigger.clone()], vec![self.upstream.clone().as_node()])
    }
}

impl<'a, T: Debug + Clone + 'a> StreamPeekRef<'a, T> for SampleStream<'a, T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}
