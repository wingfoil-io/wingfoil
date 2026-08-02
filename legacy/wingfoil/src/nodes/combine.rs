use crate::types::*;
use derive_new::new;

use std::cell::RefCell;
use std::rc::Rc;

#[derive(new)]
struct CombineNode<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    combined: Rc<RefCell<Burst<T>>>,
}

#[node(active = [upstream])]
impl<T: Element> MutableNode for CombineNode<T> {
    fn cycle(&mut self, _: &mut GraphState) -> anyhow::Result<bool> {
        self.combined.borrow_mut().push(self.upstream.peek_value());
        Ok(true)
    }
}

#[derive(new)]
struct CombineStream2<T: Element> {
    upstreams: Vec<Rc<dyn Node>>,
    combined: Rc<RefCell<Burst<T>>>,
    #[new(default)]
    value: Burst<T>,
}

#[node(active = [upstreams], output = value: Burst<T>)]
impl<T: Element> MutableNode for CombineStream2<T> {
    fn cycle(&mut self, _: &mut GraphState) -> anyhow::Result<bool> {
        self.value = std::mem::replace(&mut *self.combined.borrow_mut(), Burst::new());
        Ok(true)
    }
}

#[must_use]
pub fn combine<T: Element>(streams: Vec<Rc<dyn Stream<T>>>) -> Rc<dyn Stream<Burst<T>>> {
    let combined = Rc::new(RefCell::new(Burst::new()));
    let nodes = streams
        .iter()
        .map(|strm| CombineNode::new(strm.clone(), combined.clone()).into_node())
        .collect::<Vec<_>>();
    CombineStream2::new(nodes, combined).into_stream()
}

#[cfg(test)]
mod tests {
    use crate::{
        NanoTime, NodeOperators, RunFor, RunMode, StreamOperators, burst, combine, ticker,
    };
    use std::time::Duration;
    #[test]
    fn combine_works() {
        let _ = env_logger::try_init();
        let period = Duration::from_micros(1);
        let run_mode = RunMode::HistoricalFrom(NanoTime::ZERO);
        let run_for = RunFor::Cycles(3);
        let src = ticker(period).count();
        let streams = (0..3)
            .map(|i| src.map(move |x| x * 10_u64.pow(i)))
            .collect::<Vec<_>>();
        combine(streams)
            .logged("output", log::Level::Info)
            .accumulate()
            .finally(|res, _| {
                let expected = vec![burst![1, 10, 100], burst![2, 20, 200], burst![3, 30, 300]];
                println!("{res:?}");
                assert_eq!(res, expected);
                Ok(())
            })
            .run(run_mode, run_for)
            .unwrap();
    }
}
