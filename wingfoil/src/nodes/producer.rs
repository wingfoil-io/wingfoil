use std::rc::Rc;
use crate::types::*;
use std::fmt::Debug;

pub(crate) struct ProducerStream<'a, T: Debug + Clone + 'a> {
    upstream: Rc<dyn Node<'a> + 'a>,
    value: T,
    func: Box<dyn Fn() -> T + 'a>,
}

impl<'a, T: Debug + Clone + Default + 'a> ProducerStream<'a, T> {
    pub fn new(upstream: Rc<dyn Node<'a> + 'a>, func: Box<dyn Fn() -> T + 'a>) -> Self {
        Self {
            upstream,
            value: T::default(),
            func,
        }
    }
}

impl<'a, T: Debug + Clone + 'a> MutableNode<'a> for ProducerStream<'a, T> {
    fn cycle(&mut self, _state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        self.value = (self.func)();
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(vec![self.upstream.clone()], vec![])
    }
}

impl<'a, T: Debug + Clone + 'a> StreamPeekRef<'a, T> for ProducerStream<'a, T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}
