//! Python bindings for the PostgreSQL adapter.

use crate::PyNode;
use crate::py_element::PyElement;
use crate::py_stream::PyStream;
use crate::types::{DICT_INSERT_INFALLIBLE, INTO_PY_INFALLIBLE};

use anyhow::bail;
use chrono::NaiveDateTime;
use pyo3::conversion::IntoPyObject;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::rc::Rc;
use wingfoil::adapters::postgres::{
    PostgresConnection, PostgresDeserialize, PostgresRowExt, PostgresSerialize, Row, ToSql, Type,
    postgres_notify_trigger_sql, postgres_read, postgres_sub, postgres_timestamp, postgres_write,
    quote_ident,
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
    /// A timestamp rendered with [`postgres_timestamp`] formatting.
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

/// Read a single column, dispatching on the column's declared SQL type.
///
/// The type is resolved from the row metadata (no per-cell trial and error), and an
/// unsupported column type is an **error** — not a silent `None` — so a schema
/// mismatch (e.g. a `numeric` or `uuid` column) aborts on the first row instead of
/// producing rows full of nulls that are indistinguishable from SQL NULL.
fn column_value(row: &Row, idx: usize) -> anyhow::Result<PyPgValue> {
    let ty = row.columns()[idx].type_();
    let value = if *ty == Type::BOOL {
        row.try_get::<_, Option<bool>>(idx)?
            .map_or(PyPgValue::Null, PyPgValue::Bool)
    } else if *ty == Type::INT2 {
        row.try_get::<_, Option<i16>>(idx)?
            .map_or(PyPgValue::Null, |i| PyPgValue::Int(i as i64))
    } else if *ty == Type::INT4 {
        row.try_get::<_, Option<i32>>(idx)?
            .map_or(PyPgValue::Null, |i| PyPgValue::Int(i as i64))
    } else if *ty == Type::INT8 {
        row.try_get::<_, Option<i64>>(idx)?
            .map_or(PyPgValue::Null, PyPgValue::Int)
    } else if *ty == Type::FLOAT4 {
        row.try_get::<_, Option<f32>>(idx)?
            .map_or(PyPgValue::Null, |f| PyPgValue::Float(f as f64))
    } else if *ty == Type::FLOAT8 {
        row.try_get::<_, Option<f64>>(idx)?
            .map_or(PyPgValue::Null, PyPgValue::Float)
    } else if *ty == Type::TEXT || *ty == Type::VARCHAR || *ty == Type::BPCHAR || *ty == Type::NAME
    {
        row.try_get::<_, Option<String>>(idx)?
            .map_or(PyPgValue::Null, PyPgValue::Text)
    } else if *ty == Type::TIMESTAMP {
        row.try_get::<_, Option<NaiveDateTime>>(idx)?
            .map_or(PyPgValue::Null, |t| {
                PyPgValue::Timestamp(postgres_timestamp(t.into()))
            })
    } else if *ty == Type::TIMESTAMPTZ {
        row.try_get::<_, Option<chrono::DateTime<chrono::Utc>>>(idx)?
            .map_or(PyPgValue::Null, |t| {
                PyPgValue::Timestamp(postgres_timestamp(t.naive_utc().into()))
            })
    } else if *ty == Type::BYTEA {
        row.try_get::<_, Option<Vec<u8>>>(idx)?
            .map_or(PyPgValue::Null, PyPgValue::Bytes)
    } else {
        bail!(
            "postgres_read: unsupported column type `{ty}` for column `{}`; supported: \
             bool, int2/int4/int8, float4/float8, text/varchar, timestamp/timestamptz, bytea",
            row.columns()[idx].name()
        );
    };
    Ok(value)
}

impl PostgresDeserialize for PyPgRow {
    fn from_row(row: &Row) -> anyhow::Result<(NanoTime, Self)> {
        // Convention: the query selects its timestamp column first (col 0).
        let time = row.get_nanotime(0)?;
        let columns = row
            .columns()
            .iter()
            .enumerate()
            .map(|(i, col)| Ok((col.name().to_string(), column_value(row, i)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok((time, PyPgRow { columns }))
    }
}

/// Read a time-partitioned PostgreSQL table.
///
/// The `query` is wrapped as a subquery and filtered per time slice on `time_col`.
/// **The query must select `time_col` as its first column** so it can be extracted
/// as the on-graph timestamp. Each tick yields a `dict` of `{column_name: value}`.
///
/// `time_col` is identifier-quoted, so it must match the column name exactly as
/// stored (lower-case unless the column was created quoted). A trailing `;` on the
/// query is stripped. A column whose SQL type is unsupported (e.g. `numeric`,
/// `uuid`, `json`) fails the run on the first row rather than reading as `None`.
///
/// Args:
///     conn_str: libpq connection string, e.g.
///         `"host=localhost user=postgres password=postgres dbname=postgres"`
///     query: SQL selecting the rows (time column first), e.g.
///         `"SELECT time, sym, price FROM trades"`
///     time_col: name of the `timestamp`/`timestamptz` column used for slicing
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
    // Strip a trailing `;` so `SELECT ...;` survives the subquery wrap below.
    let query = query.trim().trim_end_matches(';').trim_end().to_string();
    let tc = quote_ident(&time_col);
    let stream: Rc<dyn Stream<Burst<PyPgRow>>> = postgres_read::<PyPgRow, _>(
        conn,
        std::time::Duration::from_secs(chunk_size),
        move |(t0, t1), _date, _iter| {
            format!(
                "SELECT sub.* FROM ({q}) AS sub \
                 WHERE sub.{tc} >= '{t0}' AND sub.{tc} < '{t1}' ORDER BY sub.{tc}",
                q = query,
                tc = tc,
                t0 = postgres_timestamp(t0),
                t1 = postgres_timestamp(t1),
            )
        },
    );

    rows_to_py_stream(stream)
}

/// Collapse a burst-of-rows stream and marshal each row into a Python dict.
fn rows_to_py_stream(stream: Rc<dyn Stream<Burst<PyPgRow>>>) -> PyStream {
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

/// Live-tail a PostgreSQL table in real time using `LISTEN`/`NOTIFY`.
///
/// The `query` is wrapped as a subquery and re-run past a time cursor whenever a
/// notification arrives on `channel` (notifications are a wake-up signal only — rows
/// always come from the query). **The query must select `time_col` as its first
/// column.** Each tick yields a `dict` of `{column_name: value}`.
///
/// Install the notify trigger on the table first — `postgres_notify_trigger_sql`
/// returns the SQL. Requires a real-time run (`realtime=True`); the time column must
/// be non-decreasing across inserts, and rows time-stamped at or before the cursor
/// are never picked up.
///
/// Args:
///     conn_str: libpq connection string
///     query: SQL selecting the rows (time column first), e.g.
///         `"SELECT time, sym, price FROM trades"`
///     time_col: name of the `timestamp`/`timestamptz` cursor column
///     channel: NOTIFY channel to LISTEN on (must match the trigger)
///     start: Unix seconds to start the cursor from (default 0.0 = emit all
///         existing rows as a catch-up, then tail live inserts)
#[pyfunction]
#[pyo3(signature = (conn_str, query, time_col, channel, start=0.0))]
pub fn py_postgres_sub(
    conn_str: String,
    query: String,
    time_col: String,
    channel: String,
    start: f64,
) -> PyStream {
    let conn = PostgresConnection::new(conn_str);
    // Strip a trailing `;` so `SELECT ...;` survives the subquery wrap below.
    let query = query.trim().trim_end_matches(';').trim_end().to_string();
    let tc = quote_ident(&time_col);
    let start_from = NanoTime::new((start * 1e9) as u64);
    let stream: Rc<dyn Stream<Burst<PyPgRow>>> =
        postgres_sub::<PyPgRow, _>(conn, channel, start_from, move |cursor| {
            format!(
                "SELECT sub.* FROM ({q}) AS sub \
                 WHERE sub.{tc} > '{c}' ORDER BY sub.{tc}",
                q = query,
                tc = tc,
                c = postgres_timestamp(cursor),
            )
        });
    rows_to_py_stream(stream)
}

/// SQL that installs an AFTER INSERT trigger firing `pg_notify(channel, '')`.
///
/// Run it against the database (e.g. with psycopg) to wire a table up for
/// `postgres_sub`. Idempotent.
#[pyfunction]
pub fn py_postgres_notify_trigger_sql(table: String, channel: String) -> String {
    postgres_notify_trigger_sql(&table, &channel)
}

/// A single write row: business column values in table order (after time).
#[derive(Debug, Clone, Default)]
struct PyPgWriteRow {
    values: Vec<PgParam>,
}

/// An owned, typed parameter produced from a Python value.
///
/// `None` inside a variant is a **typed** SQL NULL, so nullable columns bind with the
/// correct parameter type regardless of the column's SQL type.
#[derive(Debug, Clone)]
enum PgParam {
    Bool(Option<bool>),
    Int4(Option<i32>),
    Int8(Option<i64>),
    Float4(Option<f32>),
    Float8(Option<f64>),
    Text(Option<String>),
    Bytes(Option<Vec<u8>>),
}

impl PgParam {
    fn boxed(&self) -> Box<dyn ToSql + Sync + Send> {
        match self {
            PgParam::Bool(v) => Box::new(*v),
            PgParam::Int4(v) => Box::new(*v),
            PgParam::Int8(v) => Box::new(*v),
            PgParam::Float4(v) => Box::new(*v),
            PgParam::Float8(v) => Box::new(*v),
            PgParam::Text(v) => Box::new(v.clone()),
            PgParam::Bytes(v) => Box::new(v.clone()),
        }
    }
}

impl PostgresSerialize for PyPgWriteRow {
    fn to_params(&self) -> Vec<Box<dyn ToSql + Sync + Send>> {
        self.values.iter().map(PgParam::boxed).collect()
    }
}

/// Extract the declared columns from a Python dict into an ordered write row.
///
/// Fails loudly: a missing key, an unsupported declared type, or a wrong-typed value
/// is an error that aborts the run — never a silently-inserted NULL. Pass an explicit
/// Python `None` to write a SQL NULL.
fn dict_to_write_row(
    dict: &Bound<'_, PyDict>,
    columns: &[(String, String)],
) -> anyhow::Result<PyPgWriteRow> {
    let values = columns
        .iter()
        .map(|(name, ty)| {
            let value = dict
                .get_item(name)
                .map_err(|e| anyhow::anyhow!("postgres_write: reading key '{name}': {e}"))?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "postgres_write: missing column '{name}' in dict \
                         (pass an explicit None to write SQL NULL)"
                    )
                })?;
            // Extract Some(T) from the Python value, or None for Python None.
            macro_rules! extract {
                ($t:ty) => {
                    if value.is_none() {
                        None
                    } else {
                        Some(value.extract::<$t>().map_err(|e| {
                            anyhow::anyhow!("postgres_write: column '{name}' ({ty}): {e}")
                        })?)
                    }
                };
            }
            Ok(match ty.as_str() {
                "bool" => PgParam::Bool(extract!(bool)),
                "int" | "int4" | "integer" => PgParam::Int4(extract!(i32)),
                "long" | "int8" | "bigint" => PgParam::Int8(extract!(i64)),
                "float4" | "real" => PgParam::Float4(extract!(f32)),
                "float" | "float8" | "double" => PgParam::Float8(extract!(f64)),
                "text" | "str" => PgParam::Text(extract!(String)),
                "bytes" | "bytea" => PgParam::Bytes(extract!(Vec<u8>)),
                other => bail!(
                    "postgres_write: unsupported column type '{other}' for column '{name}'; \
                     supported: bool, int/int4/integer, long/int8/bigint, float4/real, \
                     float/float8/double, text/str, bytes/bytea"
                ),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(PyPgWriteRow { values })
}

/// Inner implementation for the `.postgres_write()` stream method.
///
/// The stream must yield either a single `dict` of the declared columns, or a `list`
/// of such dicts for multiple rows per tick. The graph timestamp is prepended.
/// Marshaling failures abort the run (see [`dict_to_write_row`]).
pub fn py_postgres_write_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    conn_str: String,
    table: String,
    columns: Vec<(String, String)>,
) -> Rc<dyn Node> {
    let conn = PostgresConnection::new(conn_str);

    let burst_stream: Rc<dyn Stream<Burst<PyPgWriteRow>>> = stream.try_map(move |elem| {
        Python::attach(|py| -> anyhow::Result<Burst<PyPgWriteRow>> {
            let obj = elem.as_ref().bind(py);
            if let Ok(dict) = obj.cast_exact::<PyDict>() {
                Ok(std::iter::once(dict_to_write_row(dict, &columns)?).collect())
            } else if let Ok(list) = obj.cast_exact::<PyList>() {
                list.iter()
                    .map(|item| {
                        let dict = item.cast_exact::<PyDict>().map_err(|_| {
                            anyhow::anyhow!(
                                "postgres_write: list items must be dicts, got {}",
                                item.get_type()
                            )
                        })?;
                        dict_to_write_row(dict, &columns)
                    })
                    .collect::<anyhow::Result<Burst<PyPgWriteRow>>>()
            } else {
                bail!(
                    "postgres_write: stream value must be a dict or list of dicts, got {}",
                    obj.get_type()
                );
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
