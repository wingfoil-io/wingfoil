use std::rc::Rc;
use crate::types::*;
use std::fmt::Debug;

pub(crate) struct BiMapStream<'a, IN1, IN2, OUT> {
    upstream1: Rc<dyn Stream<'a, IN1> + 'a>,
    upstream2: Rc<dyn Stream<'a, IN2> + 'a>,
    value: OUT,
    func: Box<dyn Fn(IN1, IN2) -> OUT + 'a>,
}

impl<'a, IN1, IN2, OUT: Debug + Clone + Default + 'a> BiMapStream<'a, IN1, IN2, OUT> {
    pub fn new(
        upstream1: Rc<dyn Stream<'a, IN1> + 'a>,
        upstream2: Rc<dyn Stream<'a, IN2> + 'a>,
        func: Box<dyn Fn(IN1, IN2) -> OUT + 'a>,
    ) -> Self {
        Self {
            upstream1,
            upstream2,
            value: OUT::default(),
            func,
        }
    }
}

impl<'a, IN1: Clone, IN2: Clone, OUT: Debug + Clone + 'a> MutableNode<'a> for BiMapStream<'a, IN1, IN2, OUT> {
    fn cycle(&mut self, _state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        self.value = (self.func)(self.upstream1.peek_value(), self.upstream2.peek_value());
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(
            vec![self.upstream1.clone().as_node(), self.upstream2.clone().as_node()],
            vec![],
        )
    }
}

impl<'a, IN1: Clone, IN2: Clone, OUT: Debug + Clone + 'a> StreamPeekRef<'a, OUT> for BiMapStream<'a, IN1, IN2, OUT> {
    fn peek_ref(&self) -> &OUT {
        &self.value
    }
}
