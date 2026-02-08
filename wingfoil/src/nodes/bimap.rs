use crate::types::*;
use derive_new::new;

use std::boxed::Box;

/// Maps two streams into a single stream.  Used by [add](crate::nodes::add).
#[derive(new)]
pub(crate) struct BiMapStream<IN1, IN2, OUT: Element> {
    upstream1: Dep<IN1>,
    upstream2: Dep<IN2>,
    #[new(default)]
    value: OUT,
    func: Box<dyn Fn(IN1, IN2) -> OUT>,
}

impl<IN1: 'static, IN2: 'static, OUT: Element> MutableNode for BiMapStream<IN1, IN2, OUT> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = (self.func)(
            self.upstream1.stream().peek_value(),
            self.upstream2.stream().peek_value(),
        );
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams {
        let (active, passive): (Vec<_>, Vec<_>) = [
            (self.upstream1.as_node(), self.upstream1.is_active()),
            (self.upstream2.as_node(), self.upstream2.is_active()),
        ]
        .into_iter()
        .partition(|(_, active)| *active);
        UpStreams::new(
            active.into_iter().map(|(n, _)| n).collect(),
            passive.into_iter().map(|(n, _)| n).collect(),
        )
    }
}

impl<IN1: 'static, IN2: 'static, OUT: Element> StreamPeekRef<OUT> for BiMapStream<IN1, IN2, OUT> {
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
    fn bimap_both_active() {
        // Both upstreams active: bimap ticks when either source ticks.
        // Two independent tickers at different rates both drive output.
        let a = ticker(Duration::from_nanos(100)).count();
        let b = ticker(Duration::from_nanos(100))
            .count()
            .map(|x: u64| x * 10);
        let stream = bimap(Dep::Active(a), Dep::Active(b), |a: u64, b: u64| a + b).collect();
        stream
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();
        assert_eq!(stream.peek_value().last().unwrap().value, 44);
    }

    #[test]
    fn bimap_passive_does_not_trigger() {
        // Passive upstream does not trigger bimap.
        // a ticks every 100ns (active), b ticks every 50ns (passive).
        // Output should only tick on a's schedule, not b's.
        let a = ticker(Duration::from_nanos(100)).count();
        let b = ticker(Duration::from_nanos(50)).count();
        let stream = bimap(Dep::Active(a), Dep::Passive(b), |a: u64, b: u64| a + b).collect();
        stream
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
            .unwrap();
        // Output should only have ticked at a's times, not at b's 50ns intervals
        let times: Vec<NanoTime> = stream.peek_value().iter().map(|v| v.time).collect();
        assert_eq!(
            times,
            vec![NanoTime::new(0), NanoTime::new(100), NanoTime::new(200)]
        );
    }
}
