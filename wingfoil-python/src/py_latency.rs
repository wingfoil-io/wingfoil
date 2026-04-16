//! Python bindings for latency measurement.
//!
//! Provides `Latency` and `TracedBytes` pyclasses, plus Rust-side stamp and
//! report nodes that operate on `PyElement`-wrapped `TracedBytes` values.

use std::collections::HashMap;
use std::rc::Rc;

use pyo3::exceptions::PyKeyError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::py_element::PyElement;
use wingfoil::{
    GraphState, IntoNode, IntoStream, MutableNode, Node, Stream, StreamPeekRef, UpStreams,
};

// ---------------------------------------------------------------------------
// PyLatency — a named-field latency record backed by Vec<u64>
// ---------------------------------------------------------------------------

#[pyclass(name = "Latency")]
pub struct PyLatency {
    stages: Vec<String>,
    index_map: HashMap<String, usize>,
    stamps: Vec<u64>,
}

#[pymethods]
impl PyLatency {
    #[new]
    fn new(stages: Vec<String>) -> PyResult<Self> {
        if stages.is_empty() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "stages list must not be empty",
            ));
        }
        let n = stages.len();
        let index_map: HashMap<String, usize> = stages
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), i))
            .collect();
        if index_map.len() != n {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "duplicate stage name",
            ));
        }
        Ok(Self {
            stages,
            index_map,
            stamps: vec![0u64; n],
        })
    }

    fn __getitem__(&self, stage: &str) -> PyResult<u64> {
        let idx = self
            .index_map
            .get(stage)
            .copied()
            .ok_or_else(|| PyKeyError::new_err(format!("unknown stage '{stage}'")))?;
        Ok(self.stamps[idx])
    }

    fn __setitem__(&mut self, stage: &str, value: u64) -> PyResult<()> {
        let idx = self
            .index_map
            .get(stage)
            .copied()
            .ok_or_else(|| PyKeyError::new_err(format!("unknown stage '{stage}'")))?;
        self.stamps[idx] = value;
        Ok(())
    }

    #[getter]
    fn stages(&self) -> Vec<String> {
        self.stages.clone()
    }

    #[getter]
    fn stamps(&self) -> Vec<u64> {
        self.stamps.clone()
    }

    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        let bytes: Vec<u8> = self.stamps.iter().flat_map(|v| v.to_le_bytes()).collect();
        PyBytes::new(py, &bytes)
    }

    #[staticmethod]
    fn from_bytes(data: &[u8], stages: Vec<String>) -> PyResult<Self> {
        let n = stages.len();
        let expected = n * 8;
        if data.len() < expected {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "expected at least {expected} bytes for {n} stages, got {}",
                data.len()
            )));
        }
        let index_map: HashMap<String, usize> = stages
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), i))
            .collect();
        let stamps: Vec<u64> = (0..n)
            .map(|i| {
                let offset = i * 8;
                u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
            })
            .collect();
        Ok(Self {
            stages,
            index_map,
            stamps,
        })
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
    pub fn create(stages: Vec<String>) -> Self {
        let n = stages.len();
        let index_map: HashMap<String, usize> = stages
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), i))
            .collect();
        Self {
            stages,
            index_map,
            stamps: vec![0u64; n],
        }
    }

    pub fn create_from_bytes(data: &[u8], stages: Vec<String>) -> Self {
        let n = stages.len();
        let index_map: HashMap<String, usize> = stages
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), i))
            .collect();
        let stamps: Vec<u64> = (0..n)
            .map(|i| {
                let offset = i * 8;
                if offset + 8 <= data.len() {
                    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
                } else {
                    0
                }
            })
            .collect();
        Self {
            stages,
            index_map,
            stamps,
        }
    }

    pub fn stamp_by_index(&mut self, idx: usize, time_ns: u64) {
        self.stamps[idx] = time_ns;
    }

    pub fn stage_index(&self, name: &str) -> Option<usize> {
        self.index_map.get(name).copied()
    }

    pub fn stage_count(&self) -> usize {
        self.stages.len()
    }

    pub fn stamps_ref(&self) -> &[u64] {
        &self.stamps
    }

    pub fn stages_ref(&self) -> &[String] {
        &self.stages
    }
}

// ---------------------------------------------------------------------------
// PyTracedBytes — (payload: bytes, latency: Latency)
// ---------------------------------------------------------------------------

#[pyclass(name = "TracedBytes")]
pub struct PyTracedBytes {
    #[pyo3(get)]
    pub payload: Py<PyAny>,
    #[pyo3(get)]
    pub latency: Py<PyLatency>,
}

impl Clone for PyTracedBytes {
    fn clone(&self) -> Self {
        Python::attach(|py| Self {
            payload: self.payload.clone_ref(py),
            latency: self.latency.clone_ref(py),
        })
    }
}

#[pymethods]
impl PyTracedBytes {
    #[new]
    fn new(payload: Py<PyAny>, latency: Py<PyLatency>) -> Self {
        Self { payload, latency }
    }

    fn __repr__(&self) -> String {
        Python::attach(|py| {
            let lat = self.latency.borrow(py);
            format!("TracedBytes(payload=..., latency={})", lat.__repr__())
        })
    }
}

impl PyTracedBytes {
    pub fn create(payload: Py<PyAny>, latency: Py<PyLatency>) -> Self {
        Self { payload, latency }
    }
}

// ---------------------------------------------------------------------------
// PyStampNode — stamps one named stage on each upstream tick
// ---------------------------------------------------------------------------

pub struct PyStampNode {
    upstream: Rc<dyn Stream<PyElement>>,
    value: PyElement,
    stage_name: String,
    stage_idx: Option<usize>,
    precise: bool,
}

impl PyStampNode {
    pub fn new(upstream: Rc<dyn Stream<PyElement>>, stage_name: String, precise: bool) -> Self {
        Self {
            upstream,
            value: PyElement::default(),
            stage_name,
            stage_idx: None,
            precise,
        }
    }
}

impl MutableNode for PyStampNode {
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }

    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let elem = self.upstream.peek_value();
        let wall_ns: u64 = if self.precise {
            state.wall_time_precise().into()
        } else {
            state.wall_time().into()
        };

        Python::attach(|py| -> anyhow::Result<()> {
            let obj = elem.as_ref().bind(py);
            let traced: PyRef<PyTracedBytes> = obj.extract().map_err(|_| {
                anyhow::anyhow!(
                    "stamp('{}'): expected TracedBytes, got {}",
                    self.stage_name,
                    obj.get_type()
                        .qualname()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|_| "?".to_string())
                )
            })?;
            let mut lat = traced.latency.borrow_mut(py);
            let idx = match self.stage_idx {
                Some(i) => i,
                None => {
                    let i = lat.stage_index(&self.stage_name).ok_or_else(|| {
                        anyhow::anyhow!(
                            "stamp: unknown stage '{}' (known: {:?})",
                            self.stage_name,
                            lat.stages
                        )
                    })?;
                    self.stage_idx = Some(i);
                    i
                }
            };
            lat.stamp_by_index(idx, wall_ns);
            Ok(())
        })?;

        self.value = elem;
        Ok(true)
    }
}

impl StreamPeekRef<PyElement> for PyStampNode {
    fn peek_ref(&self) -> &PyElement {
        &self.value
    }
}

// ---------------------------------------------------------------------------
// PyLatencyReportNode — sink that aggregates per-stage stats
// ---------------------------------------------------------------------------

pub struct PyLatencyReportNode {
    upstream: Rc<dyn Stream<PyElement>>,
    stage_names: Vec<String>,
    stats: Vec<wingfoil::StageStats>,
    print_on_teardown: bool,
}

impl PyLatencyReportNode {
    pub fn new(
        upstream: Rc<dyn Stream<PyElement>>,
        stage_names: Vec<String>,
        print_on_teardown: bool,
    ) -> Self {
        let n = stage_names.len();
        Self {
            upstream,
            stage_names,
            stats: vec![wingfoil::StageStats::default(); n],
            print_on_teardown,
        }
    }
}

impl MutableNode for PyLatencyReportNode {
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }

    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let elem = self.upstream.peek_value();
        Python::attach(|py| -> anyhow::Result<()> {
            let obj = elem.as_ref().bind(py);
            let traced: PyRef<PyTracedBytes> = obj
                .extract()
                .map_err(|_| anyhow::anyhow!("latency_report: expected TracedBytes"))?;
            let lat = traced.latency.borrow(py);
            let stamps = &lat.stamps;
            for i in 1..stamps.len().min(self.stats.len()) {
                let prev = stamps[i - 1];
                let cur = stamps[i];
                if prev == 0 || cur == 0 || cur < prev {
                    continue;
                }
                self.stats[i].record(cur - prev);
            }
            Ok(())
        })?;
        Ok(true)
    }

    fn stop(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
        if !self.print_on_teardown {
            return Ok(());
        }
        println!("latency report (delta from previous stage, nanoseconds):");
        println!(
            "  {:<24} {:>10} {:>12} {:>12} {:>12} {:>12} {:>12}",
            "stage", "count", "min", "mean", "p50", "p99", "max"
        );
        for i in 1..self.stage_names.len() {
            let s = &self.stats[i];
            let label = format!("{} -> {}", self.stage_names[i - 1], self.stage_names[i]);
            if s.count == 0 {
                println!("  {label:<24} {:>10}", "(no samples)");
                continue;
            }
            println!(
                "  {:<24} {:>10} {:>12} {:>12} {:>12} {:>12} {:>12}",
                label,
                s.count,
                s.min_ns,
                s.mean_ns(),
                s.quantile_ns(0.5),
                s.quantile_ns(0.99),
                s.max_ns,
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helper: create stamp/report from PyStream innards
// ---------------------------------------------------------------------------

pub fn py_stamp_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    stage: String,
    precise: bool,
) -> Rc<dyn Stream<PyElement>> {
    PyStampNode::new(stream.clone(), stage, precise).into_stream()
}

pub fn py_latency_report_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    stages: Vec<String>,
    print_on_teardown: bool,
) -> Rc<dyn Node> {
    PyLatencyReportNode::new(stream.clone(), stages, print_on_teardown).into_node()
}
