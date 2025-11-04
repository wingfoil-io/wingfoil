use crate::types::*;
use derive_new::new;
use std::rc::Rc;

#[derive(new)]
pub struct LimitStream<T: Element> {
    source: Rc<dyn Stream<T>>,
    limit: u32,
    #[new(default)]
    tick_count: u32,
    #[new(default)]
    value: T,
}

impl<T: Element> MutableNode for LimitStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        if self.tick_count >= self.limit {
            false
        } else {
            self.tick_count += 1;
            self.value = self.source.peek_value();
            true
        }
    }
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.source.clone().as_node()], vec![])
    }
}

impl<T: Element> StreamPeekRef<T> for LimitStream<T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}
