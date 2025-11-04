use crate::graph::GraphState;
use crate::types::{IntoNode, MutableNode, Node};
use std::rc::Rc;

struct AlwaysTickNode {}

impl MutableNode for AlwaysTickNode {
    fn cycle(&mut self, _: &mut GraphState) -> bool {
        true
    }

    fn start(&mut self, state: &mut GraphState) {
        state.always_callback();
    }
}

/// Produces a [Node] that ticks on every engine cycle.
pub fn always() -> Rc<dyn Node> {
    AlwaysTickNode {}.into_node()
}

#[cfg(test)]
mod tests {

    use super::always;
    use crate::*;
    use std::time::Duration;

    #[test]
    fn test_always_tick() {
        // cargo flamegraph --open  --unit-test -- test_always_tick -- --no-capture
        // cargo test --lib --release test_always_tick -- --no-capture
        let duration = Duration::from_secs(1);
        let run_for = RunFor::Duration(duration);
        let count = always().count();
        count.run(RunMode::RealTime, run_for).unwrap();
        let count = count.peek_value() as u32;
        let avg_period = duration / count;
        println!("{count} cycles.  {avg_period:?} per cycle");
    }
}
