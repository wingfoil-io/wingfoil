use derive_new::new;

use std::rc::Rc;

use crate::types::*;

#[derive(new, StreamPeekRef, WiringPoint)]
pub(crate) struct GraphStateStream<T: Element> {
    #[active]
    upstream: Rc<dyn Node>,
    #[new(default)]
    #[output]
    value: T,
    func: Box<dyn Fn(&mut GraphState) -> T>,
}

impl<T: Element> MutableNode for GraphStateStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = (self.func)(state);
        Ok(true)
    }
}
