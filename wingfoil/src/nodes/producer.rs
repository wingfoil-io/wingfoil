use derive_new::new;

use std::boxed::Box;
use std::rc::Rc;

use crate::types::*;

/// When triggered by it's source, it produces values
/// using the supplied closure.
#[derive(new)]
pub(crate) struct ProducerStream<T: Element> {
    upstream: Rc<dyn Node>,
    func: Box<dyn Fn() -> T>,
    #[new(default)]
    value: T,
}

#[node(active = [upstream], output = value: T)]
impl<T: Element> MutableNode for ProducerStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = (self.func)();
        Ok(true)
    }
}
