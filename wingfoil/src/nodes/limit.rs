use crate::types::*;
use std::rc::Rc;
use std::fmt::Debug;

pub struct LimitStream<'a, T: Debug + Clone + 'a> {
    source: Rc<dyn Stream<'a, T> + 'a>,
    limit: u32,
    tick_count: u32,
    value: T,
}

impl<'a, T: Debug + Clone + Default + 'a> LimitStream<'a, T> {
    pub fn new(source: Rc<dyn Stream<'a, T> + 'a>, limit: u32) -> Self {
        Self {
            source,
            limit,
            tick_count: 0,
            value: T::default(),
        }
    }
}

impl<'a, T: Debug + Clone + 'a> MutableNode<'a> for LimitStream<'a, T> {
    fn cycle(&mut self, _state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        if self.tick_count >= self.limit {
            Ok(false)
        } else {
            self.tick_count += 1;
            self.value = self.source.peek_value();
            Ok(true)
        }
    }
    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(vec![self.source.clone().as_node()], vec![])
    }
}

impl<'a, T: Debug + Clone + 'a> StreamPeekRef<'a, T> for LimitStream<'a, T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}