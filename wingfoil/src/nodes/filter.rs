use derive_new::new;

use std::rc::Rc;

use crate::types::*;

/// Filter's it source based on the supplied predicate.  Used by
/// [filter](crate::nodes::StreamOperators::filter).
#[derive(new)]
pub(crate) struct FilterStream<T: Element> {
    source: Rc<dyn Stream<T>>,
    condition: Rc<dyn Stream<bool>>,
    #[new(default)]
    value: T,
}

impl<T: Element> MutableNode for FilterStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        let val = self.source.peek_value();
        let ticked = self.condition.peek_value();
        if ticked {
            self.value = val;
        }
        ticked
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(
            vec![self.source.clone().as_node(), self.condition.clone().as_node()],
            vec![],
        )
    }
}

impl<T: Element> StreamPeekRef<T> for FilterStream<T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}
