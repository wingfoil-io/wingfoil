use crate::types::*;

use derive_new::new;

/// A [Node] that ticks at a specified interval.
/// Used by [ticker](crate::nodes::ticker).
#[derive(new)]
pub(crate) struct TickNode {
    interval: NanoTime,
    #[new(default)]
    at_time: Option<NanoTime>,
}

impl MutableNode for TickNode {
    fn cycle(&mut self, state: &mut GraphState) -> bool {
        let next_time = match self.at_time {
            Some(t) => {
                // anchor to first call to mitigate drift
                t + self.interval
            }
            None => {
                // first call
                state.time() + self.interval
            }
        };
        self.at_time = Some(next_time);
        state.add_callback(next_time);
        true
    }

    fn start(&mut self, state: &mut GraphState) {
        state.add_callback(NanoTime::ZERO);
    }
}

#[cfg(test)]
mod tests {

    use crate::graph::*;
    use crate::nodes::*;

    #[test]
    fn tick_node_works_in_realitme() {
        let period = Duration::from_millis(100);
        let run_to = RunFor::Duration(period * 5);
        let run_mode = RunMode::RealTime;
        let average = ticker(period)
            .ticked_at()
            .difference()
            .map(|time| {
                let t: u64 = time.into();
                t
            })
            .average();
        let average = average.clone().collect();
        average.run(run_mode, run_to).unwrap();
        let average = average.peek_value();
        average
            .iter()
            .for_each(|x| println!("{:} {:?}", x.time, x.value));
        let err = num::abs(period.as_nanos() as f64 - average.last().unwrap().value);
        debug_assert!(err < Duration::from_millis(10).as_nanos() as f64)
    }
}
