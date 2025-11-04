use derive_new::new;

use std::ops::Sub;
use std::rc::Rc;

use crate::types::*;

/// Emits the difference of it's source from one cycle to the
/// next.  Used by [difference](crate::nodes::StreamOperators::difference).
#[derive(new)]
pub(crate) struct DifferenceStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    #[new(default)]
    diff: T,
    #[new(default)]
    prev_val: Option<T>,
}

impl<T: Element + Sub<Output = T>> MutableNode for DifferenceStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        let ticked = match self.prev_val.clone() {
            Some(prv) => {
                self.diff = self.upstream.peek_value() - prv;
                true
            }
            None => false,
        };
        self.prev_val = Some(self.upstream.peek_value());
        ticked
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<T: Element + Sub<Output = T>> StreamPeekRef<T> for DifferenceStream<T> {
    fn peek_ref(&self) -> &T {
        &self.diff
    }
}
