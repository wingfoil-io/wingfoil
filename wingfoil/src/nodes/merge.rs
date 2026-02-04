use std::rc::Rc;
use crate::types::*;
use std::fmt::Debug;

pub struct MergeStream<'a, T: Debug + Clone + 'a> {
    sources: Vec<Rc<dyn Stream<'a, T> + 'a>>,
    value: T,
}

impl<'a, T: Debug + Clone + Default + 'a> MergeStream<'a, T> {
    pub fn new(sources: Vec<Rc<dyn Stream<'a, T> + 'a>>) -> Self {
        Self {
            sources,
            value: T::default(),
        }
    }
}

impl<'a, T: Debug + Clone + 'a> MutableNode<'a> for MergeStream<'a, T> {
    fn cycle(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        for source in &self.sources {
            if state.ticked(source.clone().as_node()) {
                self.value = source.peek_value();
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn upstreams(&self) -> UpStreams<'a> {
        let active = self.sources.iter().map(|s| s.clone().as_node()).collect();
        UpStreams::new(active, vec![])
    }
}

impl<'a, T: Debug + Clone + 'a> StreamPeekRef<'a, T> for MergeStream<'a, T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}
