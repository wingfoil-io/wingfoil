use crate::{Graph, GraphState, IntoNode, MutableNode, Node, NodeOperators, RunFor, RunMode};

use derive_new::new;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use criterion::Criterion;


/// Used to add wingfoil bench to criterion.
pub fn add_bench<F>(crit: &mut Criterion, name: &str, f: F)
where
    F: BenchBuilder + 'static,
{
    let custom_bencher = RefCell::new(Bencher::new(f, RunMode::RealTime));
    custom_bencher.borrow_mut().start();

    crit.bench_function(name, {
        let custom_bencher = &custom_bencher;
        move |b| {
            b.iter(|| {
                custom_bencher.borrow_mut().step();
            });
        }
    });

    custom_bencher.borrow_mut().stop();
}

/// A function that accepts a trigger node and wires downstream logic to be
/// benchmarked.
pub trait BenchBuilder: FnOnce(Rc<dyn Node>) -> Rc<dyn Node> + Send {}
impl<F> BenchBuilder for F where F: FnOnce(Rc<dyn Node>) -> Rc<dyn Node> + Send {}

struct Bencher {
    run_mode: RunMode,
    signal: Arc<AtomicU8>,
    builder: Option<Box<dyn BenchBuilder>>,
}

impl Bencher {
    pub fn new<B: BenchBuilder + 'static>(builder: B, run_mode: RunMode) -> Self {
        let signal = Arc::new(AtomicU8::new(Signal::Ready.into()));
        let builder: Option<Box<dyn BenchBuilder>> = Some(Box::new(builder));
        Self {
            signal,
            builder,
            run_mode,
        }
    }

    pub fn step(&mut self) {
        self.signal.store(Signal::Begin.into(), Ordering::SeqCst);
        loop {
            match self.signal.load(Ordering::SeqCst).into() {
                Signal::End => {
                    break;
                }
                _ => {
                    std::hint::spin_loop();
                }
            }
        }
    }

    pub fn start(&mut self) {
        let builder = self.builder.take().unwrap();
        let signal = self.signal.clone();
        let run_mode = self.run_mode;
        std::thread::spawn(move || {
            let trigger = BenchTriggerNode::new(signal.clone()).into_node();
            let node = builder(trigger).produce(move || {
                signal.store(Signal::End.into(), Ordering::SeqCst);
            });
            Graph::new(vec![node], run_mode, RunFor::Forever)
                .run()
                .unwrap();
        });
    }

    pub fn stop(&self) {
        self.signal
            .clone()
            .store(Signal::Kill.into(), Ordering::SeqCst);
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Signal {
    Ready = 1,
    Begin = 2,
    Running = 3,
    End = 4,
    Kill = 5,
}

impl From<u8> for Signal {
    fn from(val: u8) -> Self {
        match val {
            1 => Signal::Ready,
            2 => Signal::Begin,
            3 => Signal::Running,
            4 => Signal::End,
            5 => Signal::Kill,
            _ => panic!("Invalid Instruction value: {val}"),
        }
    }
}

impl From<Signal> for u8 {
    fn from(sig: Signal) -> Self {
        sig as u8
    }
}

#[derive(new)]
struct BenchTriggerNode {
    signal: Arc<AtomicU8>,
}

impl MutableNode for BenchTriggerNode {
    fn cycle(&mut self, state: &mut GraphState) -> bool {
        match self.signal.load(Ordering::SeqCst).into() {
            Signal::Begin => {
                self.signal.store(Signal::Running.into(), Ordering::SeqCst);
                true
            }
            Signal::Kill => {
                state.terminate(Ok(()));
                false
            }
            _ => false,
        }
    }

    fn start(&mut self, state: &mut GraphState) {
        state.always_callback();
    }
}

// #[derive(new)]
// struct BenchTriggerNode {
//     signal: Arc<AtomicU8>
// }

// impl MutableNode for BenchTriggerNode {
//     fn cycle(&mut self, _: &mut GraphState) -> bool {
//         true
//     }
//     fn start(&mut self, state: &mut GraphState) {
//         let signal = self.signal.clone();
//         let notifier = state.ready_notifier();
//         std::thread::spawn(move || {
//             loop {
//                 match signal.load(Ordering::SeqCst).into() {
//                     Signal::Begin => {
//                         signal.store(Signal::Running.into(), Ordering::SeqCst);
//                         notifier.notify();
//                     },
//                     Signal::Kill => {
//                         break;
//                     },
//                     _ => {
//                         std::hint::spin_loop();
//                     },
//                 }
//             }
//         });
//     }
// }
