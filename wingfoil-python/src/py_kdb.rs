//! Python bindings for KDB+ read/write adapters.

use crate::PyNode;
use crate::py_element::PyElement;
use crate::py_stream::PyStream;

use pyo3::conversion::IntoPyObject;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::pin::Pin;
use std::rc::Rc;

use futures::StreamExt;
use kdb_plus_fixed::ipc::{ConnectionMethod, QStream};
use kdb_plus_fixed::qtype;
use wingfoil::adapters::kdb::*;
use wingfoil::{Burst, FutStream, NanoTime, Node, Stream, StreamOperators};

/// A deserialized KDB row stored as column name/value pairs.
/// Each row becomes a Python dict.
#[derive(Debug, Clone, Default)]
struct PyKdbRow {
    columns: Vec<(String, PyKdbValue)>,
}

/// Intermediate value type for KDB -> Python conversion.
#[derive(Debug, Clone)]
enum PyKdbValue {
    Long(i64),
    Float(f64),
    Symbol(String),
    Bool(bool),
    Int(i32),
    Real(f32),
}

impl Default for PyKdbValue {
    fn default() -> Self {
        PyKdbValue::Long(0)
    }
}

impl PyKdbValue {
    fn to_py(&self, py: Python<'_>) -> Py<PyAny> {
        match self {
            PyKdbValue::Long(v) => v.into_pyobject(py).unwrap().into_any().unbind(),
            PyKdbValue::Float(v) => v.into_pyobject(py).unwrap().into_any().unbind(),
            PyKdbValue::Symbol(v) => v.into_pyobject(py).unwrap().into_any().unbind(),
            PyKdbValue::Bool(v) => v.into_pyobject(py).unwrap().to_owned().into_any().unbind(),
            PyKdbValue::Int(v) => v.into_pyobject(py).unwrap().into_any().unbind(),
            PyKdbValue::Real(v) => v.into_pyobject(py).unwrap().into_any().unbind(),
        }
    }
}

impl KdbDeserialize for PyKdbRow {
    fn from_kdb_row(
        row: Row<'_>,
        columns: &[String],
        _interner: &mut SymbolInterner,
    ) -> Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(0)?; // col 0: time

        let mut result = Vec::with_capacity(columns.len() - 1);
        // Skip column 0 (time) — guaranteed first by xcols in the query wrapper.
        for (i, col_name) in columns.iter().enumerate().skip(1) {
            let k = row.get(i)?;
            let value = match k.get_type() {
                qtype::LONG_ATOM => PyKdbValue::Long(k.get_long()?),
                qtype::FLOAT_ATOM => PyKdbValue::Float(k.get_float()?),
                qtype::SYMBOL_ATOM => PyKdbValue::Symbol(k.get_symbol()?.to_string()),
                qtype::BOOL_ATOM => PyKdbValue::Bool(k.get_bool()?),
                qtype::INT_ATOM => PyKdbValue::Int(k.get_int()?),
                qtype::REAL_ATOM => PyKdbValue::Real(k.get_real()?),
                qtype::SHORT_ATOM => PyKdbValue::Int(k.get_short()? as i32),
                // Temporal types stored as i64 (nanoseconds since KDB epoch)
                qtype::TIMESTAMP_ATOM | qtype::TIMESPAN_ATOM => PyKdbValue::Long(k.get_long()?),
                // Temporal types stored as i32
                qtype::DATE_ATOM
                | qtype::MONTH_ATOM
                | qtype::TIME_ATOM
                | qtype::MINUTE_ATOM
                | qtype::SECOND_ATOM => PyKdbValue::Int(k.get_int()?),
                // Datetime stored as f64 (days since KDB epoch)
                qtype::DATETIME_ATOM => PyKdbValue::Float(k.get_float()?),
                _ => PyKdbValue::Symbol(format!("{k:?}")),
            };
            result.push((col_name.clone(), value));
        }

        Ok((time, PyKdbRow { columns: result }))
    }
}

/// Read data from a KDB+ database.
///
/// Returns a PyStream where each tick is a dict with column names as keys.
///
/// Args:
///     host: KDB+ server hostname
///     port: KDB+ server port
///     query: q query to execute (e.g. "select from trade")
///     time_col: Name of the time column for time-slice filtering
///     chunk_size: Duration of each time slice in seconds (default: 3600)
///
/// Returns:
///     Stream where each tick yields a dict of {column_name: value}
///
/// Requires RunMode::HistoricalFrom with a non-zero start time and RunFor::Duration.
#[pyfunction]
#[pyo3(signature = (host, port, query, time_col, chunk_size=3600))]
pub fn py_kdb_read(
    host: String,
    port: u16,
    query: String,
    time_col: String,
    chunk_size: u64,
) -> PyStream {
    let conn = KdbConnection::new(host, port);
    let stream: Rc<dyn Stream<Burst<PyKdbRow>>> = kdb_read::<PyKdbRow, _>(
        conn,
        std::time::Duration::from_secs(chunk_size),
        move |(t0, t1), _date, _iter| {
            // xcols ensures the time column is always first (col 0) in the result,
            // regardless of its position in the user's original query.
            format!(
                "`{tc} xcols select from ({q}) where {tc} >= (`timestamp$){t0}j, {tc} < (`timestamp$){t1}j",
                tc = time_col,
                q = query,
                t0 = t0.to_kdb_timestamp(),
                t1 = t1.to_kdb_timestamp(),
            )
        },
    );

    // Collapse burst to single row, convert to PyElement (dict)
    let collapsed = stream.collapse();
    let py_stream = collapsed.map(|row: PyKdbRow| {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            for (name, value) in &row.columns {
                dict.set_item(name, value.to_py(py)).unwrap();
            }
            PyElement::new(dict.into_any().unbind())
        })
    });

    PyStream(py_stream)
}

/// Write stream data to a KDB+ table.
///
/// Args:
///     host: KDB+ server hostname
///     port: KDB+ server port
///     table: Name of the target KDB+ table
///     columns: List of (name, type) tuples for non-time columns.
///              Supported types: "symbol", "float", "long", "int", "bool"
///     upstream: The PyStream to consume
///
/// Returns:
///     A Node that drives the write operation.
#[pyfunction]
#[pyo3(signature = (host, port, table, columns, upstream))]
pub fn py_kdb_write(
    host: String,
    port: u16,
    table: String,
    columns: Vec<(String, String)>,
    upstream: &PyStream,
) -> PyResult<PyNode> {
    let conn = KdbConnection::new(host, port);
    let node = py_kdb_write_inner(conn, table, columns, &upstream.0)?;
    Ok(PyNode::new(node))
}

/// Inner implementation for kdb_write that works on the Rust stream level.
pub fn py_kdb_write_inner(
    connection: KdbConnection,
    table_name: String,
    columns: Vec<(String, String)>,
    upstream: &Rc<dyn Stream<PyElement>>,
) -> PyResult<Rc<dyn Node>> {
    let consumer = Box::new(
        move |_ctx: wingfoil::RunParams, source: Pin<Box<dyn FutStream<PyElement>>>| {
            py_kdb_write_consumer(connection, table_name, columns, source)
        },
    );

    Ok(upstream.consume_async(consumer))
}

async fn py_kdb_write_consumer(
    connection: KdbConnection,
    table_name: String,
    columns: Vec<(String, String)>,
    mut source: Pin<Box<dyn FutStream<PyElement>>>,
) -> anyhow::Result<()> {
    use chrono::NaiveDateTime;

    let creds = connection.credentials_string();
    let mut socket = QStream::connect(
        ConnectionMethod::TCP,
        &connection.host,
        connection.port,
        &creds,
    )
    .await?;

    while let Some((time, py_element)) = source.next().await {
        // Format the KDB timestamp from NanoTime.
        let naive: NaiveDateTime = time.into();
        let ts_str = naive.format("%Y.%m.%dD%H:%M:%S%.9f").to_string();

        // Extract values from Python dict and build q string column fragments.
        let mut col_frags: Vec<String> = vec![format!("enlist {ts_str}")];

        Python::attach(|py| -> anyhow::Result<()> {
            let obj = py_element.as_ref().bind(py);
            let dict: &pyo3::Bound<'_, PyDict> = obj
                .cast::<PyDict>()
                .map_err(|e| anyhow::anyhow!("expected dict, got: {e}"))?;

            for (col_name, col_type) in &columns {
                let value = dict
                    .get_item(col_name)?
                    .ok_or_else(|| anyhow::anyhow!("missing column '{col_name}' in dict"))?;

                let frag = match col_type.as_str() {
                    "symbol" => {
                        let s: String = value.extract()?;
                        format!("enlist`{s}")
                    }
                    "float" => {
                        let f: f64 = value.extract()?;
                        format!("enlist {f}f")
                    }
                    "long" => {
                        let l: i64 = value.extract()?;
                        format!("enlist {l}j")
                    }
                    "int" => {
                        let i: i32 = value.extract()?;
                        format!("enlist {i}i")
                    }
                    "bool" => {
                        let b: bool = value.extract()?;
                        format!("enlist {}b", if b { 1 } else { 0 })
                    }
                    other => {
                        anyhow::bail!("unsupported column type '{other}' for column '{col_name}'");
                    }
                };
                col_frags.push(frag);
            }
            Ok(())
        })?;

        // Build and send a q string insert: insert[`table; (enlist ts; enlist `sym; ...)]
        let q = format!(
            "insert[`{table_name}; ({cols})]",
            cols = col_frags.join("; ")
        );
        let response = socket.send_sync_message(&q.as_str()).await?;
        if response.get_type() == kdb_plus_fixed::qtype::ERROR {
            let msg = response.get_error_string().unwrap_or("unknown").to_string();
            anyhow::bail!("KDB insert error: {msg}");
        }
    }

    Ok(())
}
