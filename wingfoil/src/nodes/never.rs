use crate::graph::GraphState;
use crate::types::{IntoNode, MutableNode, Node, UpStreams};
use std::rc::Rc;

struct NeverTickNode {}

impl MutableNode for NeverTickNode {
    fn cycle(&mut self, _: &mut GraphState) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::default()
    }
}

/// Produces a [Node] that never ticks.
pub fn never() -> Rc<dyn Node> {
    NeverTickNode {}.into_node()
}
