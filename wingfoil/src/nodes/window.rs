use crate::types::*;
use std::rc::Rc;

pub(crate) struct WindowStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    interval: NanoTime,
    next_window: NanoTime,
    buffer: Vec<T>,
    value: Vec<T>,
}

impl<T: Element> MutableNode for WindowStream<T> {
    fn start(&mut self, state: &mut GraphState) {
        self.next_window = state.time() + self.interval;
    }

    fn cycle(&mut self, state: &mut GraphState) -> bool {
        let mut flushed = false;
        if state.time() >= self.next_window {
            if !self.buffer.is_empty() {
                self.value = self.buffer.clone();
                self.buffer.clear();

                while self.next_window <= state.time() {
                    self.next_window = self.next_window + self.interval;
                }
                flushed = true;
            }
        }

        self.buffer.push(self.upstream.peek_value());

        if !flushed && state.is_last_cycle() && !self.buffer.is_empty() {
             self.value = self.buffer.clone();
             self.buffer.clear();
             flushed = true;
        }
        
        flushed
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<T: Element> StreamPeekRef<Vec<T>> for WindowStream<T> {
    fn peek_ref(&self) -> &Vec<T> {
        &self.value
    }
}

impl<T: Element> WindowStream<T> {
    pub fn new(upstream: Rc<dyn Stream<T>>, interval: NanoTime) -> Self {
        Self {
            upstream,
            interval,
            next_window: NanoTime::ZERO,
            buffer: Vec::new(),
            value: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {

    use crate::graph::*;
    use crate::nodes::*;
    use crate::queue::ValueAt;

    #[test]
    fn window_stream_works() {
        //env_logger::init();
        let period = Duration::from_millis(200);

        // Run for enough time to trigger multiple windows
        for mode in [RunMode::HistoricalFrom(NanoTime::ZERO), RunMode::RealTime] {
            let count = ticker(period).count();
            let windowed = count.window(Duration::from_millis(500));
            // Run for 1100 ms.
            // Ticks: 0, 200, 400, 600, 800, 1000
            
            // Expected Windows:
            // W1 [0, 500): 0(1), 200(2), 400(3) -> Flush at 600 check (time >= 500)
            // W2 [500, 1000): 600(4), 800(5) -> Flush at 1000 check (time >= 1000)
            // W3 [1000, 1500): 1000(6) -> Flush at last cycle (1100) mechanism?
            // Actually, if run for 1100ms.
            // 0: push 1.
            // 200: push 2.
            // 400: push 3.
            // 600: 600 >= 500. flush [1, 2, 3]. push 4. next_window = 1000.
            // 800: push 5.
            // 1000: 1000 >= 1000. flush [4, 5]. push 6. next_window = 1500.
            // 1200: (not run, run_for is duration 1100) -> Wait, next tick is 1200.
            // But loop runs check at 1000? run_for duration usually stops AT limit.
            // If run nodes logic: loop while time <= end.
            
            let result = windowed.collect();
            windowed.run(mode, RunFor::Duration(Duration::from_millis(1100))).unwrap();
            
            let data = result.peek_value();
            // Verify structure
            assert_eq!(data.len(), 2); 
            // W1
            assert_eq!(data[0].value, vec![1, 2, 3]);
            // W2
            assert_eq!(data[1].value, vec![4, 5]);
            // It seems last cycle flush might not trigger if no tick happens?
            // Ticker at 1000 happens.
            // Let's rely on standard collecting behavior.
        }
    }
}
