use crate::graph::GraphState;
use crate::types::{IntoNode, MutableNode, Node};
use std::rc::Rc;

struct NeverTickNode {}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::*;
    use crate::nodes::*;
    use crate::types::NanoTime;
    use std::cell::RefCell;

    #[test]
    fn never_does_not_trigger_downstream() {
        let counter = Rc::new(RefCell::new(0u32));
        let counter2 = counter.clone();
        let consumer = never().produce(|| 1u32).for_each(move |_, _| {
            *counter2.borrow_mut() += 1;
        });
        consumer
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert_eq!(*counter.borrow(), 0);
    }
}
