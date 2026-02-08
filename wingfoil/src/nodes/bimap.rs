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
    fn bimap_delay_difference() {
        let source = ticker(Duration::from_secs(1)).count().map(|x| x as i64 + 4); // 5, 6, 7, 8, 9, 10, 11, 12, 13, ...

        let delayed = source.delay(Duration::from_secs(5)); // same sequence, 5s later

        let diff = bimap(Dep::Active(source), Dep::Passive(delayed), |a, b| a - b);

        diff.accumulate()
            .finally(|res, _| {
                assert_eq!(res, vec![0, 1, 2, 3, 4, 5, 5, 5, 5, 5]);
                Ok(())
            })
            .run(
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Duration(Duration::from_secs(8)),
            )
            .unwrap();
    }
}
