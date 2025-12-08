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
                flushed = true;
            }

            // Always update window boundaries when time passes, regardless of data
            while self.next_window <= state.time() {
                self.next_window = self.next_window + self.interval;
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

    use crate::nodes::*;
    use crate::graph::*; // For RunMode, RunFor
    use crate::queue::ValueAt; // For ValueAt

    #[test]
    fn window_stream_works() {
        ticker(Duration::from_millis(100))
            .count()
            .logged(">>", log::Level::Info)
            .window(Duration::from_millis(250))
            .collect()
            .finally(|res, _| {
                println!("{:#?}", res);
                let expected = vec![
                    ValueAt {
                        value: vec![1, 2, 3],
                        time: NanoTime::new(300000000),
                    },
                    ValueAt {
                        value: vec![4, 5],
                        time: NanoTime::new(500000000),
                    },
                    ValueAt {
                        value: vec![6, 7, 8],
                        time: NanoTime::new(800000000),
                    },
                    ValueAt {
                        value: vec![9, 10],
                        time: NanoTime::new(1000000000),
                    },
                    ValueAt {
                        value: vec![11, 12, 13],
                        time: NanoTime::new(1300000000),
                    },
                ];
                assert_eq!(expected, res);
            })
            .run(
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Duration(Duration::from_millis(1200)),
            )
            .unwrap();
    }
}
