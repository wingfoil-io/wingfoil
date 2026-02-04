use std::rc::Rc;
use crate::types::*;
use std::fmt::Debug;

pub(crate) struct FilterStream<'a, T: Debug + Clone + 'a> {
    upstream: Rc<dyn Stream<'a, T> + 'a>,
    condition: Rc<dyn Stream<'a, bool> + 'a>,
    value: T,
}

impl<'a, T: Debug + Clone + Default + 'a> FilterStream<'a, T> {
    pub fn new(upstream: Rc<dyn Stream<'a, T> + 'a>, condition: Rc<dyn Stream<'a, bool> + 'a>) -> Self {
        Self {
            upstream,
            condition,
            value: T::default(),
        }
    }
}

impl<'a, T: Debug + Clone + 'a> MutableNode<'a> for FilterStream<'a, T> {
    fn cycle(&mut self, _state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        if self.condition.peek_value() {
            self.value = self.upstream.peek_value();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(
            vec![self.upstream.clone().as_node(), self.condition.clone().as_node()],
            vec![],
        )
    }
}

impl<'a, T: Debug + Clone + 'a> StreamPeekRef<'a, T> for FilterStream<'a, T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}
