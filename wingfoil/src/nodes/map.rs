use derive_new::new;

use std::boxed::Box;
use std::rc::Rc;

use crate::types::*;

/// Map's it's source into a new [Stream] using the supplied closure.
/// Used by [map](crate::nodes::StreamOperators::map).
#[derive(new)]
pub struct MapStream<IN, OUT: Element> {
    upstream: Rc<dyn Stream<IN>>,
    #[new(default)]
    value: OUT,
    func: Box<dyn Fn(IN) -> OUT>,
}

impl<IN, OUT: Element> MutableNode for MapStream<IN, OUT> {
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        self.value = (self.func)(self.upstream.peek_value());
        true
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<IN: 'static, OUT: Element> StreamPeekRef<OUT> for MapStream<IN, OUT> {
    fn peek_ref(&self) -> &OUT {
        &self.value
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::graph::*;
    use crate::nodes::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn map_stream_works() {
        let input: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        let func = |x: u64| x + 1;
        let captured = input
            .clone()
            .as_stream()
            .map(func)
            .map(func)
            .logged("a", log::Level::Info)
            .collect();
        assert_eq!(input.peek_value(), 0);
        let mut expected = vec![];
        input.borrow_mut().push(ValueAt {
            value: 1,
            time: NanoTime::new(100),
        });
        expected.push(ValueAt {
            value: 3,
            time: NanoTime::new(100),
        });
        input.borrow_mut().push(ValueAt {
            value: 2,
            time: NanoTime::new(200),
        });
        expected.push(ValueAt {
            value: 4,
            time: NanoTime::new(200),
        });
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        println!("{:?}", expected);
        println!("{:?}", captured.peek_value());
        assert_eq!(expected, captured.peek_value());
    }
}
