use crate::types::*;
use derive_new::new;

use std::rc::Rc;

/// Counts how many times upstream has ticked.
#[derive(new)]
pub struct MergeStream<T: Element> {
    upstreams: Vec<Rc<dyn Stream<T>>>,
    #[new(default)]
    value: T,
}

#[node(active = [upstreams], output = value: T)]
impl<T: Element> MutableNode for MergeStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        for stream in self.upstreams.iter() {
            if state.ticked(stream.clone().as_node()) {
                self.value = stream.peek_value();
                break;
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;

    #[test]
    fn merge_emits_from_both_streams() {
        // Two tickers at different rates. Merge should emit whenever either ticks.
        // a: every 100ns, b: every 200ns (both start at t=0 in historical mode).
        // RunFor::Duration(d) stops after the first tick where elapsed > d.
        // With d=400ns: a fires at 0,100,200,300,400,500 (stops after 500>400),
        //              b fires at 0,200,400.
        // Merged unique times (a wins ties): 0,100,200,300,400,500 → 6 ticks.
        let a = ticker(Duration::from_nanos(100)).count();
        let b = ticker(Duration::from_nanos(200))
            .count()
            .map(|x: u64| x * 100);
        let merged = merge(vec![a, b]).collect();
        merged
            .run(
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Duration(Duration::from_nanos(400)),
            )
            .unwrap();
        let times: Vec<NanoTime> = merged.peek_value().iter().map(|v| v.time).collect();
        assert_eq!(
            times,
            vec![
                NanoTime::new(0),
                NanoTime::new(100),
                NanoTime::new(200),
                NanoTime::new(300),
                NanoTime::new(400),
                NanoTime::new(500),
            ]
        );
    }

    #[test]
    fn merge_last_ticked_value_wins() {
        // Single-element merge is a pass-through
        let src = ticker(Duration::from_nanos(100)).count();
        let merged = merge(vec![src]).collect();
        merged
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();
        let values: Vec<u64> = merged.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(values, vec![1, 2, 3]);
    }
}
