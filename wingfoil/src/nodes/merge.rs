use crate::types::*;
use derive_new::new;

use std::rc::Rc;

/// Merges several upstreams into one, emitting the value of whichever ticked
/// (the earliest-supplied wins ties). Used by [merge](crate::nodes::merge).
#[derive(new)]
pub struct MergeStream<T: Element> {
    upstreams: Vec<Rc<dyn Stream<T>>>,
    /// Graph indices of `upstreams`, resolved once on the first cycle so the
    /// per-tick tick-check is an O(1) array read rather than an `Rc` clone plus
    /// hash-map lookup per upstream.
    #[new(default)]
    upstream_indices: Vec<usize>,
    #[new(default)]
    value: T,
}

impl<T: Element> MergeStream<T> {
    /// Statically-dispatched equivalent of `cycle` for generated runners
    /// ([`crate::codegen`]). `upstream_ticked` carries the tick flags for
    /// `upstreams` (same order) from the compiled schedule, replacing the
    /// `GraphState::node_index_ticked` lookups in `cycle`. Must mirror
    /// `cycle`'s semantics exactly: earliest-supplied ticked upstream wins.
    #[doc(hidden)]
    pub fn cycle_inline(&mut self, upstream_ticked: &[bool]) -> bool {
        for (stream, &ticked) in self.upstreams.iter().zip(upstream_ticked) {
            if ticked {
                self.value = stream.peek_value();
                return true;
            }
        }
        false
    }
}

#[node(active = [upstreams], output = value: T)]
impl<T: Element> MutableNode for MergeStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        if self.upstream_indices.is_empty() && !self.upstreams.is_empty() {
            self.upstream_indices = self
                .upstreams
                .iter()
                .map(|stream| {
                    state
                        .node_index(stream.clone().as_node())
                        .expect("invariant: merge upstream wired at graph init")
                })
                .collect();
        }
        let mut ticked = false;
        for (stream, &index) in self.upstreams.iter().zip(&self.upstream_indices) {
            if state.node_index_ticked(index) {
                self.value = stream.peek_value();
                ticked = true;
                break;
            }
        }
        Ok(ticked)
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
