use crate::types::*;

use std::rc::Rc;

pub(crate) struct BufferStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    capacity: usize,
    buffer: Vec<T>,
    value: Vec<T>,
}

impl<T: Element> MutableNode for BufferStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> bool {
        self.buffer.push(self.upstream.peek_value());
        if self.buffer.len() >= self.capacity || (!self.buffer.is_empty() && state.is_last_cycle()) {
            self.value = self.buffer.clone();
            self.buffer.clear();
            assert!(!self.value.is_empty());
            true
        } else {
            false
        }
    }
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<T: Element> StreamPeekRef<Vec<T>> for BufferStream<T> {
    fn peek_ref(&self) -> &Vec<T> {
        &self.value
    }
}

impl<T: Element> BufferStream<T> {
    pub fn new(upstream: Rc<dyn Stream<T>>, capacity: usize) -> Self {
        Self {
            upstream,
            capacity,
            buffer: Vec::with_capacity(capacity),
            value: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {

    use crate::graph::*;
    use crate::nodes::*;

    #[test]
    fn buffer_stream_works() {
        //env_logger::init();
        let period = Duration::from_millis(200);
        let n = 5;
        for mode in [RunMode::HistoricalFrom(NanoTime::ZERO), RunMode::RealTime] {
            for run_for in [RunFor::Cycles(n), RunFor::Duration(period * n)] {
                let count = ticker(Duration::from_millis(500)).count();
                let buffer = count.buffer(2);
                buffer.run(mode, run_for).unwrap();
                let buffer = buffer.peek_value();
                let src = count.peek_value();
                let buffered = buffer[buffer.len() - 1];
                info!("{:?}, {:?}, {:?}, {:?}", mode, run_for, src, buffered);
                assert_eq!(src, buffered);
            }
        }
    }
}
