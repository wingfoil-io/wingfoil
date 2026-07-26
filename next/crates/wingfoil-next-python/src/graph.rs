//! The erased object form: [`PyGraph`] and [`PyStream`].
//!
//! Python needs to *hold* a graph and keep wiring into it across the FFI
//! boundary, then run it and read values back. The interpreted engine's
//! [`GraphBuilder`] is already a shared, still-open builder (`Rc<RefCell<..>>`
//! under the hood — every clone appends to the *same* graph), so the object
//! form is a thin, **non-generic** wrapper over it at [`PyElement`]:
//!
//! - [`PyGraph`] owns the shared builder plus a runner slot, and hands out
//!   sources as [`PyStream`]s.
//! - [`PyStream`] wraps a `Stream<PyElement>` and the same runner slot, so
//!   `value()` reads back whichever stream you kept after the graph ran.
//!
//! Non-generic on purpose: this is the exact shape a `#[pyclass]` will expose
//! (pyclasses can't be generic), and it keeps every Python-composable edge on
//! the single erased [`PyElement`] type so any node wires to any node.
//!
//! Combinators that consume a *typed* input (e.g. [`filter`](PyStream::filter)
//! needs a `Stream<bool>`) convert at the seam — the erased surface never
//! leaks a concrete type to Python. A Python callable passed to
//! [`map`](PyStream::map) runs through [`try_map`](StreamOps::try_map) so a
//! raised exception aborts the run with context rather than panicking.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use pyo3::prelude::*;
use wingfoil::{RunFor, RunMode};
use wingfoil_next::interp::{Builder, Runner, SlotRef};
use wingfoil_next::op::{Activation, Ctx, Tick};
use wingfoil_next::prelude::{GraphBuilder, SourceOps, Stream, StreamOps, Upstream};

use crate::PyElement;

/// The runner produced by [`PyGraph::run`], shared by the graph and every
/// [`PyStream`] wired from it so `value()` works on whichever you kept.
type RunnerSlot = Rc<RefCell<Option<Runner>>>;

/// A held graph with the classic `run` / read-value ergonomics, erased to
/// [`PyElement`]. Clones share the same underlying builder and runner slot.
#[derive(Default, Clone)]
pub struct PyGraph {
    builder: GraphBuilder,
    runner: RunnerSlot,
}

impl PyGraph {
    /// A fresh, empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    fn wrap(&self, stream: Stream<PyElement>) -> PyStream {
        PyStream {
            stream,
            runner: self.runner.clone(),
        }
    }

    /// A source that ticks once with `value` on the first cycle.
    pub fn constant(&self, value: PyElement) -> PyStream {
        self.wrap(self.builder.constant(value))
    }

    /// A source that emits the running tick count `1, 2, 3, …` (as an integer
    /// [`PyElement`]) every `period`. Sugar over `ticker(period).count()`,
    /// giving Python a simple erased multi-tick source without exposing the
    /// `()`/`u64` interior types.
    pub fn counter(&self, period: Duration) -> PyStream {
        let counted = self.builder.ticker(period).count();
        self.wrap(counted.map(|n: &u64| PyElement::from(*n as i64)))
    }

    /// Run the graph to its bound, storing the runner so retained
    /// [`PyStream`]s can be read with [`PyStream::value`].
    ///
    /// The graph builds once (first run) and the runner is reused, so it may be
    /// run **repeatedly** when re-runnable — sources + combinators + feedback,
    /// the deterministic historical subset — each run first resetting every node
    /// to its wiring-time state so runs are independent (engine reset hook). A
    /// graph with single-run sources (`external`/`poll`/`channel`) errors on the
    /// second run, surfaced from the engine.
    pub fn run(&self, run_mode: RunMode, run_for: RunFor) -> Result<()> {
        let mut slot = self.runner.borrow_mut();
        if slot.is_none() {
            *slot = Some(self.builder.build());
        }
        slot.as_mut()
            .expect("runner set above")
            .run(run_mode, run_for)
    }

    /// Wire a **Python-defined custom node** — a Python object acting as a graph
    /// node, the object-form twin of the classic `CustomStream`
    /// (`MutableNode` + `StreamPeekRef`). This is the erased-boundary use of
    /// [`GraphBuilder::custom_node`]: the node is activated by its `upstreams`'
    /// ticks and, each activation, calls the Python object's protocol:
    ///
    /// - `cycle(values) -> bool` — invoked with the list of upstream current
    ///   values; returns whether the node ticked this cycle (the classic
    ///   `cycle() -> bool` decision). A not-yet-ticked upstream reads as Python
    ///   `None`.
    /// - `peek() -> value` — read only when `cycle` returned `True`, producing
    ///   the node's output value.
    ///
    /// Upstream values are read from their value slots captured at wiring time,
    /// so a custom node sees its inputs **without** re-entering the running
    /// graph (the runner is mutably borrowed for the duration of a run, so a
    /// Python `cycle` cannot read a sibling `PyStream.value()` — the values are
    /// handed in instead). A raised exception aborts the run with context.
    ///
    /// A graph containing a custom node is single-run (caller-owned Python state
    /// has no engine reset hook — see [`GraphBuilder::custom_node`]).
    pub fn custom_node(&self, upstreams: Vec<PyStream>, obj: Py<PyAny>) -> PyStream {
        // Capture each upstream's value slot at wiring time. Reading these
        // during a cycle touches only the slot cells, never the runner, so a
        // Python custom node reads its inputs with no re-entrancy hazard.
        let slots: Vec<SlotRef<PyElement>> =
            upstreams.iter().map(|s| s.stream.value_slot()).collect();
        let active: Vec<Upstream> = upstreams.iter().map(|s| s.stream.upstream()).collect();
        let stream = self.builder.custom_node::<PyElement, _>(
            &active,
            &[],
            Activation::NONE,
            move |_ctx: &mut Ctx<'_>| {
                Python::attach(|py| {
                    let values: Vec<Py<PyAny>> = slots
                        .iter()
                        .map(|slot| {
                            let element = slot.borrow();
                            if element.is_none() {
                                py.None()
                            } else {
                                element.object().clone_ref(py)
                            }
                        })
                        .collect();
                    let ticked: bool = obj
                        .call_method1(py, "cycle", (values,))
                        .map_err(|err| anyhow::anyhow!("Python custom node cycle raised: {err}"))?
                        .extract(py)
                        .map_err(|err| {
                            anyhow::anyhow!("Python custom node cycle must return bool: {err}")
                        })?;
                    if ticked {
                        let value = obj.call_method0(py, "peek").map_err(|err| {
                            anyhow::anyhow!("Python custom node peek raised: {err}")
                        })?;
                        Ok(Tick::Value(PyElement::new(value)))
                    } else {
                        Ok(Tick::Quiet)
                    }
                })
            },
        );
        self.wrap(stream)
    }
}

/// A stream in a [`PyGraph`], erased to [`PyElement`]. Combinators mirror the
/// fluent [`StreamOps`] surface; each returns a new `PyStream` on the same
/// graph.
#[derive(Clone)]
pub struct PyStream {
    stream: Stream<PyElement>,
    runner: RunnerSlot,
}

impl PyStream {
    fn wrap(&self, stream: Stream<PyElement>) -> PyStream {
        PyStream {
            stream,
            runner: self.runner.clone(),
        }
    }

    /// Apply a Python callable to each value. The callable is invoked with the
    /// current value and its result becomes the new value; a raised exception
    /// aborts the run with context (via [`try_map`](StreamOps::try_map)).
    pub fn map(&self, func: Py<PyAny>) -> PyStream {
        let mapped = self.stream.try_map(move |e: &PyElement| {
            Python::attach(|py| {
                let result = func
                    .call1(py, (e.value(),))
                    .map_err(|err| anyhow::anyhow!("Python map callable raised: {err}"))?;
                Ok(PyElement::new(result))
            })
        });
        self.wrap(mapped)
    }

    /// Emit only when `condition`'s current value is truthy. The condition is a
    /// [`PyStream`]; its Python value is extracted to `bool` at the seam so the
    /// public surface stays erased.
    pub fn filter(&self, condition: &PyStream) -> PyStream {
        let cond: Stream<bool> = condition.stream.try_map(|e: &PyElement| bool::try_from(e));
        self.wrap(self.stream.filter(&cond))
    }

    /// Merge with another stream; the earliest-supplied ticked input wins.
    pub fn merge(&self, other: &PyStream) -> PyStream {
        self.wrap(self.stream.merge(&other.stream))
    }

    /// Re-emit each value `delay` later.
    pub fn delay(&self, delay: Duration) -> PyStream {
        self.wrap(self.stream.delay(delay))
    }

    /// Suppress consecutive duplicate values (emit on change only).
    pub fn distinct(&self) -> PyStream {
        self.wrap(self.stream.distinct())
    }

    /// Emit the running tick count `1, 2, 3, …` (as an integer [`PyElement`]),
    /// ignoring the values themselves.
    pub fn count(&self) -> PyStream {
        let counted = self.stream.map(|_: &PyElement| ()).count();
        self.wrap(counted.map(|n: &u64| PyElement::from(*n as i64)))
    }

    /// Pass through the first `limit` values, then stay quiet.
    pub fn limit(&self, limit: u32) -> PyStream {
        self.wrap(self.stream.limit(limit))
    }

    /// Rate-limit: emit at most once per `interval`.
    pub fn throttle(&self, interval: Duration) -> PyStream {
        self.wrap(self.stream.throttle(interval))
    }

    /// Emit this stream's current value whenever `trigger` ticks (a passive
    /// read of the value; `trigger`'s own value is ignored).
    pub fn sample(&self, trigger: &PyStream) -> PyStream {
        let unit = trigger.stream.map(|_: &PyElement| ());
        self.wrap(self.stream.sample(&unit))
    }

    /// Emit the successive difference `value - previous` (quiet on the first
    /// value). Uses [`PyElement`]'s `Sub`, i.e. Python `__sub__`.
    pub fn difference(&self) -> PyStream {
        self.wrap(self.stream.difference())
    }

    /// Negate each value with [`PyElement`]'s `Not`, which maps to Python
    /// `__neg__` (arithmetic negation, e.g. `5 -> -5`) — matching the classic
    /// `not` node's `T: Not` semantics. Named `not` on the Python side.
    pub fn not_(&self) -> PyStream {
        self.wrap(self.stream.not())
    }

    /// Observe each value with a Python callable, passing it through unchanged
    /// (a debug tap). Routed through [`try_map`](StreamOps::try_map) so a raised
    /// exception aborts the run with context rather than panicking.
    pub fn inspect(&self, func: Py<PyAny>) -> PyStream {
        let observed = self.stream.try_map(move |e: &PyElement| {
            Python::attach(|py| {
                func.call1(py, (e.value(),))
                    .map_err(|err| anyhow::anyhow!("Python inspect callable raised: {err}"))?;
                Ok(e.clone())
            })
        });
        self.wrap(observed)
    }

    /// Wire a **single-input op** onto this stream at the erased boundary — the
    /// extensibility primitive third-party ops (and the `pyop!` macro) build
    /// on. The op computes on its own concrete types: the input is extracted
    /// from [`PyElement`] with `A: TryFrom<&PyElement>` and the output is boxed
    /// back with `Out: Into<PyElement>`, so only the edges convert while the
    /// interior stays natively typed.
    ///
    /// `cfg` is engine-owned construction-time config; `state_init` builds the
    /// op's mutable state (re-invoked on a graph reset, so re-runs start clean);
    /// `step` is the op's `cycle`. A `TryFrom`/`step` error aborts the run with
    /// context.
    pub fn wire_op1<A, C, S, Out, Step, SInit>(
        &self,
        label: &'static str,
        activation: Activation,
        cfg: C,
        state_init: SInit,
        mut step: Step,
    ) -> PyStream
    where
        A: for<'a> TryFrom<&'a PyElement, Error = anyhow::Error> + 'static,
        C: 'static,
        S: 'static,
        Out: Into<PyElement> + 'static,
        SInit: Fn() -> S + 'static,
        Step: FnMut(&mut C, &mut S, &A, &mut Ctx<'_>) -> Result<Tick<Out>> + 'static,
    {
        let wired = self.stream.wire(move |b: &mut Builder, h| {
            b.register_op1(
                h,
                label,
                activation,
                cfg,
                state_init,
                move |c, s, a: &PyElement, ctx| {
                    let input = A::try_from(a)?;
                    Ok(match step(c, s, &input, ctx)? {
                        Tick::Value(v) => Tick::Value(v.into()),
                        Tick::Silent(v) => Tick::Silent(v.into()),
                        Tick::Quiet => Tick::Quiet,
                    })
                },
            )
        });
        self.wrap(wired)
    }

    /// The stream's current value after [`PyGraph::run`].
    ///
    /// # Panics
    ///
    /// Panics if called before the owning graph has run — there is no value to
    /// read yet. This mirrors the classic infallible `peek_value`; the
    /// precondition is documented and enforced with an explanatory panic.
    pub fn value(&self) -> PyElement {
        self.runner
            .borrow()
            .as_ref()
            .expect("invariant: PyGraph::run must be called before PyStream::value")
            .value(&self.stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wingfoil::NanoTime;

    /// Build a Python callable from a `lambda` source string.
    fn lambda(src: &str) -> Py<PyAny> {
        Python::attach(|py| {
            py.eval(&std::ffi::CString::new(src).unwrap(), None, None)
                .unwrap()
                .unbind()
        })
    }

    /// Execute `src` (which must bind a name `obj`) and return that object —
    /// used to build a Python custom-node object with `cycle`/`peek` methods.
    fn py_object(src: &str) -> Py<PyAny> {
        use pyo3::types::PyDict;
        Python::attach(|py| {
            let globals = PyDict::new(py);
            py.run(&std::ffi::CString::new(src).unwrap(), Some(&globals), None)
                .unwrap();
            globals.get_item("obj").unwrap().unwrap().unbind()
        })
    }

    fn run_cycles(g: &PyGraph, n: u32) {
        g.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(n))
            .unwrap();
    }

    #[test]
    fn constant_maps_via_python_callable() {
        let g = PyGraph::new();
        let out = g
            .constant(PyElement::from(4.0_f64))
            .map(lambda("lambda x: x * x"));
        run_cycles(&g, 1);
        let v: f64 = (&out.value()).try_into().unwrap();
        assert_eq!(16.0, v);
    }

    #[test]
    fn count_ignores_values() {
        let g = PyGraph::new();
        // Values are strings; count only counts ticks.
        let counted = g
            .counter(Duration::from_nanos(100))
            .map(lambda("lambda n: 'x'"))
            .count();
        run_cycles(&g, 3);
        let v: i64 = (&counted.value()).try_into().unwrap();
        assert_eq!(3, v);
    }

    #[test]
    fn limit_caps_ticks() {
        let g = PyGraph::new();
        let capped = g.counter(Duration::from_nanos(100)).limit(2);
        run_cycles(&g, 5);
        // The value stays at the last value passed before the cap.
        let v: i64 = (&capped.value()).try_into().unwrap();
        assert_eq!(2, v);
    }

    #[test]
    fn difference_of_counter_is_one() {
        let g = PyGraph::new();
        let diff = g.counter(Duration::from_nanos(100)).difference();
        run_cycles(&g, 4);
        let v: i64 = (&diff.value()).try_into().unwrap();
        assert_eq!(1, v); // 1,2,3,4 -> deltas 1,1,1
    }

    #[test]
    fn not_negates_value() {
        let g = PyGraph::new();
        // `not` maps to __neg__ (arithmetic negation), matching the classic node.
        let negated = g.constant(PyElement::from(5_i64)).not_();
        run_cycles(&g, 1);
        let v: i64 = (&negated.value()).try_into().unwrap();
        assert_eq!(-5, v);
    }

    #[test]
    fn inspect_taps_and_passes_through() {
        let g = PyGraph::new();
        let tapped = g
            .counter(Duration::from_nanos(100))
            .inspect(lambda("lambda v: None"));
        run_cycles(&g, 3);
        // Passes the value through unchanged.
        let v: i64 = (&tapped.value()).try_into().unwrap();
        assert_eq!(3, v);
    }

    #[test]
    fn inspect_exception_aborts_run() {
        let g = PyGraph::new();
        let _tapped = g.counter(Duration::from_nanos(100)).inspect(lambda(
            "lambda v: (_ for _ in ()).throw(ValueError('boom'))",
        ));
        let err = g
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap_err();
        assert!(format!("{err:#}").contains("Python inspect callable raised"));
    }

    #[test]
    fn counter_source_ticks_and_reads_final() {
        let g = PyGraph::new();
        let doubled = g
            .counter(Duration::from_nanos(100))
            .map(lambda("lambda n: n * 2"));
        run_cycles(&g, 3);
        let v: i64 = (&doubled.value()).try_into().unwrap();
        assert_eq!(6, v); // 3rd tick -> 3 * 2
    }

    #[test]
    fn filter_by_python_predicate() {
        let g = PyGraph::new();
        let counter = g.counter(Duration::from_nanos(100));
        let gt2 = counter.map(lambda("lambda n: n > 2"));
        let filtered = counter.filter(&gt2);

        let seen = Rc::new(RefCell::new(Vec::<i64>::new()));
        let sink = seen.clone();
        let _observed = filtered.stream.inspect(move |e: &PyElement| {
            sink.borrow_mut().push(i64::try_from(e).unwrap());
        });

        run_cycles(&g, 5);
        assert_eq!(vec![3, 4, 5], *seen.borrow());
    }

    #[test]
    fn distinct_suppresses_consecutive_duplicates() {
        let g = PyGraph::new();
        // 1,2,3,4,5,6 -> n//2 -> 0,1,1,2,2,3 -> distinct -> 0,1,2,3
        let stepped = g
            .counter(Duration::from_nanos(100))
            .map(lambda("lambda n: n // 2"))
            .distinct();

        let seen = Rc::new(RefCell::new(Vec::<i64>::new()));
        let sink = seen.clone();
        let _observed = stepped.stream.inspect(move |e: &PyElement| {
            sink.borrow_mut().push(i64::try_from(e).unwrap());
        });

        run_cycles(&g, 6);
        assert_eq!(vec![0, 1, 2, 3], *seen.borrow());
    }

    #[test]
    fn map_callable_error_aborts_run() {
        let g = PyGraph::new();
        let out = g
            .constant(PyElement::from("not a number"))
            .map(lambda("lambda x: x + 1")); // str + int raises TypeError
        let err = g
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap_err();
        assert!(format!("{err:#}").contains("Python map callable raised"));
        let _ = out;
    }

    /// A Python object acting as a graph node: sums its two upstream counters
    /// each cycle. Exercises the `cycle(values) -> bool` + `peek()` protocol and
    /// that upstream values are handed in correctly.
    #[test]
    fn python_custom_node_sums_upstreams() {
        let g = PyGraph::new();
        let a = g.counter(Duration::from_nanos(100));
        let b = g.counter(Duration::from_nanos(100));
        let adder = py_object(
            "class Adder:\n\
            \x20   def __init__(self): self.total = 0\n\
            \x20   def cycle(self, values): self.total = sum(values); return True\n\
            \x20   def peek(self): return self.total\n\
            obj = Adder()",
        );
        let summed = g.custom_node(vec![a, b], adder);

        let seen = Rc::new(RefCell::new(Vec::<i64>::new()));
        let sink = seen.clone();
        let _observed = summed.stream.inspect(move |e: &PyElement| {
            sink.borrow_mut().push(i64::try_from(e).unwrap());
        });

        run_cycles(&g, 3);
        // Both counters tick together: 1+1, 2+2, 3+3.
        assert_eq!(vec![2, 4, 6], *seen.borrow());
    }

    /// A custom node returning `False` from `cycle` stays quiet that cycle —
    /// the classic "did I tick?" decision. Emits only on even counter values.
    #[test]
    fn python_custom_node_can_stay_quiet() {
        let g = PyGraph::new();
        let counter = g.counter(Duration::from_nanos(100));
        let evens = py_object(
            "class Evens:\n\
            \x20   def __init__(self): self.v = 0\n\
            \x20   def cycle(self, values):\n\
            \x20       self.v = values[0]\n\
            \x20       return self.v % 2 == 0\n\
            \x20   def peek(self): return self.v\n\
            obj = Evens()",
        );
        let filtered = g.custom_node(vec![counter], evens);

        let seen = Rc::new(RefCell::new(Vec::<i64>::new()));
        let sink = seen.clone();
        let _observed = filtered.stream.inspect(move |e: &PyElement| {
            sink.borrow_mut().push(i64::try_from(e).unwrap());
        });

        run_cycles(&g, 6);
        assert_eq!(vec![2, 4, 6], *seen.borrow());
    }

    /// An exception raised inside a Python custom node's `cycle` aborts the run
    /// with context, like a raising `map` callable.
    #[test]
    fn python_custom_node_error_aborts_run() {
        let g = PyGraph::new();
        let counter = g.counter(Duration::from_nanos(100));
        let boom = py_object(
            "class Boom:\n\
            \x20   def cycle(self, values): raise ValueError('boom')\n\
            \x20   def peek(self): return 0\n\
            obj = Boom()",
        );
        let node = g.custom_node(vec![counter], boom);
        let err = g
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap_err();
        assert!(format!("{err:#}").contains("Python custom node cycle raised"));
        let _ = node;
    }

    #[test]
    fn graph_reruns_and_resets() {
        let g = PyGraph::new();
        let out = g
            .constant(PyElement::from(2.0_f64))
            .map(lambda("lambda x: x + 1"));
        run_cycles(&g, 1);
        let first: f64 = (&out.value()).try_into().unwrap();
        // Re-run: the engine resets every node to its wiring-time state, so a
        // re-runnable graph reproduces the same values.
        run_cycles(&g, 1);
        let second: f64 = (&out.value()).try_into().unwrap();
        assert_eq!(3.0, first);
        assert_eq!(first, second);
    }
}
