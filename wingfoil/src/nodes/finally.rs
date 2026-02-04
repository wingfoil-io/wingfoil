use crate::types::*;
use std::rc::Rc;
use std::fmt::Debug;

pub struct FinallyNode<'a, T: Debug + Clone + 'a, F: FnOnce(T, &GraphState<'a>)> {
    source: Rc<dyn Stream<'a, T> + 'a>,
    finally: Option<F>,
    value: T,
}

impl<'a, T: Debug + Clone + Default + 'a, F: FnOnce(T, &GraphState<'a>) + 'a> FinallyNode<'a, T, F> {
    pub fn new(source: Rc<dyn Stream<'a, T> + 'a>, finally: Option<F>) -> Self {
        Self {
            source,
            finally,
            value: T::default(),
        }
    }
}

impl<'a, T: Debug + Clone + 'a, F: FnOnce(T, &GraphState<'a>) + 'a> MutableNode<'a> for FinallyNode<'a, T, F> {
    fn cycle(&mut self, _state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        self.value = self.source.peek_value();
        Ok(true)
    }
    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(vec![self.source.clone().as_node()], vec![])
    }
    fn stop(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        if let Some(f) = self.finally.take() {
            f(self.value.clone(), state);
        }
        Ok(())
    }
}