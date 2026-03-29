use crate::types::*;
use derive_new::new;
use std::rc::Rc;

#[derive(new)]
pub struct InitiallyStream<T: Element, F: FnOnce(&GraphState) -> anyhow::Result<()>> {
    source: Rc<dyn Stream<T>>,
    initially: Option<F>,
    #[new(default)]
    value: T,
}

impl<T: Element, F: FnOnce(&GraphState) -> anyhow::Result<()>> MutableNode
    for InitiallyStream<T, F>
{
    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        if let Some(f) = self.initially.take() {
            f(state)?;
        }
        Ok(())
    }
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = self.source.peek_value();
        Ok(true)
    }
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.source.clone().as_node()], vec![])
    }
}

impl<T: Element, F: FnOnce(&GraphState) -> anyhow::Result<()>> StreamPeekRef<T>
    for InitiallyStream<T, F>
{
    fn peek_ref(&self) -> &T {
        &self.value
    }
}
