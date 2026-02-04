use crate::types::*;

pub struct TickNode {
    period: NanoTime,
}

impl TickNode {
    pub fn new(period: NanoTime) -> Self {
        Self { period }
    }
}

impl<'a> MutableNode<'a> for TickNode {
    fn cycle(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        if self.period > NanoTime::ZERO {
            if !state.is_last_cycle() {
                state.add_callback(state.time() + self.period);
            }
            Ok(true)
        } else {
            // period is zero, already always_callback
            Ok(true)
        }
    }

    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::none()
    }

    fn start(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        if self.period > NanoTime::ZERO {
            state.add_callback(state.time());
        } else {
            state.always_callback();
        }
        Ok(())
    }
}