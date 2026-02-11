use crate::types::*;
use derive_new::new;

use std::boxed::Box;

/// Maps three streams into a single stream using a fallible closure.
#[derive(new)]
pub(crate) struct TryTriMapStream<IN1, IN2, IN3, OUT: Element> {
    upstream1: Dep<IN1>,
    upstream2: Dep<IN2>,
    upstream3: Dep<IN3>,
    #[new(default)]
    value: OUT,
    func: Box<dyn Fn(IN1, IN2, IN3) -> anyhow::Result<OUT>>,
}

impl<IN1: 'static, IN2: 'static, IN3: 'static, OUT: Element> MutableNode
    for TryTriMapStream<IN1, IN2, IN3, OUT>
{
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = (self.func)(
            self.upstream1.stream().peek_value(),
            self.upstream2.stream().peek_value(),
            self.upstream3.stream().peek_value(),
        )?;
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams {
        let (active, passive): (Vec<_>, Vec<_>) = [
            (self.upstream1.as_node(), self.upstream1.is_active()),
            (self.upstream2.as_node(), self.upstream2.is_active()),
            (self.upstream3.as_node(), self.upstream3.is_active()),
        ]
        .into_iter()
        .partition(|(_, active)| *active);
        UpStreams::new(
            active.into_iter().map(|(n, _)| n).collect(),
            passive.into_iter().map(|(n, _)| n).collect(),
        )
    }
}

impl<IN1: 'static, IN2: 'static, IN3: 'static, OUT: Element> StreamPeekRef<OUT>
    for TryTriMapStream<IN1, IN2, IN3, OUT>
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
    fn try_trimap_success() {
        let tick = ticker(Duration::from_nanos(100));
        let a = tick.count();
        let b = tick.count().map(|x| x * 10);
        let c = tick.count().map(|x| x * 100);
        let stream = try_trimap(Dep::Active(a), Dep::Active(b), Dep::Active(c), |a, b, c| {
            Ok(a + b + c)
        });
        stream
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert_eq!(stream.peek_value(), 555);
    }

    #[test]
    fn try_trimap_error() {
        let tick = ticker(Duration::from_nanos(100));
        let a = tick.count();
        let b = tick.count();
        let c = tick.count();
        let stream = try_trimap(
            Dep::Active(a),
            Dep::Active(b),
            Dep::Active(c),
            |_: u64, _: u64, _: u64| -> anyhow::Result<u64> { anyhow::bail!("oops") },
        );
        let result = stream.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1));
        assert!(result.is_err());
    }

    #[test]
    fn try_trimap_passive_does_not_trigger() {
        let a = ticker(Duration::from_nanos(100)).count();
        let b = ticker(Duration::from_nanos(50)).count();
        let c = ticker(Duration::from_nanos(50)).count();
        let stream = try_trimap(
            Dep::Active(a),
            Dep::Passive(b),
            Dep::Passive(c),
            |a: u64, b: u64, c: u64| Ok(a + b + c),
        )
        .collect();
        stream
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
            .unwrap();
        let times: Vec<NanoTime> = stream.peek_value().iter().map(|v| v.time).collect();
        assert_eq!(
            times,
            vec![NanoTime::new(0), NanoTime::new(100), NanoTime::new(200)]
        );
    }
}
