use crate::types::*;
use std::fmt::Debug;

pub(crate) struct ConstantStream<'a, T: Debug + Clone + 'a> {
    value: T,
    ticked: bool,
    _phantom: std::marker::PhantomData<&'a T>,
}

impl<'a, T: Debug + Clone + 'a> ConstantStream<'a, T> {
    pub fn new(value: T) -> Self {
        Self {
            value,
            ticked: false,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<'a, T: Debug + Clone + 'a> MutableNode<'a> for ConstantStream<'a, T> {
    fn cycle(&mut self, _state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        if !self.ticked {
            self.ticked = true;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::none()
    }

    fn start(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        state.always_callback();
        Ok(())
    }
}

impl<'a, T: Debug + Clone + 'a> StreamPeekRef<'a, T> for ConstantStream<'a, T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}
