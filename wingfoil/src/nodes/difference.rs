use derive_new::new;

use std::ops::Sub;
use std::rc::Rc;

use crate::types::*;

/// Emits the difference of it's source from one cycle to the
/// next.  Used by [difference](crate::nodes::StreamOperators::difference).
#[derive(new, StreamPeekRef, WiringPoint)]
pub(crate) struct DifferenceStream<T: Element> {
    #[active]
    upstream: Rc<dyn Stream<T>>,
    #[new(default)]
    #[output]
    diff: T,
    #[new(default)]
    prev_val: Option<T>,
}

impl<T: Element + Sub<Output = T>> MutableNode for DifferenceStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
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
}
