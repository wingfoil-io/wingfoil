//! Python bindings for the PostgreSQL adapter.

use crate::PyNode;
use crate::py_element::PyElement;
use crate::py_stream::PyStream;
use crate::types::{DICT_INSERT_INFALLIBLE, INTO_PY_INFALLIBLE};

use chrono::NaiveDateTime;
use pyo3::conversion::IntoPyObject;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::rc::Rc;
use wingfoil::adapters::postgres::{
    PostgresConnection, PostgresDeserialize, PostgresRowExt, PostgresSerialize, Row, ToSql,
    postgres_read, postgres_write,
};
use wingfoil::{Burst, NanoTime, Node, Stream, StreamOperators};

/// A deserialized PostgreSQL row stored as column name/value pairs.
/// Each row becomes a Python dict.
#[derive(Debug, Clone, Default)]
struct PyPgRow {
    columns: Vec<(String, PyPgValue)>,
}

/// Intermediate value type for PostgreSQL -> Python conversion.
#[derive(Debug, Clone, Default)]
enum PyPgValue {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    /// A `timestamp` rendered as an ISO-8601 string.
    Timestamp(String),
}

impl PyPgValue {
    fn to_py(&self, py: Python<'_>) -> Py<PyAny> {
        match self {
            PyPgValue::Null => py.None(),
            PyPgValue::Bool(v) => v
                .into_pyobject(py)
                .expect(INTO_PY_INFALLIBLE)
                .to_owned()
                .into_any()
                .unbind(),
            PyPgValue::Int(v) => v
                .into_pyobject(py)
                .expect(INTO_PY_INFALLIBLE)
                .into_any()
                .unbind(),
            PyPgValue::Float(v) => v
                .into_pyobject(py)
                .expect(INTO_PY_INFALLIBLE)
                .into_any()
                .unbind(),
            PyPgValue::Text(v) => v
                .into_pyobject(py)
                .expect(INTO_PY_INFALLIBLE)
                .into_any()
                .unbind(),
            PyPgValue::Bytes(v) => PyBytes::new(py, v).into_any().unbind(),
            PyPgValue::Timestamp(v) => v
                .into_pyobject(py)
                .expect(INTO_PY_INFALLIBLE)
                .into_any()
                .unbind(),
        }
    }
}

/// Read a single column, probing candidate SQL types in turn.
///
/// `try_get::<Option<T>>` returns `Ok(None)` for SQL NULL and `Err` when the column
/// type is incompatible with `T`, so probing in order resolves the value without
/// knowing the column type up front.
fn column_value(row: &Row, idx: usize) -> PyPgValue {
    if let Ok(v) = row.try_get::<_, Option<bool>>(idx) {
        return v.map_or(PyPgValue::Null, PyPgValue::Bool);
    }
    if let Ok(v) = row.try_get::<_, Option<i32>>(idx) {
        return v.map_or(PyPgValue::Null, |i| PyPgValue::Int(i as i64));
    }
    if let Ok(v) = row.try_get::<_, Option<i64>>(idx) {
        return v.map_or(PyPgValue::Null, PyPgValue::Int);
    }
    if let Ok(v) = row.try_get::<_, Option<f32>>(idx) {
        return v.map_or(PyPgValue::Null, |f| PyPgValue::Float(f as f64));
    }
    if let Ok(v) = row.try_get::<_, Option<f64>>(idx) {
        return v.map_or(PyPgValue::Null, PyPgValue::Float);
    }
    if let Ok(v) = row.try_get::<_, Option<String>>(idx) {
        return v.map_or(PyPgValue::Null, PyPgValue::Text);
    }
    if let Ok(v) = row.try_get::<_, Option<NaiveDateTime>>(idx) {
        return v.map_or(PyPgValue::Null, |t| {
            PyPgValue::Timestamp(t.format("%Y-%m-%d %H:%M:%S%.6f").to_string())
        });
    }
    if let Ok(v) = row.try_get::<_, Option<Vec<u8>>>(idx) {
        return v.map_or(PyPgValue::Null, PyPgValue::Bytes);
    }
    PyPgValue::Null
}

impl PostgresDeserialize for PyPgRow {
    fn from_row(row: &Row) -> anyhow::Result<(NanoTime, Self)> {
        // Convention: the query selects its timestamp column first (col 0).
        let time = row.get_nanotime(0)?;
        let columns = row
            .columns()
            .iter()
            .enumerate()
            .map(|(i, col)| (col.name().to_string(), column_value(row, i)))
            .collect();
        Ok((time, PyPgRow { columns }))
    }
}

/// Read a time-partitioned PostgreSQL table.
///
/// The `query` is wrapped as a subquery and filtered per time slice on `time_col`.
/// **The query must select `time_col` as its first column** so it can be extracted
/// as the on-graph timestamp. Each tick yields a `dict` of `{column_name: value}`.
///
/// Args:
///     conn_str: libpq connection string, e.g.
///         `"host=localhost user=postgres password=postgres dbname=postgres"`
///     query: SQL selecting the rows (time column first), e.g.
///         `"SELECT time, sym, price FROM trades"`
///     time_col: name of the `timestamp` column used for slicing
///     chunk_size: duration of each time slice in seconds (default: 3600)
///
/// Requires RunMode::HistoricalFrom with a non-zero start time and RunFor::Duration.
#[pyfunction]
#[pyo3(signature = (conn_str, query, time_col, chunk_size=3600))]
pub fn py_postgres_read(
    conn_str: String,
    query: String,
    time_col: String,
    chunk_size: u64,
) -> PyStream {
    let conn = PostgresConnection::new(conn_str);
    let stream: Rc<dyn Stream<Burst<PyPgRow>>> = postgres_read::<PyPgRow, _>(
        conn,
        std::time::Duration::from_secs(chunk_size),
        move |(t0, t1), _date, _iter| {
            use wingfoil::adapters::postgres::postgres_timestamp;
            format!(
                "SELECT sub.* FROM ({q}) AS sub \
                 WHERE sub.{tc} >= '{t0}' AND sub.{tc} < '{t1}' ORDER BY sub.{tc}",
                q = query,
                tc = time_col,
                t0 = postgres_timestamp(t0),
                t1 = postgres_timestamp(t1),
            )
        },
    );

    let py_stream = stream.collapse().map(|row: PyPgRow| {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            for (name, value) in &row.columns {
                dict.set_item(name, value.to_py(py))
                    .expect(DICT_INSERT_INFALLIBLE);
            }
            PyElement::new(dict.into_any().unbind())
        })
    });

    PyStream(py_stream)
}

/// A single write row: business column values in table order (after time).
#[derive(Debug, Clone, Default)]
struct PyPgWriteRow {
    values: Vec<PgParam>,
}

/// An owned, `ToSql`-able parameter produced from a Python value.
#[derive(Debug, Clone)]
enum PgParam {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Null,
}

impl PgParam {
    fn boxed(&self) -> Box<dyn ToSql + Sync + Send> {
        match self {
            PgParam::Bool(b) => Box::new(*b),
            PgParam::Int(i) => Box::new(*i),
            PgParam::Float(f) => Box::new(*f),
            PgParam::Text(s) => Box::new(s.clone()),
            PgParam::Bytes(b) => Box::new(b.clone()),
            PgParam::Null => Box::new(Option::<String>::None),
        }
    }
}

impl PostgresSerialize for PyPgWriteRow {
    fn to_params(&self) -> Vec<Box<dyn ToSql + Sync + Send>> {
        self.values.iter().map(PgParam::boxed).collect()
    }
}

/// Extract the declared columns from a Python dict into an ordered write row.
fn dict_to_write_row(dict: &Bound<'_, PyDict>, columns: &[(String, String)]) -> PyPgWriteRow {
    let values = columns
        .iter()
        .map(|(name, ty)| {
            let item = dict.get_item(name).ok().flatten();
            match item {
                None => PgParam::Null,
                Some(value) if value.is_none() => PgParam::Null,
                Some(value) => match ty.as_str() {
                    "bool" => value.extract::<bool>().map(PgParam::Bool),
                    "int" | "long" => value.extract::<i64>().map(PgParam::Int),
                    "float" | "double" => value.extract::<f64>().map(PgParam::Float),
                    "text" | "str" => value.extract::<String>().map(PgParam::Text),
                    "bytes" | "bytea" => value.extract::<Vec<u8>>().map(PgParam::Bytes),
                    other => {
                        log::error!("postgres_write: unsupported column type '{other}'");
                        Ok(PgParam::Null)
                    }
                }
                .unwrap_or_else(|e| {
                    log::error!("postgres_write: column '{name}' extract failed: {e}");
                    PgParam::Null
                }),
            }
        })
        .collect();
    PyPgWriteRow { values }
}

/// Inner implementation for the `.postgres_write()` stream method.
///
/// The stream must yield either a single `dict` of the declared columns, or a `list`
/// of such dicts for multiple rows per tick. The graph timestamp is prepended.
pub fn py_postgres_write_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    conn_str: String,
    table: String,
    columns: Vec<(String, String)>,
) -> Rc<dyn Node> {
    let conn = PostgresConnection::new(conn_str);

    let burst_stream: Rc<dyn Stream<Burst<PyPgWriteRow>>> = stream.map(move |elem| {
        Python::attach(|py| {
            let obj = elem.as_ref().bind(py);
            if let Ok(dict) = obj.cast_exact::<PyDict>() {
                std::iter::once(dict_to_write_row(dict, &columns)).collect()
            } else if let Ok(list) = obj.cast_exact::<PyList>() {
                list.iter()
                    .filter_map(|item| {
                        item.cast_exact::<PyDict>()
                            .ok()
                            .map(|d| dict_to_write_row(d, &columns))
                    })
                    .collect()
            } else {
                log::error!("postgres_write: stream value must be a dict or list of dicts");
                Burst::new()
            }
        })
    });

    postgres_write(conn, table, &burst_stream)
}

/// Write this stream to a PostgreSQL table.
///
/// See [`PyStream::postgres_write`] — this pyfunction form exists for symmetry with
/// `py_postgres_read`; end users normally call the `.postgres_write()` method.
#[pyfunction]
#[pyo3(signature = (conn_str, table, columns, upstream))]
pub fn py_postgres_write(
    conn_str: String,
    table: String,
    columns: Vec<(String, String)>,
    upstream: &PyStream,
) -> PyNode {
    PyNode::new(py_postgres_write_inner(
        &upstream.0,
        conn_str,
        table,
        columns,
    ))
}
