use crate::types::*;
use derive_new::new;

use std::boxed::Box;

/// Maps two streams into a single stream using a fallible closure.
/// Used by [try_bimap](crate::nodes::try_bimap).
#[derive(new)]
pub(crate) struct TryBiMapStream<IN1, IN2, OUT: Element> {
    upstream1: Dep<IN1>,
    upstream2: Dep<IN2>,
    #[new(default)]
    value: OUT,
    func: Box<dyn Fn(IN1, IN2) -> anyhow::Result<OUT>>,
}

impl<IN1: 'static, IN2: 'static, OUT: Element> MutableNode for TryBiMapStream<IN1, IN2, OUT> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = (self.func)(
            self.upstream1.stream().peek_value(),
            self.upstream2.stream().peek_value(),
        )?;
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams {
        let deps = [
            (self.upstream1.as_node(), self.upstream1.is_active()),
            (self.upstream2.as_node(), self.upstream2.is_active()),
        ];
        let (active, passive): (Vec<_>, Vec<_>) = deps.into_iter().partition(|(_, active)| *active);
        UpStreams::new(
            active.into_iter().map(|(n, _)| n).collect(),
            passive.into_iter().map(|(n, _)| n).collect(),
        )
    }
}

impl<IN1: 'static, IN2: 'static, OUT: Element> StreamPeekRef<OUT>
    for TryBiMapStream<IN1, IN2, OUT>
{
    fn peek_ref(&self) -> &OUT {
        &self.value
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;
    use std::time::Duration;

    #[test]
    fn try_bimap_success() {
        let tick = ticker(Duration::from_nanos(100));
        let a = tick.count();
        let b = tick.count().map(|x| x * 10);
        let stream = try_bimap(Dep::Active(a), Dep::Active(b), |a, b| Ok(a + b));
        stream
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert_eq!(stream.peek_value(), 55);
    }

    #[test]
    fn try_bimap_error() {
        let tick = ticker(Duration::from_nanos(100));
        let a = tick.count();
        let b = tick.count();
        let stream = try_bimap(
            Dep::Active(a),
            Dep::Active(b),
            |_: u64, _: u64| -> anyhow::Result<u64> { anyhow::bail!("oops") },
        );
        let result = stream.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1));
        assert!(result.is_err());
    }
}
