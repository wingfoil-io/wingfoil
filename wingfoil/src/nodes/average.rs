use crate::types::*;

use derive_new::new;

use num_traits::ToPrimitive;
use std::rc::Rc;
use std::fmt::Debug;

/// Computes running average.  Used by [average](crate::nodes::StreamOperators::average).
#[derive(new)]
pub(crate) struct AverageStream<'a, T> {
    upstream: Rc<dyn Stream<'a, T> + 'a>,
    #[new(default)]
    value: f64,
    #[new(default)]
    count: u64,
}

impl<'a, T: ToPrimitive + Debug + Clone + 'a> MutableNode<'a> for AverageStream<'a, T> {
    fn cycle(&mut self, _state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        self.count += 1;
        let sample = self.upstream.peek_value().to_f64().unwrap_or(f64::NAN);
        self.value += (sample - self.value) / self.count as f64;
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<'a, T: ToPrimitive + Debug + Clone + 'a> StreamPeekRef<'a, f64> for AverageStream<'a, T> {
    fn peek_ref(&self) -> &f64 {
        &self.value
    }
}
