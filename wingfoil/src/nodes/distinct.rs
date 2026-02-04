use std::rc::Rc;
use crate::types::*;
use std::fmt::Debug;

pub(crate) struct DistinctStream<'a, T: Debug + Clone + 'a> {
    upstream: Rc<dyn Stream<'a, T> + 'a>,
    value: T,
}

impl<'a, T: Debug + Clone + 'a + PartialEq + Default> DistinctStream<'a, T> {
    pub fn new(upstream: Rc<dyn Stream<'a, T> + 'a>) -> Self {
        Self {
            upstream,
            value: T::default(),
        }
    }
}

impl<'a, T: Debug + Clone + 'a + PartialEq> MutableNode<'a> for DistinctStream<'a, T> {
    fn cycle(&mut self, _state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        let next_value = self.upstream.peek_value();
        if next_value != self.value {
            self.value = next_value;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<'a, T: Debug + Clone + 'a + PartialEq> StreamPeekRef<'a, T> for DistinctStream<'a, T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}
