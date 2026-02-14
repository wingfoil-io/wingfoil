use crate::{
    Graph, GraphState, IntoNode, MutableNode, Node, NodeOperators, RunFor, RunMode, UpStreams,
};

use criterion::Criterion;
use derive_new::new;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// Used to add wingfoil bench to criterion.
pub fn add_bench<F>(crit: &mut Criterion, name: &str, f: F)
where
    F: BenchBuilder + 'static,
{
    let custom_bencher = RefCell::new(Bencher::new(f, RunMode::RealTime));
    custom_bencher
        .borrow_mut()
        .start()
        .expect("failed to start bencher");

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
            if let Ok(Signal::End) = self.signal.load(Ordering::SeqCst).try_into() {
                break;
            }
            std::hint::spin_loop();
        }
    }

    pub fn start(&mut self) -> anyhow::Result<()> {
        let builder = self
            .builder
            .take()
            .ok_or_else(|| anyhow::anyhow!("bencher already started"))?;
        let signal = self.signal.clone();
        let run_mode = self.run_mode;
        std::thread::spawn(move || {
            let trigger = BenchTriggerNode::new(signal.clone()).into_node();
            let node = builder(trigger).produce(move || {
                signal.store(Signal::End.into(), Ordering::SeqCst);
            });
            let result =
                Graph::new(vec![node], run_mode, RunFor::Forever).and_then(|mut g| g.run());
            if let Err(e) = result {
                log::error!("bench graph run failed: {e:?}");
            }
        });
        Ok(())
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

impl TryFrom<u8> for Signal {
    type Error = anyhow::Error;
    fn try_from(val: u8) -> anyhow::Result<Self> {
        match val {
            1 => Ok(Signal::Ready),
            2 => Ok(Signal::Begin),
            3 => Ok(Signal::Running),
            4 => Ok(Signal::End),
            5 => Ok(Signal::Kill),
            _ => anyhow::bail!("invalid Signal value: {val}"),
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
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        match self.signal.load(Ordering::SeqCst).try_into()? {
            Signal::Begin => {
                self.signal.store(Signal::Running.into(), Ordering::SeqCst);
                Ok(true)
            }
            Signal::Kill => {
                anyhow::bail!("Killed")
            }
            _ => Ok(false),
        }
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::default()
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        state.always_callback()?;
        Ok(())
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
