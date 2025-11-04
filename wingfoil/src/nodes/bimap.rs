use crate::types::*;
use derive_new::new;

use std::boxed::Box;
use std::rc::Rc;

/// Maps two streams into a single stream.  Used by [add](crate::nodes::add).
#[derive(new)]
pub(crate) struct BiMapStream<IN1, IN2, OUT: Element> {
    upstream1: Rc<dyn Stream<IN1>>,
    upstream2: Rc<dyn Stream<IN2>>,
    #[new(default)]
    value: OUT,
    func: Box<dyn Fn(IN1, IN2) -> OUT>,
}

impl<IN1, IN2, OUT: Element> MutableNode for BiMapStream<IN1, IN2, OUT> {
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        self.value = (self.func)(self.upstream1.peek_value(), self.upstream2.peek_value());
        true
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(
            vec![self.upstream1.clone().as_node(), self.upstream2.clone().as_node()],
            vec![],
        )
    }
}

impl<IN1: 'static, IN2: 'static, OUT: Element> StreamPeekRef<OUT> for BiMapStream<IN1, IN2, OUT> {
    fn peek_ref(&self) -> &OUT {
        &self.value
    }
}
