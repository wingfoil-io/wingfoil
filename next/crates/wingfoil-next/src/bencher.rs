//! Criterion harness for benchmarking a live graph — the next twin of classic
//! wingfoil's `add_bench` (`wingfoil/src/bencher.rs`).
//!
//! Gated behind the `bench` feature, exactly as classic gates its own, so a
//! default build does not pull in criterion's dependency tree.
//!
//! # How it works (identical to classic)
//!
//! The graph runs `Forever` on a worker thread, driven by a source node that
//! ticks only when the criterion thread asks it to. One `iter()` step is:
//!
//! 1. the criterion thread stores `Begin` into a shared [`AtomicU8`];
//! 2. the worker's trigger node — declared [`Activation::ALWAYS`], so the
//!    realtime kernel never parks and cycles it back-to-back — observes `Begin`,
//!    flips it to `Running` and ticks;
//! 3. the graph under test propagates that tick;
//! 4. a sink node hung off the graph's output stores `End`;
//! 5. the criterion thread's spin loop sees `End` and the sample closes.
//!
//! So the measured quantity is one full engine cycle through the wired graph,
//! including scheduler overhead — not the cost of building it.
//!
//! # Engine mapping
//!
//! | classic | next |
//! |---|---|
//! | `MutableNode` + `state.always_callback()` | [`GraphBuilder::custom_node`] + [`Activation::ALWAYS`] |
//! | `cycle -> Ok(true)` / `Ok(false)` | [`Tick::Value`] / [`Tick::Quiet`] |
//! | `builder(trigger).produce(f)` | a `custom_node` sink on the returned [`Upstream`] |
//! | `Graph::new(.., RunFor::Forever).run()` | [`GraphBuilder::build`] + `Runner::run` |
//!
//! The builder closure is handed the [`GraphBuilder`] as well as the trigger
//! stream, because next wires nodes through a builder rather than through
//! `Rc<dyn Node>` values, and it returns a type-erased [`Upstream`] so a graph
//! of any output type can be benchmarked.
//!
//! One naming deviation: classic re-exports the helper at its crate root
//! (`wingfoil::add_bench`), while next keeps it on its module path
//! (`wingfoil_next::bencher::add_bench`), matching how the rest of the crate is
//! organised (`interp`, `op`, `fluent`, `stats`, `adapters::*`).

use crate::fluent::{GraphBuilder, Stream, Upstream};
use crate::op::{Activation, Tick};

use criterion::Criterion;
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use crate::{RunFor, RunMode};

/// Used to add a wingfoil-next bench to criterion.
///
/// `f` is handed the [`GraphBuilder`] and the bench trigger stream, wires the
/// logic under test, and returns the graph's output edge.
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

/// A function that accepts a graph builder plus a trigger stream, wires the
/// downstream logic to be benchmarked, and returns its output edge.
pub trait BenchBuilder: FnOnce(&GraphBuilder, &Stream<()>) -> Upstream + Send {}
impl<F> BenchBuilder for F where F: FnOnce(&GraphBuilder, &Stream<()>) -> Upstream + Send {}

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
        let builder = self
            .builder
            .take()
            .expect("invariant: Bencher::start called more than once");
        let signal = self.signal.clone();
        let run_mode = self.run_mode;
        std::thread::spawn(move || {
            let graph = GraphBuilder::new();
            let trigger = bench_trigger(&graph, signal.clone());
            let output = builder(&graph, &trigger);
            let signal_for_end = signal.clone();
            // The `produce` sink: tells the criterion thread that the cycle
            // reached the bottom of the graph.
            let _sink: Stream<()> =
                graph.custom_node(&[output], &[], Activation::NONE, move |_ctx| {
                    signal_for_end.store(Signal::End.into(), Ordering::SeqCst);
                    Ok(Tick::Value(()))
                });
            let mut runner = graph.build();
            if let Err(e) = runner.run(run_mode, RunFor::Forever) {
                log::error!("bencher worker thread terminated: {e:#}");
                // Wake the bencher loop so step() doesn't spin forever.
                signal.store(Signal::End.into(), Ordering::SeqCst);
            }
        });
    }

    pub fn stop(&self) {
        self.signal
            .clone()
            .store(Signal::Kill.into(), Ordering::SeqCst);
    }
}

/// The trigger source: an `ALWAYS` custom node that ticks exactly once per
/// `Begin` the criterion thread posts, and aborts the run on `Kill`.
fn bench_trigger(graph: &GraphBuilder, signal: Arc<AtomicU8>) -> Stream<()> {
    graph.custom_node(&[], &[], Activation::ALWAYS, move |_ctx| {
        match signal.load(Ordering::SeqCst).into() {
            Signal::Begin => {
                signal.store(Signal::Running.into(), Ordering::SeqCst);
                Ok(Tick::Value(()))
            }
            Signal::Kill => {
                anyhow::bail!("Killed")
            }
            _ => Ok(Tick::Quiet),
        }
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── Signal conversions (ported one-for-one from classic) ──────────────

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

    // ── The trigger node's cycle branches ─────────────────────────────────
    //
    // Classic drives these through a one-cycle historical run. Next rejects
    // `Activation::ALWAYS` under historical replay (there is no external
    // resource to poll), so the branches are exercised through a realtime run
    // bounded to a single cycle instead — same three assertions.

    fn run_trigger_once(initial: Signal) -> (Arc<AtomicU8>, anyhow::Result<()>) {
        let signal = Arc::new(AtomicU8::new(initial.into()));
        let graph = GraphBuilder::new();
        let _trigger = bench_trigger(&graph, signal.clone());
        let mut runner = graph.build();
        let result = runner.run(RunMode::RealTime, RunFor::Cycles(1));
        (signal, result)
    }

    #[test]
    fn bench_trigger_cycle_ready_stays_quiet() {
        let (signal, result) = run_trigger_once(Signal::Ready);
        result.unwrap();
        // Signal::Ready → Tick::Quiet, so the graph never ticks downstream.
        assert_eq!(signal.load(Ordering::SeqCst), u8::from(Signal::Ready));
    }

    #[test]
    fn bench_trigger_cycle_begin_transitions_to_running() {
        let (signal, result) = run_trigger_once(Signal::Begin);
        result.unwrap();
        // Signal::Begin → stores Running and ticks.
        assert_eq!(signal.load(Ordering::SeqCst), u8::from(Signal::Running));
    }

    #[test]
    fn bench_trigger_cycle_kill_returns_error() {
        let (_signal, result) = run_trigger_once(Signal::Kill);
        assert!(result.is_err());
    }
}
