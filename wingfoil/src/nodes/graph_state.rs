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

#[node(active = [upstream], output = value: T)]
impl<T: Element> MutableNode for GraphStateStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = (self.func)(state);
        Ok(true)
    }
}
