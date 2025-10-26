use derive_new::new;

use std::boxed::Box;
use std::rc::Rc;

use crate::types::*;

/// Map's it's source into a new [Stream] using the supplied closure.
/// Used by [map](crate::nodes::StreamOperators::map).
#[derive(new)]
pub struct MapFilterStream<IN, OUT: Element> {
    upstream: Rc<dyn Stream<IN>>,
    #[new(default)]
    value: OUT,
    func: Box<dyn Fn(IN) -> (OUT, bool)>,
}

impl<IN, OUT: Element> MutableNode for MapFilterStream<IN, OUT> {
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        let (val, ticked) = (self.func)(self.upstream.peek_value());
        if ticked {
            self.value = val;
        }
        ticked
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<IN: 'static, OUT: Element> StreamPeekRef<OUT> for MapFilterStream<IN, OUT> {
    fn peek_ref(&self) -> &OUT {
        &self.value
    }
}
