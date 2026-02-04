use std::ops::Sub;
use std::rc::Rc;

use crate::types::*;
use std::fmt::Debug;

/// Emits the difference of it's source from one cycle to the
/// next.  Used by [difference](crate::nodes::StreamOperators::difference).
pub(crate) struct DifferenceStream<'a, T: Debug + Clone + 'a> {
    upstream: Rc<dyn Stream<'a, T> + 'a>,
    diff: T,
    prev_val: Option<T>,
}

impl<'a, T: Debug + Clone + Default + 'a> DifferenceStream<'a, T> {
    pub fn new(upstream: Rc<dyn Stream<'a, T> + 'a>) -> Self {
        Self {
            upstream,
            diff: T::default(),
            prev_val: None,
        }
    }
}

impl<'a, T: Debug + Clone + 'a + Sub<Output = T>> MutableNode<'a> for DifferenceStream<'a, T> {
    fn cycle(&mut self, _state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        let ticked = match self.prev_val.clone() {
            Some(prv) => {
                self.diff = self.upstream.peek_value() - prv;
                true
            }
            None => false,
        };
        self.prev_val = Some(self.upstream.peek_value());
        Ok(ticked)
    }

    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<'a, T: Debug + Clone + 'a + Sub<Output = T>> StreamPeekRef<'a, T> for DifferenceStream<'a, T> {
    fn peek_ref(&self) -> &T {
        &self.diff
    }
}