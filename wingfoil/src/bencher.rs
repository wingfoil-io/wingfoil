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
        let builder = self.builder.take().expect("Bencher::start() called twice");
        let signal = self.signal.clone();
        let run_mode = self.run_mode;
        std::thread::spawn(move || {
            let _end_guard = SignalEndOnDrop(signal.clone());
            let produce_signal = signal.clone();
            let trigger = BenchTriggerNode::new(signal.clone()).into_node();
            let node = builder(trigger).produce(move || {
                produce_signal.store(Signal::End.into(), Ordering::SeqCst);
            });
            if let Err(e) = Graph::new(vec![node], run_mode, RunFor::Forever).run() {
                error!("bench graph failed: {e}");
            }
        });
    }

    pub fn stop(&self) {
        self.signal
            .clone()
            .store(Signal::Kill.into(), Ordering::SeqCst);
    }
}

struct SignalEndOnDrop(Arc<AtomicU8>);

impl Drop for SignalEndOnDrop {
    fn drop(&mut self) {
        self.0.store(Signal::End.into(), Ordering::SeqCst);
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
    fn upstreams(&self) -> UpStreams {
        UpStreams::default()
    }

    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        match self.signal.load(Ordering::SeqCst).into() {
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

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        state.always_callback()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::*;
    use crate::types::NanoTime;

    // ── Signal conversions ────────────────────────────────────────────────

    #[test]
    fn signal_from_u8_all_variants() {
        assert_eq!(Signal::from(1u8), Signal::Ready);
        assert_eq!(Signal::from(2u8), Signal::Begin);
        assert_eq!(Signal::from(3u8), Signal::Running);
        assert_eq!(Signal::from(4u8), Signal::End);
        assert_eq!(Signal::from(5u8), Signal::Kill);
    }

    #[test]
    #[should_panic(expected = "Invalid Instruction value")]
    fn signal_from_u8_invalid_panics() {
        let _ = Signal::from(99u8);
    }

    #[test]
    fn signal_into_u8_roundtrips() {
        for (sig, expected) in [
            (Signal::Ready, 1u8),
            (Signal::Begin, 2u8),
            (Signal::Running, 3u8),
            (Signal::End, 4u8),
            (Signal::Kill, 5u8),
        ] {
            let back: u8 = sig.into();
            assert_eq!(back, expected);
            assert_eq!(Signal::from(back), sig);
        }
    }

    // ── BenchTriggerNode::cycle branches ────────────────────────────────

    #[test]
    fn bench_trigger_cycle_ready_returns_false() {
        let signal = Arc::new(AtomicU8::new(Signal::Ready.into()));
        let node = BenchTriggerNode::new(signal.clone()).into_node();
        node.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        // Signal::Ready → cycle returns Ok(false), so graph never ticks downstream
        assert_eq!(signal.load(Ordering::SeqCst), u8::from(Signal::Ready));
    }

    #[test]
    fn bench_trigger_cycle_begin_transitions_to_running() {
        let signal = Arc::new(AtomicU8::new(Signal::Begin.into()));
        let node = BenchTriggerNode::new(signal.clone()).into_node();
        node.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        // Signal::Begin → cycle stores Running and returns Ok(true)
        assert_eq!(signal.load(Ordering::SeqCst), u8::from(Signal::Running));
    }

    #[test]
    fn bench_trigger_cycle_kill_returns_error() {
        let signal = Arc::new(AtomicU8::new(Signal::Kill.into()));
        let node = BenchTriggerNode::new(signal.clone()).into_node();
        let result = node.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1));
        assert!(result.is_err());
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
