use crate::graph::GraphState;
use crate::types::{IntoNode, MutableNode, Node, UpStreams, WiringPoint};
use std::rc::Rc;

struct NeverTickNode {}

impl WiringPoint for NeverTickNode {
    fn upstreams(&self) -> UpStreams {
        UpStreams::none()
    }
}

impl MutableNode for NeverTickNode {
    fn cycle(&mut self, _: &mut GraphState) -> anyhow::Result<bool> {
        Ok(false)
    }
}

/// Produces a [Node] that never ticks.
#[must_use]
pub fn never() -> Rc<dyn Node> {
    NeverTickNode {}.into_node()
}
