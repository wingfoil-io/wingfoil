//! Python bindings for the wingfoil-next **latency** surface
//! ([`wingfoil_next::latency`]) — stamp wall-clock timestamps onto messages as
//! they hop through a graph (and across processes), then aggregate the
//! per-stage deltas at the end of the pipeline.
//!
//! Not an I/O adapter: nothing here is feature-gated, because
//! `wingfoil_next::latency` is unconditional in the engine. The module is
//! registered in the `#[pymodule]` unconditionally too, so every wheel has it.
//!
//! | Python                                     | Rust                    | shape |
//! |--------------------------------------------|-------------------------|-------|
//! | `Latency(stages)`                          | [`PyLatency`]           | the record — a named `u64` stamp per stage |
//! | `TracedBytes(payload, latency)`            | [`PyTracedBytes`]       | a payload paired with its record |
//! | `LatencyStats`                             | [`PyLatencyStats`]      | the read-back handle a report hands back |
//! | `stamp(stream, stage)`                     | [`stamp`]               | pass-through transform, cycle-start clock |
//! | `stamp_if(stream, stage, enabled)`         | [`stamp_if`]            | ditto, wired only when `enabled` |
//! | `stamp_precise(stream, stage)`             | [`stamp_precise`]       | ditto, fresh TSC read per tick |
//! | `stamp_precise_if(stream, stage, enabled)` | [`stamp_precise_if`]    | ditto, wired only when `enabled` |
//! | `latency_report(stream, stages, …)`        | [`latency_report`]      | sink -> `(Stream, LatencyStats)` |
//! | `latency_report_if(stream, stages, …)`     | [`latency_report_if`]   | ditto, aggregating only when `enabled` |
//!
//! # The dynamic edge
//!
//! A Rust caller declares stages with [`latency_stages!`], which mints one
//! zero-sized `Stage` **type** per stage and a `#[repr(C)]` record; a stamp then
//! names its stage as a type parameter (`.stamp::<trade::decode>()`) and the
//! whole thing resolves at compile time.
//!
//! Python can name neither, so this module supplies the *dynamic* twin: a
//! [`PyLatency`] carries a runtime `Vec<String>` of stage names alongside its
//! `Vec<u64>` stamps, and a stamp resolves its slot by name on each tick. That
//! is the only structural difference — the stamps are the same little-endian
//! `u64`s in the same order, so [`PyLatency::to_bytes`] /
//! [`PyLatency::from_bytes`] interoperate with a Rust peer's
//! [`latency_stages!`] record over a wire hop.
//!
//! Aggregation and the printed report are **not** re-implemented here: both go
//! through `wingfoil_next::latency`'s [`record_stage_deltas`] /
//! [`format_latency_report`], the runtime-named entry points the typed
//! [`LatencyStats`](wingfoil_next::latency::LatencyStats) also delegates to. A
//! Python report is therefore byte-identical to a Rust one.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **Free functions, not `Stream` methods.** Legacy exposes
//!    `stream.stamp("a")`; here it is `wingfoil_next.stamp(stream, "a")` — the
//!    same ergonomic deviation the otlp/augurs stream transforms carry, for
//!    uniformity with the plugin story (a binding in a third-party crate cannot
//!    add a method to the `Stream` pyclass).
//! 2. **`latency_report` returns `(sink, stats)`, not just the sink.** The
//!    engine's `LatencyReportOps::latency_report` hands back a shared
//!    [`LatencyStats`](wingfoil_next::latency::LatencyStats) so the caller can
//!    read the numbers after the run; legacy Python could only print them. The
//!    second element is a [`PyLatencyStats`] with `stages` / `report()` /
//!    `stats[stage]`. New capability, not a loss.
//! 3. **Bursts are stamped element-wise.** A next adapter source erases a burst
//!    to a Python `list` per tick, so a value reaching `stamp` may be a
//!    `list`/`tuple` of `TracedBytes` rather than one. Every member is stamped,
//!    under a single GIL attach for the whole burst. Legacy had no burst shape.
//! 4. **The stage index is resolved per tick, not cached.** Legacy's
//!    `PyStampNode` cached the index off the first value it saw and reused it
//!    blindly, so a later value carrying a *different* stage list was stamped
//!    into whatever slot happened to sit at that index. The lookup is a small
//!    hash probe, dwarfed by the GIL attach the stamp needs anyway.
//! 5. **`Latency.from_bytes` validates its stage list** (non-empty, no
//!    duplicates), as the constructor already did. Legacy checked only the byte
//!    length, so `from_bytes(data, ["a", "a"])` built a record whose duplicate
//!    name silently shadowed a slot that could then never be stamped.
//! 6. **A stage-count mismatch at `latency_report` is an error.** Legacy
//!    aggregated over `min(record_stages, declared_stages)`, silently reporting
//!    on a prefix when the two disagreed. Here it aborts the run naming both
//!    counts (the fail-loudly rule the adapter bindings follow).
//! 7. **`latency_report_if(enabled=False)` returns a sink that never ticks**,
//!    matching the engine's `latency_report_if`; legacy returned the *upstream*
//!    node, so the disabled call changed the returned value's type. The stats
//!    handle is still returned, with every count at zero.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use anyhow::{Result, anyhow, bail};
use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};
use wingfoil_next::latency::{StageStats, format_latency_report, record_stage_deltas};

use crate::graph::PyStream;
use crate::python::Stream;
use crate::{Activation, Ctx, PyElement, Tick};

/// Bytes per stamp on the wire — one little-endian `u64`, the layout a
/// [`latency_stages!`](wingfoil_next::latency::latency_stages) record has in
/// memory.
const STAMP_BYTES: usize = 8;

// ---------------------------------------------------------------------------
// Latency — the runtime-named record
// ---------------------------------------------------------------------------

/// A latency record with a **runtime** stage list: `stages[i]` names the stamp
/// in `stamps[i]`. The dynamic stand-in for the `#[repr(C)]` record
/// [`latency_stages!`](wingfoil_next::latency::latency_stages) generates for a
/// Rust caller.
#[pyclass(name = "Latency")]
pub struct PyLatency {
    stages: Vec<String>,
    index_map: HashMap<String, usize>,
    stamps: Vec<u64>,
}

#[pymethods]
impl PyLatency {
    /// A record over `stages`, every stamp zero (i.e. unset).
    #[new]
    fn new(stages: Vec<String>) -> PyResult<Self> {
        check_stages(&stages)?;
        Ok(Self::create(stages))
    }

    /// The stamp for `stage`. `KeyError` if this record has no such stage.
    fn __getitem__(&self, stage: &str) -> PyResult<u64> {
        Ok(self.stamps[self.require_stage(stage)?])
    }

    /// Set the stamp for `stage`. `KeyError` if this record has no such stage.
    fn __setitem__(&mut self, stage: &str, value: u64) -> PyResult<()> {
        let idx = self.require_stage(stage)?;
        self.stamps[idx] = value;
        Ok(())
    }

    /// The stage names, in stamp order.
    #[getter]
    fn stages(&self) -> Vec<String> {
        self.stages.clone()
    }

    /// The stamps, in stage order. Zero means "not stamped".
    #[getter]
    fn stamps(&self) -> Vec<u64> {
        self.stamps.clone()
    }

    /// The stamps as a little-endian `u64` header — the wire form a Rust peer
    /// reads straight back as its `latency_stages!` record.
    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        let bytes: Vec<u8> = self.stamps.iter().flat_map(|v| v.to_le_bytes()).collect();
        PyBytes::new(py, &bytes)
    }

    /// Read a record back off the wire: `data`'s leading `8 * len(stages)`
    /// bytes are the stamps, anything after them is ignored (a header split off
    /// a larger frame). Raises if `data` is too short for `stages`.
    #[staticmethod]
    fn from_bytes(data: &[u8], stages: Vec<String>) -> PyResult<Self> {
        check_stages(&stages)?;
        let expected = stages.len() * STAMP_BYTES;
        if data.len() < expected {
            return Err(PyValueError::new_err(format!(
                "expected at least {expected} bytes for {} stages, got {}",
                stages.len(),
                data.len()
            )));
        }
        Ok(Self::create_from_bytes(data, stages))
    }

    fn __repr__(&self) -> String {
        let pairs: Vec<String> = self
            .stages
            .iter()
            .zip(self.stamps.iter())
            .map(|(name, val)| format!("{name}={val}"))
            .collect();
        format!("Latency({})", pairs.join(", "))
    }

    fn __len__(&self) -> usize {
        self.stages.len()
    }
}

impl PyLatency {
    /// A zeroed record over `stages`, without the constructor's validation.
    ///
    /// The infallible form a **decoder** needs: an adapter splitting a stamp
    /// header off each received frame builds one of these per message, having
    /// validated the stage list once at wiring.
    pub fn create(stages: Vec<String>) -> Self {
        let stamps = vec![0_u64; stages.len()];
        Self {
            index_map: index_map(&stages),
            stages,
            stamps,
        }
    }

    /// [`create`](Self::create) from a wire header: the leading
    /// `8 * stages.len()` bytes of `data` as little-endian `u64`s. Stamps the
    /// data is too short for stay zero (unset), so a truncated frame degrades
    /// to "not stamped" rather than failing a decode.
    pub fn create_from_bytes(data: &[u8], stages: Vec<String>) -> Self {
        let stamps: Vec<u64> = (0..stages.len())
            .map(|i| {
                let offset = i * STAMP_BYTES;
                data.get(offset..offset + STAMP_BYTES)
                    .map(|slice| {
                        u64::from_le_bytes(
                            slice
                                .try_into()
                                .expect("invariant: an 8-byte slice converts to [u8; 8]"),
                        )
                    })
                    .unwrap_or(0)
            })
            .collect();
        Self {
            index_map: index_map(&stages),
            stages,
            stamps,
        }
    }

    /// The index of `stage`, or `None` if this record has no such stage.
    fn stage_index(&self, stage: &str) -> Option<usize> {
        self.index_map.get(stage).copied()
    }

    /// [`stage_index`](Self::stage_index) as a Python `KeyError`.
    fn require_stage(&self, stage: &str) -> PyResult<usize> {
        self.stage_index(stage)
            .ok_or_else(|| PyKeyError::new_err(format!("unknown stage '{stage}'")))
    }
}

/// Name -> index, the lookup a stamp resolves its slot through.
fn index_map(stages: &[String]) -> HashMap<String, usize> {
    stages
        .iter()
        .enumerate()
        .map(|(i, s)| (s.clone(), i))
        .collect()
}

/// Reject a stage list that cannot address its own stamps: empty (nothing to
/// stamp) or carrying a duplicate name (which would shadow a slot).
fn check_stages(stages: &[String]) -> PyResult<()> {
    if stages.is_empty() {
        return Err(PyValueError::new_err("stages list must not be empty"));
    }
    if index_map(stages).len() != stages.len() {
        return Err(PyValueError::new_err("duplicate stage name"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// TracedBytes — a payload carrying a record
// ---------------------------------------------------------------------------

/// A payload paired with its [`PyLatency`] record — the Python twin of
/// [`Traced<T, L>`](wingfoil_next::latency::Traced).
///
/// The record is a *shared* Python object, so a stamp mutates the same record
/// every downstream node sees; the payload is any Python object (the name
/// follows legacy, whose only carrier was a `bytes` frame).
#[pyclass(name = "TracedBytes")]
pub struct PyTracedBytes {
    #[pyo3(get)]
    pub payload: Py<PyAny>,
    #[pyo3(get)]
    pub latency: Py<PyLatency>,
}

#[pymethods]
impl PyTracedBytes {
    #[new]
    fn new(payload: Py<PyAny>, latency: Py<PyLatency>) -> Self {
        Self { payload, latency }
    }

    fn __repr__(&self) -> String {
        Python::attach(|py| {
            format!(
                "TracedBytes(payload={}, latency={})",
                self.payload
                    .bind(py)
                    .repr()
                    .map(|r| r.to_string())
                    .unwrap_or_else(|_| "?".to_string()),
                self.latency.borrow(py).__repr__()
            )
        })
    }
}

// ---------------------------------------------------------------------------
// LatencyStats — the aggregator and its Python handle
// ---------------------------------------------------------------------------

/// Per-stage delta statistics over a **runtime** stage list — the dynamic twin
/// of [`LatencyStats<L>`](wingfoil_next::latency::LatencyStats).
///
/// `stages[i]` accumulates the hop from stage `i - 1` to stage `i`; `stages[0]`
/// stays empty, since stage 0 has no predecessor.
struct DynLatencyStats {
    names: Vec<String>,
    stages: Vec<StageStats>,
}

impl DynLatencyStats {
    fn new(names: Vec<String>) -> Self {
        let stages = vec![StageStats::default(); names.len()];
        Self { names, stages }
    }

    /// Drop every sample, keeping the stage list — what a second
    /// [`Runner::run`](wingfoil_next::interp::Runner::run) needs so a re-run
    /// aggregates afresh rather than continuing the previous run's numbers.
    fn clear(&mut self) {
        self.stages.fill(StageStats::default());
    }

    /// Record one message's stamps. A record whose stage count disagrees with
    /// the declared list is an error: the two lists are meant to be the same
    /// stages, and reporting on whichever prefix they share would silently
    /// mislabel every hop.
    fn observe(&mut self, stamps: &[u64]) -> Result<()> {
        if stamps.len() != self.stages.len() {
            bail!(
                "latency_report: declared {} stages {:?} but the record carries {}",
                self.stages.len(),
                self.names,
                stamps.len()
            );
        }
        record_stage_deltas(&mut self.stages, stamps);
        Ok(())
    }

    fn format_report(&self) -> String {
        let names: Vec<&str> = self.names.iter().map(String::as_str).collect();
        format_latency_report(&names, &self.stages)
    }
}

/// The handle [`latency_report`] hands back: a live view of the per-hop
/// statistics its sink accumulates, readable after the graph has run.
#[pyclass(name = "LatencyStats", unsendable)]
pub struct PyLatencyStats(Rc<RefCell<DynLatencyStats>>);

#[pymethods]
impl PyLatencyStats {
    /// The stage names this report was declared over.
    #[getter]
    fn stages(&self) -> Vec<String> {
        self.0.borrow().names.clone()
    }

    fn __len__(&self) -> usize {
        self.0.borrow().names.len()
    }

    /// The statistics for the hop *ending* at `stage`, as a dict of
    /// `count` / `min_ns` / `mean_ns` / `p50_ns` / `p99_ns` / `max_ns`.
    ///
    /// `KeyError` for an unknown stage, and for the first stage — it has no
    /// predecessor, so there is no hop to report.
    fn __getitem__<'py>(&self, py: Python<'py>, stage: &str) -> PyResult<Bound<'py, PyDict>> {
        let stats = self.0.borrow();
        let idx = stats
            .names
            .iter()
            .position(|name| name == stage)
            .ok_or_else(|| {
                PyKeyError::new_err(format!(
                    "unknown stage '{stage}' (known: {:?})",
                    stats.names
                ))
            })?;
        if idx == 0 {
            return Err(PyKeyError::new_err(format!(
                "stage '{stage}' is the first stage and has no predecessor to \
                 measure a delta from"
            )));
        }
        let s = &stats.stages[idx];
        let dict = PyDict::new(py);
        dict.set_item("count", s.count)?;
        // `min_ns` starts at u64::MAX so the first sample always wins; report
        // the empty case as 0 rather than leaking that sentinel to Python.
        dict.set_item("min_ns", if s.count == 0 { 0 } else { s.min_ns })?;
        dict.set_item("mean_ns", s.mean_ns())?;
        dict.set_item("p50_ns", s.quantile_ns(0.5))?;
        dict.set_item("p99_ns", s.quantile_ns(0.99))?;
        dict.set_item("max_ns", s.max_ns)?;
        Ok(dict)
    }

    /// The multi-line summary, exactly as `print_on_teardown` prints it.
    fn report(&self) -> String {
        self.0.borrow().format_report()
    }

    fn __repr__(&self) -> String {
        format!("LatencyStats(stages={:?})", self.0.borrow().names)
    }
}

// ---------------------------------------------------------------------------
// The graph entry points
// ---------------------------------------------------------------------------

/// Visit every [`PyTracedBytes`] in one tick's value under a **single** GIL
/// attach: a `list`/`tuple` is a burst and every member is visited, anything
/// else is one message.
///
/// One attach per burst, not per element, is the boundary rule the erasure
/// seams follow — `PyGraph::run` detaches for the whole run, so each attach
/// inside a cycle is a real `PyGILState_Ensure`/`Release` pair and nesting them
/// is what makes the inner ones cheap.
fn for_each_traced<F>(value: &PyElement, who: &str, mut visit: F) -> Result<()>
where
    F: FnMut(Python<'_>, PyRef<'_, PyTracedBytes>) -> Result<()>,
{
    if value.is_none() {
        bail!("{who}: expected a TracedBytes, got an empty (never-ticked) value");
    }
    Python::attach(|py| {
        let obj = value.object().bind(py);
        if obj.is_instance_of::<PyList>() || obj.is_instance_of::<PyTuple>() {
            for item in obj
                .try_iter()
                .map_err(|err| anyhow!("{who}: reading the burst: {err}"))?
            {
                let item = item.map_err(|err| anyhow!("{who}: reading the burst: {err}"))?;
                visit(py, traced(&item, who)?)?;
            }
            Ok(())
        } else {
            visit(py, traced(obj, who)?)
        }
    })
}

/// Read one message as a [`PyTracedBytes`], naming what arrived instead when it
/// is not one.
fn traced<'py>(obj: &Bound<'py, PyAny>, who: &str) -> Result<PyRef<'py, PyTracedBytes>> {
    obj.extract::<PyRef<'py, PyTracedBytes>>().map_err(|_| {
        let got = obj
            .get_type()
            .qualname()
            .map(|name| name.to_string())
            .unwrap_or_else(|_| "?".to_string());
        anyhow!("{who}: expected a TracedBytes, got {got}")
    })
}

/// Wire the stamping node. `precise` picks the clock: the cycle-start snap
/// ([`Ctx::wall_time`]) or a fresh TSC read per tick
/// ([`Ctx::wall_time_precise`]).
fn wire_stamp(stream: &PyStream, stage: String, precise: bool) -> PyStream {
    let label = if precise { "stamp_precise" } else { "stamp" };
    stream.wire_op1(
        label,
        Activation::NONE,
        stage,
        || (),
        move |stage: &mut String, _state: &mut (), value: &PyElement, ctx: &mut Ctx<'_>| {
            let now = u64::from(if precise {
                ctx.wall_time_precise()
            } else {
                ctx.wall_time()
            });
            for_each_traced(value, label, |py, message| {
                let mut record = message.latency.borrow_mut(py);
                let idx = record.stage_index(stage).ok_or_else(|| {
                    anyhow!(
                        "{label}: unknown stage '{stage}' (the record's stages are {:?})",
                        record.stages
                    )
                })?;
                record.stamps[idx] = now;
                Ok(())
            })?;
            Ok(Tick::Value(value.clone()))
        },
    )
}

/// Stamp the wall-clock time into `stage` of each value's `Latency`, passing the
/// value through unchanged.
///
/// The stream must carry `TracedBytes` (or a list of them — an adapter burst);
/// anything else, or a stage the record does not declare, aborts the run.
/// Reads the **cycle-start** clock snap, so stages that stamp in the same engine
/// cycle share a timestamp — use [`stamp_precise`] for intra-cycle resolution.
#[pyfunction]
pub fn stamp(stream: PyRef<'_, Stream>, stage: String) -> Stream {
    Stream::from(wire_stamp(stream.object(), stage, false))
}

/// [`stamp`], wired only when `enabled`. When false the stream is returned
/// unchanged — no node, no runtime cost — so one config flag can turn stamping
/// off without wiring edits.
#[pyfunction]
pub fn stamp_if(stream: PyRef<'_, Stream>, stage: String, enabled: bool) -> Stream {
    if enabled {
        stamp(stream, stage)
    } else {
        Stream::from(stream.object().clone())
    }
}

/// [`stamp`] reading a **fresh** clock sample per tick rather than the
/// cycle-start snap, so stages stamping within one engine cycle get distinct
/// timestamps (a few nanoseconds per tick more than [`stamp`]).
#[pyfunction]
pub fn stamp_precise(stream: PyRef<'_, Stream>, stage: String) -> Stream {
    Stream::from(wire_stamp(stream.object(), stage, true))
}

/// [`stamp_precise`], wired only when `enabled` — see [`stamp_if`].
#[pyfunction]
pub fn stamp_precise_if(stream: PyRef<'_, Stream>, stage: String, enabled: bool) -> Stream {
    if enabled {
        stamp_precise(stream, stage)
    } else {
        Stream::from(stream.object().clone())
    }
}

/// The report sink's config: where the samples land, and whether to print.
struct ReportCfg {
    stats: Rc<RefCell<DynLatencyStats>>,
    print_on_teardown: bool,
}

/// Wire the report sink, handing back the shared accumulator it feeds.
fn wire_report(
    stream: &PyStream,
    stages: Vec<String>,
    print_on_teardown: bool,
) -> (PyStream, Rc<RefCell<DynLatencyStats>>) {
    let stats = Rc::new(RefCell::new(DynLatencyStats::new(stages)));
    // The stats live in `cfg`, which the engine does not reset — so clearing
    // them is folded into the state factory, which the engine *does* re-run
    // before a second `run()`. (At wiring it clears an already-empty report.)
    let stats_for_reset = stats.clone();
    let sink = stream.wire_op1_with_stop(
        "latency_report",
        Activation::NONE,
        ReportCfg {
            stats: stats.clone(),
            print_on_teardown,
        },
        move || stats_for_reset.borrow_mut().clear(),
        move |cfg: &mut ReportCfg, _state: &mut (), value: &PyElement, _ctx: &mut Ctx<'_>| {
            for_each_traced(value, "latency_report", |py, message| {
                let record = message.latency.borrow(py);
                cfg.stats.borrow_mut().observe(&record.stamps)
            })?;
            Ok(Tick::Value(()))
        },
        move |cfg: &mut ReportCfg, _state: &mut (), _ctx: &mut Ctx<'_>| {
            if cfg.print_on_teardown {
                print!("{}", cfg.stats.borrow().format_report());
            }
            Ok(())
        },
    );
    (sink, stats)
}

/// Install a latency report sink over `stages`, aggregating each value's
/// per-hop deltas.
///
/// Returns `(sink, stats)`: the sink is a terminal stream (its value is `None`)
/// that must stay reachable from the graph for the aggregation to run, and
/// `stats` is a live [`LatencyStats`](PyLatencyStats) readable once the graph
/// has run. With `print_on_teardown` the same summary is printed when the run
/// ends. A re-run aggregates afresh.
///
/// `stages` must be the same list, in the same order, that the values' `Latency`
/// records carry; a record with a different number of stages aborts the run.
#[pyfunction]
#[pyo3(signature = (stream, stages, print_on_teardown = true))]
pub fn latency_report(
    stream: PyRef<'_, Stream>,
    stages: Vec<String>,
    print_on_teardown: bool,
) -> PyResult<(Stream, PyLatencyStats)> {
    check_stages(&stages)?;
    let (sink, stats) = wire_report(stream.object(), stages, print_on_teardown);
    Ok((Stream::from(sink), PyLatencyStats(stats)))
}

/// [`latency_report`], aggregating only when `enabled`.
///
/// When false the sink is wired but never ticks and the returned stats stay at
/// zero, so a config flag toggles the report without changing the call's shape.
#[pyfunction]
#[pyo3(signature = (stream, stages, enabled, print_on_teardown = true))]
pub fn latency_report_if(
    stream: PyRef<'_, Stream>,
    stages: Vec<String>,
    enabled: bool,
    print_on_teardown: bool,
) -> PyResult<(Stream, PyLatencyStats)> {
    if enabled {
        return latency_report(stream, stages, print_on_teardown);
    }
    check_stages(&stages)?;
    let sink = stream.object().wire_op1(
        "latency_report_disabled",
        Activation::NONE,
        (),
        || (),
        |_cfg: &mut (), _state: &mut (), _value: &PyElement, _ctx: &mut Ctx<'_>| {
            Ok(Tick::<()>::Quiet)
        },
    );
    let stats = Rc::new(RefCell::new(DynLatencyStats::new(stages)));
    Ok((Stream::from(sink), PyLatencyStats(stats)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wingfoil::{NanoTime, RunFor, RunMode};

    use crate::graph::PyGraph;

    /// A `TracedBytes(b"payload", Latency(stages))` as a graph value.
    fn traced_element(stages: &[&str]) -> PyElement {
        let stages: Vec<String> = stages.iter().map(|s| s.to_string()).collect();
        Python::attach(|py| {
            let latency = Py::new(py, PyLatency::create(stages)).unwrap();
            let payload = PyBytes::new(py, b"payload").into_any().unbind();
            let traced = Py::new(py, PyTracedBytes { payload, latency }).unwrap();
            PyElement::new(traced.into_any())
        })
    }

    /// The stamps of a `TracedBytes` element (or of a burst's first member).
    fn stamps_of(element: &PyElement) -> Vec<u64> {
        Python::attach(|py| {
            let obj = element.object().bind(py);
            let message = if obj.is_instance_of::<PyList>() {
                obj.get_item(0).unwrap()
            } else {
                obj.clone()
            };
            let traced: PyRef<PyTracedBytes> = message.extract().unwrap();
            traced.latency.borrow(py).stamps.clone()
        })
    }

    fn run(graph: &PyGraph, cycles: u32) -> Result<()> {
        graph.run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(cycles),
        )
    }

    #[test]
    fn a_record_addresses_its_stamps_by_name() {
        let mut record = PyLatency::create(vec!["a".into(), "b".into()]);
        assert_eq!(Some(1), record.stage_index("b"));
        assert_eq!(None, record.stage_index("missing"));
        record.stamps[1] = 42;
        assert_eq!(vec![0, 42], record.stamps);
    }

    #[test]
    fn stamps_round_trip_through_the_wire_header() {
        let mut record = PyLatency::create(vec!["a".into(), "b".into()]);
        record.stamps = vec![12345, 67890];
        let bytes: Vec<u8> = record.stamps.iter().flat_map(|v| v.to_le_bytes()).collect();
        // A trailing payload must not disturb the header split.
        let mut frame = bytes.clone();
        frame.extend_from_slice(b"payload");
        let back = PyLatency::create_from_bytes(&frame, record.stages.clone());
        assert_eq!(vec![12345, 67890], back.stamps);
    }

    #[test]
    fn a_short_header_leaves_the_missing_stamps_unset() {
        let back = PyLatency::create_from_bytes(&7_u64.to_le_bytes(), vec!["a".into(), "b".into()]);
        assert_eq!(vec![7, 0], back.stamps);
    }

    #[test]
    fn an_unusable_stage_list_is_rejected() {
        assert!(check_stages(&[]).is_err());
        assert!(check_stages(&["a".into(), "a".into()]).is_err());
        assert!(check_stages(&["a".into(), "b".into()]).is_ok());
    }

    #[test]
    fn stamping_writes_the_wall_clock_in_stage_order() {
        let graph = PyGraph::new();
        let source = graph.values(vec![traced_element(&["start", "end"])], Duration::ZERO);
        let stamped = wire_stamp(
            &wire_stamp(&source, "start".into(), false),
            "end".into(),
            false,
        );
        run(&graph, 1).unwrap();

        let stamps = stamps_of(&stamped.value());
        assert!(stamps[0] > 0, "start was not stamped: {stamps:?}");
        assert!(stamps[1] >= stamps[0], "stages out of order: {stamps:?}");
    }

    #[test]
    fn the_precise_clock_separates_stages_within_one_cycle() {
        let graph = PyGraph::new();
        let source = graph.values(vec![traced_element(&["start", "end"])], Duration::ZERO);
        let stamped = wire_stamp(
            &wire_stamp(&source, "start".into(), true),
            "end".into(),
            true,
        );
        run(&graph, 1).unwrap();

        let stamps = stamps_of(&stamped.value());
        assert!(
            stamps[1] > stamps[0],
            "a fresh clock read per tick must advance: {stamps:?}"
        );
    }

    #[test]
    fn every_member_of_a_burst_is_stamped() {
        let graph = PyGraph::new();
        // A burst reaches the boundary as a Python list, the shape an adapter
        // source erases to.
        let burst = Python::attach(|py| {
            let items = vec![
                traced_element(&["start", "end"]).value(),
                traced_element(&["start", "end"]).value(),
            ];
            PyElement::new(PyList::new(py, items).unwrap().into_any().unbind())
        });
        let stamped = wire_stamp(
            &graph.values(vec![burst], Duration::ZERO),
            "start".into(),
            false,
        );
        run(&graph, 1).unwrap();

        let value = stamped.value();
        let each: Vec<Vec<u64>> = Python::attach(|py| {
            value
                .object()
                .bind(py)
                .try_iter()
                .unwrap()
                .map(|item| {
                    let traced: PyRef<PyTracedBytes> = item.unwrap().extract().unwrap();
                    traced.latency.borrow(py).stamps.clone()
                })
                .collect()
        });
        assert_eq!(2, each.len());
        for stamps in each {
            assert!(stamps[0] > 0, "burst member not stamped: {stamps:?}");
        }
    }

    #[test]
    fn stamping_an_unknown_stage_aborts_the_run() {
        let graph = PyGraph::new();
        let source = graph.values(vec![traced_element(&["start"])], Duration::ZERO);
        let _stamped = wire_stamp(&source, "nope".into(), false);
        let err = run(&graph, 1).unwrap_err();
        assert!(
            format!("{err:#}").contains("unknown stage 'nope'"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn stamping_a_non_traced_value_aborts_the_run() {
        let graph = PyGraph::new();
        let source = graph.values(vec![PyElement::from(1.0_f64)], Duration::ZERO);
        let _stamped = wire_stamp(&source, "start".into(), false);
        let err = run(&graph, 1).unwrap_err();
        assert!(
            format!("{err:#}").contains("expected a TracedBytes, got float"),
            "unexpected error: {err:#}"
        );
    }

    /// Two messages stamped a known distance apart, so the report's arithmetic
    /// is checkable rather than merely non-empty.
    fn report_over_fixed_deltas(deltas: &[u64]) -> DynLatencyStats {
        let mut stats = DynLatencyStats::new(vec!["a".into(), "b".into()]);
        for delta in deltas {
            stats.observe(&[1_000, 1_000 + delta]).unwrap();
        }
        stats
    }

    #[test]
    fn the_report_aggregates_the_hop_between_stages() {
        let stats = report_over_fixed_deltas(&[100, 300]);
        assert_eq!(2, stats.stages[1].count);
        assert_eq!(100, stats.stages[1].min_ns);
        assert_eq!(300, stats.stages[1].max_ns);
        assert_eq!(200, stats.stages[1].mean_ns());
        // Stage 0 has no predecessor, so it is never recorded against.
        assert_eq!(0, stats.stages[0].count);
    }

    #[test]
    fn the_report_renders_one_row_per_hop() {
        let text = report_over_fixed_deltas(&[100]).format_report();
        assert!(text.contains("a -> b"), "unexpected report:\n{text}");
        assert!(!text.contains("(no samples)"), "unexpected report:\n{text}");
        // The stage-0 row has no predecessor and is not printed.
        assert_eq!(3, text.lines().count(), "unexpected report:\n{text}");
    }

    #[test]
    fn an_unstamped_hop_reports_no_samples() {
        let mut stats = DynLatencyStats::new(vec!["a".into(), "b".into()]);
        stats.observe(&[1_000, 0]).unwrap();
        assert_eq!(0, stats.stages[1].count);
        assert!(stats.format_report().contains("(no samples)"));
    }

    #[test]
    fn clearing_drops_the_previous_runs_samples() {
        let mut stats = report_over_fixed_deltas(&[100]);
        stats.clear();
        assert_eq!(0, stats.stages[1].count);
        assert_eq!(vec!["a".to_string(), "b".to_string()], stats.names);
    }

    #[test]
    fn a_record_with_the_wrong_stage_count_is_rejected() {
        let mut stats = DynLatencyStats::new(vec!["a".into(), "b".into()]);
        let err = stats.observe(&[1, 2, 3]).unwrap_err();
        assert!(
            format!("{err:#}").contains("declared 2 stages"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn a_report_over_a_graph_measures_the_stamped_hop() {
        let graph = PyGraph::new();
        let source = graph.values(
            vec![
                traced_element(&["start", "end"]),
                traced_element(&["start", "end"]),
            ],
            Duration::from_nanos(100),
        );
        let stamped = wire_stamp(
            &wire_stamp(&source, "start".into(), true),
            "end".into(),
            true,
        );
        let (_sink, stats) = wire_report(&stamped, vec!["start".into(), "end".into()], false);
        run(&graph, 2).unwrap();

        assert_eq!(2, stats.borrow().stages[1].count);
        assert!(stats.borrow().stages[1].min_ns > 0);
    }

    #[test]
    fn a_report_over_an_unstamped_stream_aborts_the_run() {
        let graph = PyGraph::new();
        let source = graph.values(vec![PyElement::from(1.0_f64)], Duration::ZERO);
        let (_sink, _stats) = wire_report(&source, vec!["start".into(), "end".into()], false);
        let err = run(&graph, 1).unwrap_err();
        assert!(
            format!("{err:#}").contains("expected a TracedBytes"),
            "unexpected error: {err:#}"
        );
    }
}
