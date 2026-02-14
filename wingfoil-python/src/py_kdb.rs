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
use kdb_plus_fixed::ipc::{ConnectionMethod, K, QStream};
use wingfoil::adapters::kdb::*;
use wingfoil::{Burst, FutStream, Node, Stream, StreamOperators};

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
    ) -> Result<Self, KdbError> {
        let mut result = Vec::with_capacity(columns.len() - 1);

        // Skip column 0 (time) - handled by adapter
        for (i, col_name) in columns.iter().enumerate().skip(1) {
            let k = row.get(i)?;
            let value = if let Ok(v) = k.get_long() {
                PyKdbValue::Long(v)
            } else if let Ok(v) = k.get_float() {
                PyKdbValue::Float(v)
            } else if let Ok(v) = k.get_symbol() {
                PyKdbValue::Symbol(v.to_string())
            } else if let Ok(v) = k.get_bool() {
                PyKdbValue::Bool(v)
            } else if let Ok(v) = k.get_int() {
                PyKdbValue::Int(v)
            } else if let Ok(v) = k.get_real() {
                PyKdbValue::Real(v)
            } else {
                PyKdbValue::Symbol(format!("{:?}", k))
            };
            result.push((col_name.clone(), value));
        }

        Ok(PyKdbRow { columns: result })
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
///     time_col: Name of the time column for chunking
///     chunk_size: Maximum rows per query chunk (controls memory usage)
///
/// Returns:
///     Stream where each tick yields a dict of {column_name: value}
#[pyfunction]
#[pyo3(signature = (host, port, query, time_col, chunk_size=10000))]
pub fn py_kdb_read(
    host: String,
    port: u16,
    query: String,
    time_col: String,
    chunk_size: usize,
) -> PyStream {
    let conn = KdbConnection::new(host, port);
    let stream: Rc<dyn Stream<Burst<PyKdbRow>>> =
        kdb_read::<PyKdbRow>(conn, query, time_col, chunk_size);

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
    let consumer = Box::new(move |source: Pin<Box<dyn FutStream<PyElement>>>| {
        py_kdb_write_consumer(connection, table_name, columns, source)
    });

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
        // Convert NanoTime to proper KDB timestamp
        let naive: NaiveDateTime = time.into();
        let mut row_values = vec![K::new_timestamp(naive.and_utc())];

        // Extract values from Python dict based on column spec
        Python::attach(|py| -> anyhow::Result<()> {
            let obj = py_element.as_ref().bind(py);
            let dict: &pyo3::Bound<'_, PyDict> = obj
                .cast::<PyDict>()
                .map_err(|e| anyhow::anyhow!("expected dict, got: {}", e))?;

            for (col_name, col_type) in &columns {
                let value = dict
                    .get_item(col_name)?
                    .ok_or_else(|| anyhow::anyhow!("missing column '{}' in dict", col_name))?;

                let k = match col_type.as_str() {
                    "symbol" => {
                        let s: String = value.extract()?;
                        K::new_symbol(s)
                    }
                    "float" => {
                        let f: f64 = value.extract()?;
                        K::new_float(f)
                    }
                    "long" => {
                        let l: i64 = value.extract()?;
                        K::new_long(l)
                    }
                    "int" => {
                        let i: i32 = value.extract()?;
                        K::new_int(i)
                    }
                    "bool" => {
                        let b: bool = value.extract()?;
                        K::new_bool(b)
                    }
                    other => {
                        anyhow::bail!(
                            "unsupported column type '{}' for column '{}'",
                            other,
                            col_name
                        );
                    }
                };
                row_values.push(k);
            }
            Ok(())
        })?;

        let full_row = K::new_compound_list(row_values);

        let query = K::new_compound_list(vec![
            K::new_string("insert".to_string(), kdb_plus_fixed::qattribute::NONE),
            K::new_symbol(table_name.clone()),
            full_row,
        ]);

        socket.send_sync_message(&query).await?;
    }

    Ok(())
}
