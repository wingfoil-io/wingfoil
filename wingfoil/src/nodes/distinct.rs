use crate::types::*;
use derive_new::new;
use std::rc::Rc;

/// Only propagates it's source when it's value changes.  Used
/// by [distinct](crate::nodes::StreamOperators::distinct).
#[derive(new)]
pub(crate) struct DistinctStream<T: Element> {
    source: Rc<dyn Stream<T>>, // the source stream
    #[new(default)] // used by derive_new
    value: T,
}

impl<T: Element + PartialEq> MutableNode for DistinctStream<T> {
    // called by Graph when it determines this node needs
    // to be cycled
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        let curr = self.source.peek_value();
        if self.value == curr {
            // value did not change, do not tick
            false
        } else {
            // value changed, tick
            self.value = curr;
            true
        }
    }

    // called by Graph at wiring (initialisation) time
    fn upstreams(&self) -> UpStreams {
        // this node is driven only by its source
        UpStreams::new(vec![self.source.clone().as_node()], vec![])
    }
}

// downstream nodes can inspect the current value of this
// stream by calling this method
impl<T: Element + PartialEq> StreamPeekRef<T> for DistinctStream<T> {
    fn peek_ref(&self) -> &T {
        // for large structs, please wrap in an Rc
        // to get shallow copy semantics
        &self.value
    }
}
