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
        self.buffer.push(self.upstream.peek_value());
        if state.time() >= self.next_window || (!self.buffer.is_empty() && state.is_last_cycle()) {
            self.value = self.buffer.clone();
            self.buffer.clear();
            assert!(!self.value.is_empty());

            // Advance window. Handle cases where we might have skipped multiple windows or just one.
            // For simple "buffer like" behavior, just moving the deadline forward is enough.
            // But aligned windows usually align to the clock.
            // Let's stick to "interval from start" logic to align windows.
            // But state.time() is what matters.
            
            // If we are late, we just set the next window to be current time + interval? 
            // Or keep strictly to the grid? 
            // Strict grid: while next_window <= state.time() { next_window += interval }
            
            while self.next_window <= state.time() {
                self.next_window = self.next_window + self.interval;
            }
            true
        } else {
            false
        }
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

    #[test]
    fn window_stream_works() {
        //env_logger::init();
        let period = Duration::from_millis(200);
        let n = 5;
        // Run for enough time to trigger multiple windows
        for mode in [RunMode::HistoricalFrom(NanoTime::ZERO), RunMode::RealTime] {
            // Window size 500ms. Ticker 200ms. 
            // Window 1: 0, 200, 400. Flatten @ >= 500?
            // Ticks: 0, 200, 400, 600, 800, 1000
            // W1 (0-500): 0, 200, 400. Flush at 600?
            // If we only cycle on upstream tick, we will flush at 600 with data [0, 200, 400, 600].
            // Wait, if 600 is >= 500, we flush. 
            // Logic: push current (600). check 600 >= 500. flush [0, 200, 400, 600].
            // This includes the boundary item in the previous window if we push first.
            // Maybe we should check BEFORE push?
            // BufferStream pushes then checks.
            
            let count = ticker(period).count();
            let windowed = count.window(Duration::from_millis(500));
            // Run for 1100 ms.
            // Ticks: 0, 200, 400, 600, 800, 1000
            // Windows: 
            // 0-500: Ticks 0, 200, 400. 
            // Next tick 600. 600 > 500. Flush.
            
            windowed.run(mode, RunFor::Duration(Duration::from_millis(1100))).unwrap();
        }
    }
}
