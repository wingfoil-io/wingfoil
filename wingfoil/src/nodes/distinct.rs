use crate::types::*;
use derive_new::new;
use std::rc::Rc;

/// Only propagates it's source when it's value changes.  Used
/// by [distinct](crate::nodes::StreamOperators::distinct).
#[derive(new, StreamPeekRef)]
pub(crate) struct DistinctStream<T: Element> {
    source: Rc<dyn Stream<T>>, // the source stream
    #[new(default)] // used by derive_new
    #[output]
    value: T,
}

impl<T: Element + PartialEq> MutableNode for DistinctStream<T> {
    // called by Graph when it determines this node needs
    // to be cycled
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let curr = self.source.peek_value();
        if self.value == curr {
            // value did not change, do not tick
            Ok(false)
        } else {
            // value changed, tick
            self.value = curr;
            Ok(true)
        }
    }

    // called by Graph at wiring (initialisation) time
    fn upstreams(&self) -> UpStreams {
        // this node is driven only by its source
        UpStreams::new(vec![self.source.clone().as_node()], vec![])
    }
}
