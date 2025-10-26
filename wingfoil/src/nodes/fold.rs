use crate::types::*;

use derive_new::new;

use std::rc::Rc;

#[derive(new)]
pub(crate) struct FoldStream<IN: Element, OUT: Element> {
    upstream: Rc<dyn Stream<IN>>,
    func: Box<dyn Fn(&mut OUT, IN)>,
    #[new(default)]
    value: OUT,
}

impl<IN: Element, OUT: Element> MutableNode for FoldStream<IN, OUT> {
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        (self.func)(&mut self.value, self.upstream.peek_value());
        true
    }
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

impl<IN: Element, OUT: Element> StreamPeekRef<OUT> for FoldStream<IN, OUT> {
    fn peek_ref(&self) -> &OUT {
        &self.value
    }
}

#[cfg(test)]
mod tests {

    use crate::graph::*;
    use crate::nodes::*;
    use crate::time::NanoTime;
    use std::time::Duration;

    #[test]
    fn fold_sum_works() {
        let f = |a: &mut u64, b: u64| {
            *a += b;
        };
        let reduced = ticker(Duration::from_nanos(100)).count().fold(Box::new(f));
        let captured = reduced.clone().collect();
        assert_eq!(reduced.peek_value(), 0);
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();
        let expected = vec![
            ValueAt {
                value: 1,
                time: NanoTime::new(0),
            }, // 0 + 1 = 1
            ValueAt {
                value: 3,
                time: NanoTime::new(100),
            }, // 1 + 2 = 3
            ValueAt {
                value: 6,
                time: NanoTime::new(200),
            }, // 3 + 3 = 6
            ValueAt {
                value: 10,
                time: NanoTime::new(300),
            }, // 6 + 4 = 10
        ];
        assert_eq!(expected, captured.peek_value());
        assert_eq!(10, reduced.peek_value());
    }

    #[test]
    fn fold_collect_works() {
        let f = |a: &mut Vec<u64>, b: u64| {
            a.push(b);
        };
        let reduced = ticker(Duration::from_nanos(100)).count().fold(Box::new(f));
        let captured = reduced.clone().collect();
        assert!(reduced.peek_value().is_empty());
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();
        let expected = vec![
            ValueAt {
                value: vec![1],
                time: NanoTime::new(0),
            },
            ValueAt {
                value: vec![1, 2],
                time: NanoTime::new(100),
            },
            ValueAt {
                value: vec![1, 2, 3],
                time: NanoTime::new(200),
            },
            ValueAt {
                value: vec![1, 2, 3, 4],
                time: NanoTime::new(300),
            },
        ];
        assert_eq!(expected, captured.peek_value());
        assert_eq!(vec![1, 2, 3, 4], reduced.peek_value());
    }

    #[test]
    fn count() {
        let count = ticker(Duration::from_nanos(100)).count();
        let captured = count.clone().collect();
        assert_eq!(count.peek_value(), 0);
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();
        let expected = vec![
            ValueAt {
                value: 1,
                time: NanoTime::new(0),
            },
            ValueAt {
                value: 2,
                time: NanoTime::new(100),
            },
            ValueAt {
                value: 3,
                time: NanoTime::new(200),
            },
        ];
        assert_eq!(expected, captured.peek_value());
        assert_eq!(3, count.peek_value());
    }
}
