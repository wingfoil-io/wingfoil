use derive_new::new;

use std::rc::Rc;

use crate::types::*;

#[derive(new)]
pub(crate) struct GraphStateStream<T: Element> {
    upstream: Rc<dyn Node>,
    #[new(default)]
    value: T,
    func: Box<dyn Fn(&mut GraphState) -> T>,
}

impl<T: Element> MutableNode for GraphStateStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> bool {
        self.value = (self.func)(state);
        true
    }

    fn upstreams(&self) -> UpStreams {
        // only ticks on trigger
        UpStreams::new(vec![self.upstream.clone()], vec![])
    }
}

impl<T: Element> StreamPeekRef<T> for GraphStateStream<T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}
