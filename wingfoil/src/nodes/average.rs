use crate::types::*;

use derive_new::new;

use num_traits::ToPrimitive;
use std::rc::Rc;

/// Computes running average.  Used by [average](crate::nodes::StreamOperators::average).
#[derive(new)]
pub(crate) struct AverageStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    #[new(default)]
    value: f64,
    #[new(default)]
    count: u64,
}

impl<T: Element + ToPrimitive> MutableNode for AverageStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        self.count += 1;
        let sample = match self.upstream.peek_value().to_f64() {
            Some(smpl) => smpl,
            None => f64::NAN,
        };
        self.value += (sample - self.value) / self.count as f64;
        true
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<T: Element + ToPrimitive> StreamPeekRef<f64> for AverageStream<T> {
    fn peek_ref(&self) -> &f64 {
        &self.value
    }
}
