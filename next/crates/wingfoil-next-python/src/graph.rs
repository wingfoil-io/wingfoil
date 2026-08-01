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
use pyo3::IntoPyObject;
use pyo3::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::interp::{Builder, Handle, Runner, SlotRef};
use wingfoil_next::op::{Activation, Ctx, Tick};
use wingfoil_next::prelude::{Burst, GraphBuilder, SourceOps, Stream, StreamOps, Upstream};
use wingfoil_next::stats::StatisticsOps;

use crate::PyElement;

/// The runner produced by [`PyGraph::run`], shared by the graph and every
/// [`PyStream`] wired from it so `value()` works on whichever you kept.
type RunnerSlot = Rc<RefCell<Option<Runner>>>;

/// A raw pointer marked `Send` purely to satisfy the bound on
/// [`Python::detach`], which requires a `Send` closure even though it runs that
/// closure in place on the calling thread. See the safety note in
/// [`PyGraph::run`], the only user.
struct SendPtr<T>(*const T);

impl<T> SendPtr<T> {
    /// The wrapped pointer. An accessor rather than a field read because
    /// edition-2024 closures capture disjoint *fields*: reading `.0` directly
    /// would capture the bare (non-`Send`) pointer instead of this wrapper.
    fn get(&self) -> *const T {
        self.0
    }
}

// SAFETY: the pointer is never dereferenced on, or sent to, another thread —
// `detach` releases the GIL around an in-place call. See `PyGraph::run`.
unsafe impl<T> Send for SendPtr<T> {}

/// Box a `(time, value)` pair into a Python `(nanos, value)` tuple element —
/// the edge conversion shared by `with_time` and `collect` (nanoseconds as an
/// int).
fn time_value_tuple(time: NanoTime, value: &PyElement) -> PyElement {
    Python::attach(|py| {
        let nanos = u64::from(time) as i64;
        let tuple = (nanos, value.value())
            .into_pyobject(py)
            .expect("invariant: (i64, PyObject) is always tuple-convertible");
        PyElement::new(tuple.into_any().unbind())
    })
}

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

    /// A source that replays a finite sequence of Python values, one per tick,
    /// `period` apart (the first at t=0). This is how a Python caller feeds real
    /// data into a graph rather than a synthetic `counter`/`constant`.
    ///
    /// Built on the historical-replay [`channel`](SourceOps::channel) layer, so
    /// it replays **deterministically** in historical mode and a graph
    /// containing it is single-run (the producer channel is consumed by the
    /// first run). Distinct per-tick timestamps mean each value rides its own
    /// cycle (no same-instant grouping).
    pub fn values(&self, values: Vec<PyElement>, period: Duration) -> PyStream {
        let step = period.as_nanos() as u64;
        let rows = values
            .into_iter()
            .enumerate()
            .map(move |(i, value)| Ok((value, NanoTime::from(i as u64 * step))));
        let bursts = self.builder.replay_results(rows);
        // Each burst holds exactly one value (timestamps are distinct); take it.
        self.wrap(
            bursts.map(|burst: &Burst<PyElement>| {
                burst.last().cloned().unwrap_or_else(PyElement::none)
            }),
        )
    }

    /// The underlying fluent [`GraphBuilder`] — the seam a `#[pyadapter]` source
    /// method wires onto. The adapter runs its native wiring against this
    /// builder (splicing into the same graph), then the typed result is erased
    /// with [`erase_source`](Self::erase_source).
    pub fn builder(&self) -> &GraphBuilder {
        &self.builder
    }

    /// Erase a natively-typed source `Stream<T>` to a [`PyStream`] on this graph
    /// — the output half of the `#[pyadapter]` source seam (the interior stays
    /// native `T`; only the Python-facing edge erases).
    pub fn erase_source<T>(&self, typed: Stream<T>) -> PyStream
    where
        T: Clone + Into<PyElement> + 'static,
    {
        self.wrap(typed.map(|v: &T| v.clone().into()))
    }

    /// Erase a `Stream<Burst<T>>` source to a [`PyStream`] — each burst (the
    /// same-instant group) becomes a Python `list` of its erased values. The
    /// burst counterpart of [`erase_source`](Self::erase_source), for adapters
    /// whose source produces the burst shape (`$name_read`/`$name_sub`).
    pub fn erase_burst_source<T>(&self, typed: Stream<Burst<T>>) -> PyStream
    where
        T: Clone + Default + Into<PyElement> + 'static,
    {
        self.wrap(typed.map(|burst: &Burst<T>| {
            let items: Vec<PyElement> = burst.iter().map(|v| v.clone().into()).collect();
            PyElement::list(&items)
        }))
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
    ///
    /// **The GIL is released for the duration of the run** (re-acquired per
    /// Python callback via `Python::attach`). Without that, a real-time adapter
    /// source could not deliver — its worker thread, and every other Python
    /// thread, would be blocked until `run` returned, so a live tail would only
    /// ever see rows that already existed when it started. This mirrors legacy
    /// `wingfoil-python`, whose `run` releases the GIL for the same reason.
    pub fn run(&self, run_mode: RunMode, run_for: RunFor) -> Result<()> {
        // Build outside the detached section: construction touches no Python.
        {
            let mut slot = self.runner.borrow_mut();
            if slot.is_none() {
                *slot = Some(self.builder.build());
            }
        }
        let runner = SendPtr(Rc::as_ptr(&self.runner));
        Python::attach(|py| {
            py.detach(move || {
                // SAFETY: `detach` runs its closure **in place on this thread** —
                // it only releases the GIL for the closure's duration, it does
                // not move the work elsewhere. Its `Send` bound is therefore
                // conservative, and the pointee is kept alive for the whole call
                // by `self`, which outlives it. Nothing else can reach the slot:
                // `PyGraph` is `Rc`-based and its pyclass is `unsendable`, so it
                // is pinned to this thread.
                let slot = unsafe { &*runner.get() };
                slot.borrow_mut()
                    .as_mut()
                    .expect("invariant: runner built above")
                    .run(run_mode, run_for)
            })
        })
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

    /// Merge with several other streams at once (the classic n-ary `merge`); on
    /// any tick the earliest-supplied ticked input wins. Equivalent to a chain
    /// of 2-ary [`merge`](Self::merge)s.
    pub fn merge_all(&self, others: &[PyStream]) -> PyStream {
        let refs: Vec<&Stream<PyElement>> = others.iter().map(|s| &s.stream).collect();
        self.wrap(self.stream.merge_all(&refs))
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

    /// Print each value (`{value:?}`) to stdout as it ticks, passing it through
    /// unchanged (the classic `print` debug tap). Prints per tick rather than
    /// buffering to teardown — see the `Print` op's deviation note.
    pub fn print(&self) -> PyStream {
        self.wrap(self.stream.print())
    }

    /// Log each value (`"{time} {label} {value:?}"` at `level`, via the `log`
    /// crate) as it ticks, passing it through unchanged (the classic `logged`
    /// debug tap). Wire up any `log` backend (e.g. Python `logging` bridged in,
    /// or `env_logger`) to see the output.
    pub fn logged(&self, label: &str, level: log::Level) -> PyStream {
        self.wrap(self.stream.logged(label, level))
    }

    /// Collect every emitted value into a growing Python `list`, re-emitted each
    /// tick (the classic `accumulate`).
    pub fn accumulate(&self) -> PyStream {
        let acc = self
            .stream
            .accumulate()
            .map(|items: &Vec<PyElement>| PyElement::list(items));
        self.wrap(acc)
    }

    /// Buffer values and flush them as a Python `list` once `capacity`
    /// accumulate (and once more on the last cycle).
    pub fn buffer(&self, capacity: usize) -> PyStream {
        let buffered = self
            .stream
            .buffer(capacity)
            .map(|items: &Vec<PyElement>| PyElement::list(items));
        self.wrap(buffered)
    }

    /// Buffer values and flush them as a Python `list` on each `interval`
    /// boundary (and once more on the last cycle).
    pub fn window(&self, interval: Duration) -> PyStream {
        let windowed = self
            .stream
            .window(interval)
            .map(|items: &Vec<PyElement>| PyElement::list(items));
        self.wrap(windowed)
    }

    /// Pair each value with the current engine time as a Python `(nanos, value)`
    /// tuple (the classic `with_time`, nanoseconds as an int).
    pub fn with_time(&self) -> PyStream {
        let timed = self
            .stream
            .with_time()
            .map(|(time, value): &(NanoTime, PyElement)| time_value_tuple(*time, value));
        self.wrap(timed)
    }

    /// Collect every `(nanos, value)` pair into a growing Python `list` of
    /// tuples, re-emitted each tick (the classic `collect` — value + time,
    /// what `dataframe` builds on).
    pub fn collect(&self) -> PyStream {
        let collected =
            self.stream
                .with_time()
                .accumulate()
                .map(|rows: &Vec<(NanoTime, PyElement)>| {
                    let tuples: Vec<PyElement> = rows
                        .iter()
                        .map(|(time, value)| time_value_tuple(*time, value))
                        .collect();
                    PyElement::list(&tuples)
                });
        self.wrap(collected)
    }

    /// Fold values into an accumulator with a Python callable, emitting the
    /// accumulator after each fold (the classic `fold`). `func(acc, value)`
    /// returns the new accumulator, seeded from `init`. The accumulator is
    /// **engine-owned state** re-seeded from `init` on a graph reset, so a
    /// re-run restarts the fold (it does not continue). A raised exception
    /// aborts the run with context.
    pub fn fold(&self, init: PyElement, func: Py<PyAny>) -> PyStream {
        let folded = self.stream.wire(move |b: &mut Builder, h| {
            b.register_op1(
                h,
                "fold",
                Activation::NONE,
                func,                 // cfg: the Python reducer
                move || init.clone(), // state factory: accumulator seeded to init (resets on re-run)
                move |func: &mut Py<PyAny>, acc: &mut PyElement, value: &PyElement, _ctx| {
                    Python::attach(|py| {
                        let next = func
                            .call1(py, (acc.value(), value.value()))
                            .map_err(|err| anyhow::anyhow!("Python fold callable raised: {err}"))?;
                        *acc = PyElement::new(next);
                        Ok(Tick::Value(acc.clone()))
                    })
                },
            )
        });
        self.wrap(folded)
    }

    /// Map-and-filter with a Python callable (the classic `filter_map`):
    /// `func(value)` returning Python `None` drops the tick, any other result
    /// is emitted. A raised exception aborts the run with context.
    pub fn filter_map(&self, func: Py<PyAny>) -> PyStream {
        self.wire_stateless("filter_map", move |value| {
            Python::attach(|py| {
                let result = func
                    .call1(py, (value.value(),))
                    .map_err(|err| anyhow::anyhow!("Python filter_map callable raised: {err}"))?;
                Ok(if result.is_none(py) {
                    Tick::Quiet
                } else {
                    Tick::Value(PyElement::new(result))
                })
            })
        })
    }

    /// Keep a value only when a Python predicate returns truthy (the classic
    /// `filter_value`); drop it otherwise. A raised exception aborts the run.
    pub fn filter_value(&self, predicate: Py<PyAny>) -> PyStream {
        self.wire_stateless("filter_value", move |value| {
            Python::attach(|py| {
                let keep = predicate
                    .call1(py, (value.value(),))
                    .map_err(|err| anyhow::anyhow!("Python filter_value predicate raised: {err}"))?
                    .is_truthy(py)
                    .map_err(|err| anyhow::anyhow!("Python filter_value predicate: {err}"))?;
                Ok(if keep {
                    Tick::Value(value.clone())
                } else {
                    Tick::Quiet
                })
            })
        })
    }

    /// Drop values whose payload is Python `None`, passing everything else
    /// through unchanged (the classic `filter_none`).
    pub fn filter_none(&self) -> PyStream {
        self.wire_stateless("filter_none", |value| {
            let is_py_none = Python::attach(|py| value.object().is_none(py));
            Ok(if is_py_none {
                Tick::Quiet
            } else {
                Tick::Value(value.clone())
            })
        })
    }

    /// Cumulative running **sum** over the values (the classic `sum`,
    /// `Window::Unbounded`). Each value is read as `f64` at the edge — a
    /// non-numeric value aborts the run with context — and the running total is
    /// re-boxed as a float [`PyElement`].
    pub fn sum(&self) -> PyStream {
        let as_f64 = self.stream.try_map(|e: &PyElement| f64::try_from(e));
        self.wrap(as_f64.cumulative_sum().map(|v: &f64| PyElement::from(*v)))
    }

    /// Cumulative running **mean** over the values (the classic `mean` /
    /// `average`, `Window::Unbounded`, count-weighted). Values are read as `f64`
    /// at the edge (a non-numeric value aborts the run) and the running mean is
    /// re-boxed as a float [`PyElement`].
    pub fn mean(&self) -> PyStream {
        let as_f64 = self.stream.try_map(|e: &PyElement| f64::try_from(e));
        self.wrap(as_f64.cumulative_mean().map(|v: &f64| PyElement::from(*v)))
    }

    /// Combine this stream with `other` through a Python callable (the classic
    /// `bimap`): whenever either input ticks, `func(this_value, other_value)` is
    /// called with both inputs' current values and its result is emitted. Both
    /// inputs are active. A raised exception aborts the run with context — so
    /// this one method also covers the classic `try_bimap` (a Python callable
    /// always propagates its exception).
    pub fn bimap(&self, other: &PyStream, func: Py<PyAny>) -> PyStream {
        let other_handle = other.stream.handle();
        let joined = self
            .stream
            .wire(move |b: &mut Builder, this: Handle<PyElement>| {
                b.register_op2(
                    this,
                    other_handle,
                    "bimap",
                    Activation::NONE,
                    func, // cfg: the Python combiner
                    || (),
                    move |func: &mut Py<PyAny>,
                          _state: &mut (),
                          a: &PyElement,
                          b: &PyElement,
                          _ctx| {
                        Python::attach(|py| {
                            let result = func.call1(py, (a.value(), b.value())).map_err(|err| {
                                anyhow::anyhow!("Python bimap callable raised: {err}")
                            })?;
                            Ok(Tick::Value(PyElement::new(result)))
                        })
                    },
                )
            });
        self.wrap(joined)
    }

    /// Build a pandas `DataFrame` (columns `time`, `value`) from every emitted
    /// value paired with its engine time (the classic `dataframe`). The frame is
    /// assembled **once, on the last cycle**, so the stream's final value is the
    /// completed `DataFrame`; earlier cycles stay quiet. Rows are engine-owned
    /// state re-seeded on a graph reset, so a re-run rebuilds cleanly.
    ///
    /// `pandas` is imported lazily here — only a graph that actually calls
    /// `dataframe` needs it; if it is not importable the run aborts with context.
    pub fn dataframe(&self) -> PyStream {
        let framed = self.stream.with_time().wire(|b: &mut Builder, h| {
            b.register_op1(
                h,
                "dataframe",
                Activation::NONE,
                (),
                Vec::<(i64, Py<PyAny>)>::new, // state: accumulated (nanos, value) rows
                move |_cfg: &mut (),
                      rows: &mut Vec<(i64, Py<PyAny>)>,
                      (time, value): &(NanoTime, PyElement),
                      ctx| {
                    Python::attach(|py| {
                        rows.push((u64::from(*time) as i64, value.object().clone_ref(py)));
                        if !ctx.is_last_cycle() {
                            return Ok(Tick::Quiet);
                        }
                        let pandas = py
                            .import("pandas")
                            .map_err(|err| anyhow::anyhow!("dataframe() needs pandas: {err}"))?;
                        let times: Vec<i64> = rows.iter().map(|(t, _)| *t).collect();
                        let values: Vec<Py<PyAny>> =
                            rows.iter().map(|(_, v)| v.clone_ref(py)).collect();
                        let columns = pyo3::types::PyDict::new(py);
                        columns
                            .set_item("time", times)
                            .and_then(|()| columns.set_item("value", values))
                            .map_err(|err| anyhow::anyhow!("dataframe() column build: {err}"))?;
                        let frame = pandas
                            .call_method1("DataFrame", (columns,))
                            .map_err(|err| anyhow::anyhow!("pandas.DataFrame raised: {err}"))?;
                        Ok(Tick::Value(PyElement::new(frame.unbind())))
                    })
                },
            )
        });
        self.wrap(framed)
    }

    /// Reduce values with a Python callable, emitting the running result (the
    /// classic `reduce`). The **first** value seeds the accumulator and is
    /// emitted as-is; each later value emits `func(acc, value)`
    /// (functools.reduce-style, no explicit initial). The accumulator is
    /// engine-owned state re-seeded on a graph reset, so a re-run restarts. A
    /// raised exception aborts the run with context.
    pub fn reduce(&self, func: Py<PyAny>) -> PyStream {
        let reduced = self.stream.wire(move |b: &mut Builder, h| {
            b.register_op1(
                h,
                "reduce",
                Activation::NONE,
                func,                         // cfg: the Python reducer
                || Option::<PyElement>::None, // state: accumulator, empty until the first value
                move |func: &mut Py<PyAny>,
                      acc: &mut Option<PyElement>,
                      value: &PyElement,
                      _ctx| {
                    match acc.take() {
                        None => {
                            *acc = Some(value.clone());
                            Ok(Tick::Value(value.clone()))
                        }
                        Some(current) => Python::attach(|py| {
                            let next = func.call1(py, (current.value(), value.value())).map_err(
                                |err| anyhow::anyhow!("Python reduce callable raised: {err}"),
                            )?;
                            let next = PyElement::new(next);
                            *acc = Some(next.clone());
                            Ok(Tick::Value(next))
                        }),
                    }
                },
            )
        });
        self.wrap(reduced)
    }

    /// Decompose a stream of 2-tuples into its two component streams (the
    /// classic `split`); both branches tick whenever the source does. Reading a
    /// non-indexable value aborts the run with context.
    pub fn split(&self) -> (PyStream, PyStream) {
        (self.item(0), self.item(1))
    }

    /// Project index `i` out of each (indexable) value — the per-branch half of
    /// [`split`](Self::split).
    fn item(&self, i: usize) -> PyStream {
        self.wire_stateless("split", move |value| {
            Python::attach(|py| {
                let item = value.object().bind(py).get_item(i).map_err(|err| {
                    anyhow::anyhow!("Python split: value is not indexable at {i}: {err}")
                })?;
                Ok(Tick::Value(PyElement::new(item.unbind())))
            })
        })
    }

    /// Wire a **stateless, fallible** single-input op that maps each value to a
    /// [`Tick`] — the shared plumbing for `filter_map`/`filter_value`/
    /// `filter_none`. Runs over the engine's `register_op1` so a returned `Err`
    /// aborts the run with node context; carries no state, so it re-runs cleanly.
    fn wire_stateless<F>(&self, label: &'static str, mut step: F) -> PyStream
    where
        F: FnMut(&PyElement) -> Result<Tick<PyElement>> + 'static,
    {
        let wired = self.stream.wire(move |b: &mut Builder, h| {
            b.register_op1(
                h,
                label,
                Activation::NONE,
                (),
                || (),
                move |_cfg: &mut (), _state: &mut (), value: &PyElement, _ctx| step(value),
            )
        });
        self.wrap(wired)
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

    /// Wire a **two-input op** onto this stream and `other` at the erased
    /// boundary — the two-active-input counterpart of [`wire_op1`](Self::wire_op1)
    /// (the `#[pyop]` seam for `In<'a> = (&'a A, &'a B)` ops). Both inputs are
    /// extracted from [`PyElement`] (`A`/`B: TryFrom`), both trigger the op, and
    /// the output is boxed back to `PyElement`; a `TryFrom`/`step` error aborts
    /// the run with context.
    #[allow(clippy::too_many_arguments)]
    pub fn wire_op2<A, B, C, S, Out, Step, SInit>(
        &self,
        other: &PyStream,
        label: &'static str,
        activation: Activation,
        cfg: C,
        state_init: SInit,
        mut step: Step,
    ) -> PyStream
    where
        A: for<'a> TryFrom<&'a PyElement, Error = anyhow::Error> + 'static,
        B: for<'a> TryFrom<&'a PyElement, Error = anyhow::Error> + 'static,
        C: 'static,
        S: 'static,
        Out: Into<PyElement> + 'static,
        SInit: Fn() -> S + 'static,
        Step: FnMut(&mut C, &mut S, &A, &B, &mut Ctx<'_>) -> Result<Tick<Out>> + 'static,
    {
        let other_handle = other.stream.handle();
        let wired = self
            .stream
            .wire(move |b: &mut Builder, this: Handle<PyElement>| {
                b.register_op2(
                    this,
                    other_handle,
                    label,
                    activation,
                    cfg,
                    state_init,
                    move |c, s, a: &PyElement, bb: &PyElement, ctx| {
                        let input_a = A::try_from(a)?;
                        let input_b = B::try_from(bb)?;
                        Ok(match step(c, s, &input_a, &input_b, ctx)? {
                            Tick::Value(v) => Tick::Value(v.into()),
                            Tick::Silent(v) => Tick::Silent(v.into()),
                            Tick::Quiet => Tick::Quiet,
                        })
                    },
                )
            });
        self.wrap(wired)
    }

    /// Wire a **three-input op** onto this stream, `second` and `third` at the
    /// erased boundary — the three-active-input counterpart of
    /// [`wire_op2`](Self::wire_op2) (the `#[pyop]` seam for
    /// `In<'a> = (&'a A, &'a B, &'a C)` ops, the `join3` shape). All three
    /// inputs are extracted from [`PyElement`], all three trigger the op, and
    /// the output is boxed back; a `TryFrom`/`step` error aborts the run with
    /// context.
    #[allow(clippy::too_many_arguments)]
    pub fn wire_op3<A, B, C, Cfg, S, Out, Step, SInit>(
        &self,
        second: &PyStream,
        third: &PyStream,
        label: &'static str,
        activation: Activation,
        cfg: Cfg,
        state_init: SInit,
        mut step: Step,
    ) -> PyStream
    where
        A: for<'a> TryFrom<&'a PyElement, Error = anyhow::Error> + 'static,
        B: for<'a> TryFrom<&'a PyElement, Error = anyhow::Error> + 'static,
        C: for<'a> TryFrom<&'a PyElement, Error = anyhow::Error> + 'static,
        Cfg: 'static,
        S: 'static,
        Out: Into<PyElement> + 'static,
        SInit: Fn() -> S + 'static,
        Step: FnMut(&mut Cfg, &mut S, &A, &B, &C, &mut Ctx<'_>) -> Result<Tick<Out>> + 'static,
    {
        let second_handle = second.stream.handle();
        let third_handle = third.stream.handle();
        let wired = self
            .stream
            .wire(move |b: &mut Builder, this: Handle<PyElement>| {
                b.register_op3(
                    this,
                    second_handle,
                    third_handle,
                    label,
                    activation,
                    cfg,
                    state_init,
                    move |c, s, a: &PyElement, bb: &PyElement, cc: &PyElement, ctx| {
                        let input_a = A::try_from(a)?;
                        let input_b = B::try_from(bb)?;
                        let input_c = C::try_from(cc)?;
                        Ok(match step(c, s, &input_a, &input_b, &input_c, ctx)? {
                            Tick::Value(v) => Tick::Value(v.into()),
                            Tick::Silent(v) => Tick::Silent(v.into()),
                            Tick::Quiet => Tick::Quiet,
                        })
                    },
                )
            });
        self.wrap(wired)
    }

    /// Extract this erased stream to a natively-typed `Stream<T>` — the input
    /// half of the `#[pygraph]` seam. Each value is converted from [`PyElement`]
    /// via `T: TryFrom<&PyElement>` at the boundary (a conversion error aborts
    /// the run), so a Rust-authored sub-graph runs over concrete `T` internally
    /// while Python only ever sees the erased edge. Splices onto the caller's
    /// builder (same graph) like any other combinator.
    pub fn typed_input<T>(&self) -> Stream<T>
    where
        T: for<'a> TryFrom<&'a PyElement, Error = anyhow::Error> + Clone + Default + 'static,
    {
        self.stream.try_map(|e: &PyElement| T::try_from(e))
    }

    /// Box a natively-typed `Stream<U>` (a `#[pygraph]` sub-graph's output) back
    /// to the erased boundary as a [`PyStream`] on this graph — the output half
    /// of the seam.
    pub fn erased_output<U>(&self, typed: Stream<U>) -> PyStream
    where
        U: Clone + Into<PyElement> + 'static,
    {
        self.wrap(typed.map(|u: &U| u.clone().into()))
    }

    /// Extract this erased stream to a `Stream<Burst<T>>` — the input half of
    /// the **burst** adapter seam. Each erased value becomes one burst
    /// (`T: TryFrom<&PyElement>`); a conversion error aborts the run:
    ///
    /// - a Python **`list`/`tuple`** becomes a **multi-value** burst, one member
    ///   per item — so a burst *source*'s per-tick list (from
    ///   [`erase_burst_source`](Self::erase_burst_source)) round-trips back into
    ///   a multi-value burst;
    /// - any other value becomes a **single-element** burst — so a plain Python
    ///   stream of scalars also feeds a burst-shaped sink.
    pub fn typed_burst_input<T>(&self) -> Stream<Burst<T>>
    where
        T: for<'a> TryFrom<&'a PyElement, Error = anyhow::Error> + Clone + Default + 'static,
    {
        use pyo3::prelude::PyAnyMethods;
        use pyo3::types::{PyList, PyTuple};
        self.stream.try_map(|e: &PyElement| {
            Python::attach(|py| {
                let obj = e.object().bind(py);
                let mut burst: Burst<T> = Burst::default();
                // A list/tuple is a same-instant group -> a multi-value burst.
                // (Checked explicitly, so a `str` — itself a Python sequence —
                // stays a single scalar value.)
                if obj.is_instance_of::<PyList>() || obj.is_instance_of::<PyTuple>() {
                    let items: Vec<Py<PyAny>> = obj
                        .extract()
                        .map_err(|err| anyhow::anyhow!("burst input: reading list/tuple: {err}"))?;
                    for item in items {
                        burst.push(T::try_from(&PyElement::new(item))?);
                    }
                } else {
                    burst.push(T::try_from(e)?);
                }
                Ok(burst)
            })
        })
    }

    /// Box a `Stream<Burst<U>>` back to the erased boundary — each burst becomes
    /// a Python `list` of its erased values. The burst counterpart of
    /// [`erased_output`](Self::erased_output).
    pub fn erased_burst_output<U>(&self, typed: Stream<Burst<U>>) -> PyStream
    where
        U: Clone + Default + Into<PyElement> + 'static,
    {
        self.wrap(typed.map(|burst: &Burst<U>| {
            let items: Vec<PyElement> = burst.iter().map(|v| v.clone().into()).collect();
            PyElement::list(&items)
        }))
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
    fn accumulate_grows_a_list() {
        let g = PyGraph::new();
        let acc = g.counter(Duration::from_nanos(100)).accumulate();
        run_cycles(&g, 3);
        let v: Vec<i64> = Python::attach(|py| acc.value().value().extract(py).unwrap());
        assert_eq!(vec![1, 2, 3], v);
    }

    #[test]
    fn buffer_flushes_a_list_at_capacity() {
        let g = PyGraph::new();
        let buffered = g.counter(Duration::from_nanos(100)).buffer(2);
        run_cycles(&g, 4);
        // Last full flush is [3, 4].
        let v: Vec<i64> = Python::attach(|py| buffered.value().value().extract(py).unwrap());
        assert_eq!(vec![3, 4], v);
    }

    #[test]
    fn with_time_pairs_nanos_and_value() {
        let g = PyGraph::new();
        let timed = g.counter(Duration::from_nanos(100)).with_time();
        run_cycles(&g, 3);
        // Ticks fire at t=0,100,200 — 3rd tick is value 3 at t=200.
        let pair: (i64, i64) = Python::attach(|py| timed.value().value().extract(py).unwrap());
        assert_eq!((200, 3), pair);
    }

    #[test]
    fn collect_gathers_time_value_tuples() {
        let g = PyGraph::new();
        let collected = g.counter(Duration::from_nanos(100)).collect();
        run_cycles(&g, 2);
        let rows: Vec<(i64, i64)> =
            Python::attach(|py| collected.value().value().extract(py).unwrap());
        assert_eq!(vec![(0, 1), (100, 2)], rows);
    }

    #[test]
    fn fold_accumulates_and_resets_on_rerun() {
        let g = PyGraph::new();
        let summed = g
            .counter(Duration::from_nanos(100))
            .fold(PyElement::from(0_i64), lambda("lambda acc, v: acc + v"));
        run_cycles(&g, 3);
        let first: i64 = (&summed.value()).try_into().unwrap();
        assert_eq!(6, first); // 1+2+3
        // Re-run restarts the fold (engine re-seeds the accumulator), not continue.
        run_cycles(&g, 3);
        let second: i64 = (&summed.value()).try_into().unwrap();
        assert_eq!(6, second);
    }

    #[test]
    fn fold_exception_aborts_run() {
        let g = PyGraph::new();
        let _bad = g.counter(Duration::from_nanos(100)).fold(
            PyElement::from(0_i64),
            lambda("lambda acc, v: (_ for _ in ()).throw(ValueError())"),
        );
        let err = g
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap_err();
        assert!(format!("{err:#}").contains("Python fold callable raised"));
    }

    #[test]
    fn filter_map_keeps_non_none() {
        let g = PyGraph::new();
        // Keep even counts, scaled; drop odd (None).
        let kept = g
            .counter(Duration::from_nanos(100))
            .filter_map(lambda("lambda n: n * 10 if n % 2 == 0 else None"));
        run_cycles(&g, 4);
        let v: i64 = (&kept.value()).try_into().unwrap();
        assert_eq!(40, v); // last kept: 4 -> 40
    }

    #[test]
    fn filter_value_keeps_on_predicate() {
        let g = PyGraph::new();
        let kept = g
            .counter(Duration::from_nanos(100))
            .filter_value(lambda("lambda n: n > 2"));
        run_cycles(&g, 5);
        let v: i64 = (&kept.value()).try_into().unwrap();
        assert_eq!(5, v); // 3,4,5 pass; last is 5
    }

    #[test]
    fn filter_none_drops_python_none() {
        let g = PyGraph::new();
        let kept = g
            .counter(Duration::from_nanos(100))
            .map(lambda("lambda n: n if n % 2 == 0 else None"))
            .filter_none();
        run_cycles(&g, 6);
        let v: i64 = (&kept.value()).try_into().unwrap();
        assert_eq!(6, v); // 2,4,6 pass; last is 6
    }

    #[test]
    fn dataframe_builds_on_last_cycle() {
        // Skip when pandas isn't installed (parity with pytest's importorskip),
        // so this test doesn't fail in a bare CI environment.
        if Python::attach(|py| py.import("pandas").is_err()) {
            return;
        }
        let g = PyGraph::new();
        let df = g.counter(Duration::from_nanos(100)).dataframe();
        run_cycles(&g, 3);
        // The final value is a pandas DataFrame with time/value columns.
        let (times, values): (Vec<i64>, Vec<i64>) = Python::attach(|py| {
            let frame = df.value().value();
            let bound = frame.bind(py);
            let t = bound
                .call_method1("__getitem__", ("time",))
                .unwrap()
                .call_method0("tolist")
                .unwrap()
                .extract()
                .unwrap();
            let v = bound
                .call_method1("__getitem__", ("value",))
                .unwrap()
                .call_method0("tolist")
                .unwrap()
                .extract()
                .unwrap();
            (t, v)
        });
        assert_eq!(vec![0, 100, 200], times);
        assert_eq!(vec![1, 2, 3], values);
    }

    #[test]
    fn reduce_runs_from_first_value() {
        let g = PyGraph::new();
        // First value seeds; then max-so-far.
        let running_max = g
            .counter(Duration::from_nanos(100))
            .map(lambda("lambda n: (n * 7) % 5")) // 2,4,1,3,0,2
            .reduce(lambda("lambda acc, v: max(acc, v)"));
        run_cycles(&g, 6);
        let v: i64 = (&running_max.value()).try_into().unwrap();
        assert_eq!(4, v);
    }

    #[test]
    fn split_decomposes_tuples() {
        let g = PyGraph::new();
        let pairs = g
            .counter(Duration::from_nanos(100))
            .map(lambda("lambda n: (n, n * 10)"));
        let (left, right) = pairs.split();
        run_cycles(&g, 3);
        let l: i64 = (&left.value()).try_into().unwrap();
        let r: i64 = (&right.value()).try_into().unwrap();
        assert_eq!((3, 30), (l, r));
    }

    #[test]
    fn merge_all_earliest_supplied_wins_ties() {
        let g = PyGraph::new();
        let a = g.counter(Duration::from_nanos(300));
        let b = g
            .counter(Duration::from_nanos(300))
            .map(lambda("lambda n: n + 100"));
        let c = g
            .counter(Duration::from_nanos(300))
            .map(lambda("lambda n: n + 200"));
        let merged = a.merge_all(&[b, c]);
        run_cycles(&g, 3);
        // All three tick together each instant; `a` (earliest) wins the tie.
        let v: i64 = (&merged.value()).try_into().unwrap();
        assert_eq!(3, v);
    }

    #[test]
    fn bimap_combines_two_inputs() {
        let g = PyGraph::new();
        let a = g.counter(Duration::from_nanos(100)); // 1,2,3
        let b = g
            .counter(Duration::from_nanos(100))
            .map(lambda("lambda n: n * 10")); // 10,20,30
        let summed = a.bimap(&b, lambda("lambda x, y: x + y"));
        run_cycles(&g, 3);
        let v: i64 = (&summed.value()).try_into().unwrap();
        assert_eq!(33, v); // 3 + 30
    }

    #[test]
    fn bimap_exception_aborts_run() {
        let g = PyGraph::new();
        let a = g.counter(Duration::from_nanos(100));
        let b = g.counter(Duration::from_nanos(100));
        let _bad = a.bimap(
            &b,
            lambda("lambda x, y: (_ for _ in ()).throw(ValueError())"),
        );
        let err = g
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap_err();
        assert!(format!("{err:#}").contains("Python bimap callable raised"));
    }

    #[test]
    fn sum_is_cumulative() {
        let g = PyGraph::new();
        let total = g.counter(Duration::from_nanos(100)).sum();
        run_cycles(&g, 4);
        let v: f64 = (&total.value()).try_into().unwrap();
        assert_eq!(10.0, v); // 1+2+3+4
    }

    #[test]
    fn mean_is_cumulative() {
        let g = PyGraph::new();
        let avg = g.counter(Duration::from_nanos(100)).mean();
        run_cycles(&g, 4);
        let v: f64 = (&avg.value()).try_into().unwrap();
        assert_eq!(2.5, v); // (1+2+3+4)/4
    }

    #[test]
    fn sum_of_non_numeric_aborts_run() {
        let g = PyGraph::new();
        let _bad = g
            .counter(Duration::from_nanos(100))
            .map(lambda("lambda n: 'x'"))
            .sum();
        let err = g
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap_err();
        assert!(format!("{err:#}").contains("not a f64"));
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
    fn values_replays_a_sequence() {
        let g = PyGraph::new();
        let src = g.values(
            vec![
                PyElement::from(10_i64),
                PyElement::from(20_i64),
                PyElement::from(30_i64),
            ],
            Duration::from_nanos(100),
        );
        let acc = src.accumulate();
        run_cycles(&g, 3);
        let v: Vec<i64> = Python::attach(|py| acc.value().value().extract(py).unwrap());
        assert_eq!(vec![10, 20, 30], v);
    }

    #[test]
    fn values_source_feeds_downstream_ops() {
        let g = PyGraph::new();
        // Feed real data and run it through a combinator chain.
        let doubled = g
            .values(
                vec![
                    PyElement::from(1_i64),
                    PyElement::from(2_i64),
                    PyElement::from(3_i64),
                ],
                Duration::from_nanos(100),
            )
            .map(lambda("lambda x: x * 2"))
            .sum();
        run_cycles(&g, 3);
        let v: f64 = (&doubled.value()).try_into().unwrap();
        assert_eq!(12.0, v); // (1+2+3)*2
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
