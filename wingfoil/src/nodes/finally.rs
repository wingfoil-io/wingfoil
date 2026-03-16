use crate::types::*;
use derive_new::new;
use std::rc::Rc;

#[derive(new, WiringPoint)]
pub struct FinallyNode<T: Element, F: FnOnce(T, &GraphState) -> anyhow::Result<()>> {
    #[active]
    source: Rc<dyn Stream<T>>,
    finally: Option<F>,
    #[new(default)]
    value: T,
}

impl<T: Element, F: FnOnce(T, &GraphState) -> anyhow::Result<()>> MutableNode
    for FinallyNode<T, F>
{
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = self.source.peek_value();
        Ok(true)
    }
    fn stop(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        if let Some(f) = self.finally.take() {
            f(self.value.clone(), state)?;
        }
        Ok(())
    }
}
