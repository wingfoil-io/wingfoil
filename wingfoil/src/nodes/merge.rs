use crate::types::*;
use derive_new::new;

use std::rc::Rc;

/// Counts how many times upstream has ticked.
#[derive(new)]
pub struct MergeStream<T: Element> {
    upstreams: Vec<Rc<dyn Stream<T>>>,
    #[new(default)]
    value: T,
}

impl<T: Element> MutableNode for MergeStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> bool {
        for stream in self.upstreams.iter() {
            if state.ticked(stream.clone().as_node()) {
                self.value = stream.peek_value();
                break;
            }
        }
        true
    }
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(
            self.upstreams
                .iter()
                .map(|stream| stream.clone().as_node())
                .collect(),
            vec![],
        )
    }
}

impl<T: Element> StreamPeekRef<T> for MergeStream<T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}

#[cfg(test)]
mod tests {
    use crate::{NodeOperators, RunFor, RunMode, StreamOperators, always, merge};
    #[test]
    fn merge_works() {
        // cargo flamegraph  --unit-test -- merge_works
        let src = always().count();
        let streams = (0..10)
            .map(|_| {
                let mut stream = src.clone();
                for _ in 0..10 {
                    stream = stream.map(std::hint::black_box);
                }
                stream
            })
            .collect::<Vec<_>>();
        merge(streams)
            .run(RunMode::RealTime, RunFor::Cycles(1))
            .unwrap();
    }
}
