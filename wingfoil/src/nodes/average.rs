use crate::types::*;

use derive_new::new;

use num_traits::ToPrimitive;
use std::rc::Rc;

/// Computes running average.  Used by [average](crate::nodes::StreamOperators::average).
#[derive(new, StreamPeekRef, WiringPoint)]
pub(crate) struct AverageStream<T: Element> {
    #[active]
    upstream: Rc<dyn Stream<T>>,
    #[new(default)]
    #[output]
    value: f64,
    #[new(default)]
    count: u64,
}

impl<T: Element + ToPrimitive> MutableNode for AverageStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.count += 1;
        let sample = self.upstream.peek_value().to_f64().unwrap_or(f64::NAN);
        self.value += (sample - self.value) / self.count as f64;
        Ok(true)
    }
}
