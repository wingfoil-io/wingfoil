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

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;
    use crate::types::NanoTime;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn ticked_at_emits_graph_time() {
        let src: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        src.borrow_mut().push(ValueAt::new(0, NanoTime::new(100)));
        src.borrow_mut().push(ValueAt::new(0, NanoTime::new(250)));

        let times = src.clone().as_node().ticked_at().collect();
        times
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let vals: Vec<NanoTime> = times.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(vals, vec![NanoTime::new(100), NanoTime::new(250)]);
    }

    #[test]
    fn ticked_at_elapsed_emits_elapsed_time() {
        let src: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        src.borrow_mut().push(ValueAt::new(0, NanoTime::new(100)));
        src.borrow_mut().push(ValueAt::new(0, NanoTime::new(250)));

        let elapsed = src.clone().as_node().ticked_at_elapsed().collect();
        elapsed
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        // elapsed = time - start_time(0) => same as ticked_at
        let vals: Vec<NanoTime> = elapsed.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(vals, vec![NanoTime::new(100), NanoTime::new(250)]);
    }
}
