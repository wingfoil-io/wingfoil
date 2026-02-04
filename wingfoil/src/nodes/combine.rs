use crate::types::*;
use derive_new::new;

use std::cell::RefCell;
use std::rc::Rc;
use tinyvec::TinyVec;
use std::fmt::Debug;

#[derive(new)]
struct CombineNode<'a, T: Debug + Clone + Default + 'a> {
    upstream: Rc<dyn Stream<'a, T> + 'a>,
    combined: Rc<RefCell<TinyVec<[T; 1]>>>,
}

impl<'a, T: Debug + Clone + Default + 'a> MutableNode<'a> for CombineNode<'a, T> {
    fn cycle(&mut self, _: &mut GraphState<'a>) -> anyhow::Result<bool> {
        self.combined.borrow_mut().push(self.upstream.peek_value());
        Ok(true)
    }
    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

#[derive(new)]
struct CombineStream2<'a, T: Debug + Clone + Default + 'a> {
    upstreams: Vec<Rc<dyn Node<'a> + 'a>>,
    combined: Rc<RefCell<TinyVec<[T; 1]>>>,
    #[new(default)]
    value: TinyVec<[T; 1]>,
}

impl<'a, T: Debug + Clone + Default + 'a> MutableNode<'a> for CombineStream2<'a, T> {
    fn cycle(&mut self, _: &mut GraphState<'a>) -> anyhow::Result<bool> {
        self.value = std::mem::replace(&mut *self.combined.borrow_mut(), TinyVec::new());
        Ok(true)
    }
    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(self.upstreams.clone(), vec![])
    }
}

impl<'a, T: Debug + Clone + Default + 'a> StreamPeekRef<'a, TinyVec<[T; 1]>> for CombineStream2<'a, T> {
    fn peek_ref(&self) -> &TinyVec<[T; 1]> {
        &self.value
    }
}

pub fn combine<'a, T: Debug + Clone + Default + 'a>(streams: Vec<Rc<dyn Stream<'a, T> + 'a>>) -> Rc<dyn Stream<'a, TinyVec<[T; 1]>> + 'a> {
    let combined = Rc::new(RefCell::new(TinyVec::new()));
    let nodes = streams
        .iter()
        .map(|strm| CombineNode::new(strm.clone(), combined.clone()).into_node())
        .collect::<Vec<_>>();
    CombineStream2::new(nodes, combined).into_stream()
}

#[cfg(test)]
mod tests {
    use crate::{NanoTime, NodeOperators, RunFor, RunMode, StreamOperators, combine, ticker};
    use std::time::Duration;
    use tinyvec::tiny_vec;
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
                let expected = vec![
                    tiny_vec![1, 10, 100],
                    tiny_vec![2, 20, 200],
                    tiny_vec![3, 30, 300],
                ];
                println!("{:?}", res);
                assert_eq!(res, expected);
            })
            .run(run_mode, run_for)
            .unwrap();
    }
}