use crate::types::*;
use derive_new::new;
use std::rc::Rc;

#[derive(new)]
pub struct FinallyNode<T: Element, F: FnOnce(T, &GraphState) + Clone> {
    source: Rc<dyn Stream<T>>,
    finally: F,
    #[new(default)]
    value: T,
}

impl<T: Element, F: FnOnce(T, &GraphState) + Clone> MutableNode for FinallyNode<T, F> {
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        self.value = self.source.peek_value();
        true
    }
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.source.clone().as_node()], vec![])
    }
    fn stop(&mut self, state: &mut GraphState) {
        (self.finally.clone())(self.value.clone(), state)
    }
}
