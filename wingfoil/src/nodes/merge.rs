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
    /// The merge semantics, single-sourced for both execution paths: emit the
    /// value of the *earliest-supplied* upstream whose position is ticked.
    fn emit_first_ticked(&mut self, ticked_at: impl Fn(usize) -> bool) -> bool {
        for pos in 0..self.upstreams.len() {
            if ticked_at(pos) {
                self.value = self.upstreams[pos].peek_value();
                return true;
            }
        }
        false
    }

    /// Statically-dispatched cycle for generated runners ([`crate::codegen`]).
    /// `upstream_ticked` carries the tick flags for `upstreams` (same order)
    /// from the compiled schedule — the interpreted path derives the same
    /// flags from `GraphState` in `cycle`; both feed
    /// [`emit_first_ticked`](Self::emit_first_ticked).
    #[doc(hidden)]
    pub fn cycle_inline(&mut self, upstream_ticked: &[bool]) -> bool {
        self.emit_first_ticked(|pos| upstream_ticked.get(pos).copied().unwrap_or(false))
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
        let indices = std::mem::take(&mut self.upstream_indices);
        let ticked = self.emit_first_ticked(|pos| state.node_index_ticked(indices[pos]));
        self.upstream_indices = indices;
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
