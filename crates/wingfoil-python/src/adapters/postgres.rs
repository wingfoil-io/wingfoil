//! Python bindings for the wingfoil **PostgreSQL** adapter
//! ([`wingfoil::adapters::postgres`]).
//!
//! Four graph entry points plus one helper, all `#[pyadapter]`-generated:
//!
//! | Python                          | Rust                | shape |
//! |---------------------------------|---------------------|-------|
//! | `postgres_read(graph, …)`       | [`pg_read`]         | historical, time-sliced replay |
//! | `postgres_sub(graph, …)`        | [`pg_sub`]          | real-time `LISTEN`/`NOTIFY` live tail |
//! | `postgres_source(graph, …)`     | [`pg_source`]       | mode-agnostic — dispatches on the run mode |
//! | `postgres_write(stream, …)`     | [`PostgresSinkOps`] | streaming insert sink |
//! | `postgres_notify_trigger_sql()` | —                   | the trigger DDL `postgres_sub` needs |
//!
//! # The dynamic edge
//!
//! A Rust caller writes a record `struct` and implements
//! [`PostgresDeserialize`] / [`PostgresSerialize`] on it. Python has no such
//! struct, so these bindings supply two *dynamic* record types instead:
//!
//! - **reads** decode into [`PyPgRow`] — column name/value pairs resolved from
//!   the row's own type metadata — which erases to a Python **`dict`**;
//! - **writes** take the caller's declared `columns` (name + SQL type, in table
//!   order) and marshal each Python `dict` into a typed `PyPgWriteRow`.
//!
//! Both intermediates are plain Rust data (no `Py<PyAny>` inside), which is what
//! lets them cross to the adapter's own worker thread — the reason for the
//! [`PyPgValue`] / `PgParam` indirection rather than passing Python objects
//! straight through.
//!
//! Every source is burst-shaped (`Stream<Burst<PyPgRow>>`), so each tick yields
//! a Python **`list` of row dicts** — the same-instant group, losslessly.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **`postgres_read` yields a list per tick, not a single dict.** Legacy
//!    `collapse()`d the burst and kept only the last row of any timestamp, which
//!    silently dropped rows sharing a timestamp (its own docstring warned about
//!    it). Bursts erase to lists uniformly here, so the read is lossless and
//!    matches `postgres_sub`'s shape. Call `[0]` for the single-row case.
//! 2. **The run window is an argument.** The Rust `postgres_read` takes the
//!    run's `[start, end)` at *wiring* time (slicing is a pure, fail-fast
//!    check), and a Python `Graph` does not know its run mode until `run()` — so
//!    `start_nanos` / `duration_nanos` are passed explicitly and must match the
//!    eventual `graph.run(...)`.
//! 3. **New capabilities**: `postgres_source` (one wiring call, either run mode)
//!    and the `buffer_size` back-pressure bound, neither of which legacy had.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use chrono::NaiveDateTime;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use wingfoil::NanoTime;
use wingfoil::adapters::postgres::{
    PostgresConnection, PostgresDeserialize, PostgresRowExt, PostgresSerialize, PostgresSinkOps,
    PostgresSourceConfig, Row, ToSql, Type, postgres_notify_trigger_sql as pg_notify_trigger_sql,
    postgres_read as pg_read, postgres_source as pg_source, postgres_sub as pg_sub,
    postgres_timestamp, quote_ident,
};
use wingfoil::prelude::{Burst, GraphBuilder, Stream, StreamOps};

use crate::adapters::common::{historical_params, realtime_params, run_mode, secs_to_nanotime};
use crate::{PyElement, pyadapter};

// ---------------------------------------------------------------------------
// Reading: a row -> a Python dict.
// ---------------------------------------------------------------------------

/// One decoded column value on its way from PostgreSQL to Python.
///
/// Deliberately *not* a `Py<PyAny>`: rows are decoded on the adapter's async
/// worker and handed to the graph across a channel, so the intermediate must be
/// plain `Send` Rust data. The Python object is built later, on the graph
/// thread, by [`PyPgValue::to_py`].
#[derive(Debug, Clone, Default, PartialEq)]
pub enum PyPgValue {
    /// SQL `NULL` — Python `None`.
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    /// A `timestamp`/`timestamptz` rendered with [`postgres_timestamp`]
    /// formatting, so it round-trips into a query predicate verbatim.
    Timestamp(String),
}

impl PyPgValue {
    /// Build the Python object for this value.
    fn to_py(&self, py: Python<'_>) -> Py<PyAny> {
        /// Box a scalar. Primitive `IntoPyObject` conversions are infallible,
        /// so the `expect` is an unreachable invariant. `to_owned()` covers the
        /// borrowed results (`bool` interns `True`/`False`).
        macro_rules! boxed {
            ($v:expr) => {
                $v.into_pyobject(py)
                    .map(|b| b.to_owned().into_any().unbind())
                    .expect("invariant: scalar -> PyObject conversion is infallible")
            };
        }
        match self {
            PyPgValue::Null => py.None(),
            PyPgValue::Bool(v) => boxed!(*v),
            PyPgValue::Int(v) => boxed!(*v),
            PyPgValue::Float(v) => boxed!(*v),
            PyPgValue::Text(v) => boxed!(v.as_str()),
            PyPgValue::Bytes(v) => PyBytes::new(py, v).into_any().unbind(),
            PyPgValue::Timestamp(v) => boxed!(v.as_str()),
        }
    }
}

/// A deserialized PostgreSQL row as column name/value pairs — the dynamic
/// stand-in for the record `struct` a Rust caller would write. Erases to a
/// Python `dict`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PyPgRow {
    columns: Vec<(String, PyPgValue)>,
}

impl PyPgRow {
    /// The decoded columns, in the order the query selected them.
    pub fn columns(&self) -> &[(String, PyPgValue)] {
        &self.columns
    }
}

/// Decode one column, dispatching on its **declared** SQL type.
///
/// The type comes from the row metadata (no per-cell trial and error), and an
/// unsupported column type is an **error** — not a silent `None` — so a schema
/// mismatch (a `numeric`, `uuid` or `json` column, say) aborts on the first row
/// instead of producing rows full of nulls indistinguishable from SQL `NULL`.
fn column_value(row: &Row, idx: usize) -> Result<PyPgValue> {
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
    fn from_row(row: &Row) -> Result<(NanoTime, Self)> {
        // Convention (as legacy): the query selects its timestamp column first.
        let time = row.get_nanotime(0)?;
        let columns = row
            .columns()
            .iter()
            .enumerate()
            .map(|(i, col)| Ok((col.name().to_string(), column_value(row, i)?)))
            .collect::<Result<Vec<_>>>()?;
        Ok((time, PyPgRow { columns }))
    }
}

impl From<PyPgRow> for PyElement {
    fn from(row: PyPgRow) -> Self {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            for (name, value) in &row.columns {
                dict.set_item(name, value.to_py(py))
                    .expect("invariant: inserting a str key into a fresh PyDict cannot fail");
            }
            PyElement::new(dict.into_any().unbind())
        })
    }
}

// ---------------------------------------------------------------------------
// Query construction — the base query wrapped as a filtered subquery.
// ---------------------------------------------------------------------------

/// Strip a trailing `;` so `SELECT …;` survives the subquery wrap.
fn normalise_query(query: &str) -> String {
    query.trim().trim_end_matches(';').trim_end().to_string()
}

/// The historical slice query: `time >= t0 AND time < t1`, ordered by time —
/// the contract [`pg_read`] requires of its `query_fn`.
fn window_query(
    query: &str,
    time_col: &str,
) -> impl FnMut((NanoTime, NanoTime), i32, usize) -> String + Send + 'static {
    let query = normalise_query(query);
    let tc = quote_ident(time_col);
    move |(t0, t1), _date, _iter| {
        format!(
            "SELECT sub.* FROM ({query}) AS sub \
             WHERE sub.{tc} >= '{t0}' AND sub.{tc} < '{t1}' ORDER BY sub.{tc}",
            t0 = postgres_timestamp(t0),
            t1 = postgres_timestamp(t1),
        )
    }
}

/// The live-tail cursor query: `time > cursor`, ordered by time — the contract
/// [`pg_sub`] requires of its `query_fn`.
fn cursor_query(query: &str, time_col: &str) -> impl FnMut(NanoTime) -> String + Send + 'static {
    let query = normalise_query(query);
    let tc = quote_ident(time_col);
    move |cursor| {
        format!(
            "SELECT sub.* FROM ({query}) AS sub WHERE sub.{tc} > '{c}' ORDER BY sub.{tc}",
            c = postgres_timestamp(cursor),
        )
    }
}

// ---------------------------------------------------------------------------
// Sources.
// ---------------------------------------------------------------------------

/// Read a time-partitioned PostgreSQL table as a bounded historical replay.
///
/// `query` is wrapped as a subquery and filtered per time slice on `time_col`;
/// **it must select `time_col` as its first column** so the row's on-graph
/// timestamp can be extracted. Each tick yields a `list` of `{column: value}`
/// dicts — the rows sharing that timestamp.
///
/// `start_nanos` / `duration_nanos` describe the run this source is wired for
/// and must match the eventual `graph.run(start_nanos=…, duration_nanos=…)`;
/// the window is validated and sliced at wiring, so a bad one raises here.
/// `time_col` is identifier-quoted, so it must match the stored column name
/// exactly (lower-case unless created quoted). A trailing `;` is stripped. An
/// unsupported column type fails the run on the first row rather than reading as
/// `None`. `buffer_size` bounds the replay's look-ahead (`None` = unbounded).
#[pyadapter(name = postgres_read, source)]
#[pyo3(signature = (
    conn_str, query, time_col, start_nanos, duration_nanos,
    chunk_secs = 3600, buffer_size = None,
))]
fn read(
    g: &GraphBuilder,
    conn_str: String,
    query: String,
    time_col: String,
    start_nanos: u64,
    duration_nanos: u64,
    chunk_secs: u64,
    buffer_size: Option<usize>,
) -> Result<Stream<Burst<PyPgRow>>> {
    pg_read::<PyPgRow>(
        g,
        historical_params(start_nanos, duration_nanos),
        PostgresConnection::new(conn_str),
        Duration::from_secs(chunk_secs),
        window_query(&query, &time_col),
        buffer_size,
    )
}

/// Live-tail a PostgreSQL table in real time using `LISTEN`/`NOTIFY`.
///
/// `query` is wrapped as a subquery and re-run past a time cursor whenever a
/// notification arrives on `channel` (notifications are a wake-up signal only —
/// rows always come from the query). **The query must select `time_col` as its
/// first column.** Each tick yields a `list` of `{column: value}` dicts.
///
/// Install the notify trigger on the table first —
/// `postgres_notify_trigger_sql` returns the SQL. `start_secs` is the initial
/// cursor in Unix seconds (`0.0` = replay every existing row as a catch-up, then
/// tail). The time column must be **strictly increasing** across inserts: the
/// cursor query is `time > cursor`, so a row stamped at or before the cursor
/// (including one tying the current max) is dropped.
///
/// `realtime` declares the run mode this source is wired for and must match the
/// eventual `graph.run(realtime=…)`. The live tail has no historical timeline to
/// replay, so `realtime=False` raises here — use `postgres_read` instead.
#[pyadapter(name = postgres_sub, source)]
#[pyo3(signature = (conn_str, query, time_col, channel, start_secs = 0.0, realtime = true))]
fn sub(
    g: &GraphBuilder,
    conn_str: String,
    query: String,
    time_col: String,
    channel: String,
    start_secs: f64,
    realtime: bool,
) -> Result<Stream<Burst<PyPgRow>>> {
    pg_sub::<PyPgRow, _>(
        g,
        run_mode(realtime),
        PostgresConnection::new(conn_str),
        channel,
        secs_to_nanotime(start_secs)?,
        cursor_query(&query, &time_col),
    )
}

/// One PostgreSQL source for **either** run mode: wire once, choose real-time vs
/// historical at `run()`.
///
/// `realtime=False` dispatches to the `postgres_read` mechanism (using
/// `start_nanos` / `duration_nanos` / `chunk_secs` / `buffer_size`);
/// `realtime=True` dispatches to the `postgres_sub` live tail (using `channel` /
/// `start_secs`). Both halves are configured from the same `query` + `time_col`,
/// so the downstream wiring is identical either way — flip `realtime` (and the
/// matching `graph.run(realtime=…)`) and nothing else changes.
///
/// This has no legacy `wingfoil-python` equivalent; it is the Python exposure of
/// next's unified `<adapter>_source`.
#[pyadapter(name = postgres_source, source)]
#[pyo3(signature = (
    conn_str, query, time_col, channel, realtime,
    start_nanos = 0, duration_nanos = 0, start_secs = 0.0,
    chunk_secs = 3600, buffer_size = None,
))]
fn source(
    g: &GraphBuilder,
    conn_str: String,
    query: String,
    time_col: String,
    channel: String,
    realtime: bool,
    start_nanos: u64,
    duration_nanos: u64,
    start_secs: f64,
    chunk_secs: u64,
    buffer_size: Option<usize>,
) -> Result<Stream<Burst<PyPgRow>>> {
    let params = if realtime {
        realtime_params()
    } else {
        historical_params(start_nanos, duration_nanos)
    };
    let cfg = PostgresSourceConfig::new()
        .historical(
            Duration::from_secs(chunk_secs),
            window_query(&query, &time_col),
            buffer_size,
        )
        .live(
            channel,
            secs_to_nanotime(start_secs)?,
            cursor_query(&query, &time_col),
        );
    pg_source::<PyPgRow>(g, params, PostgresConnection::new(conn_str), cfg)
}

// ---------------------------------------------------------------------------
// Writing: a Python dict -> typed SQL parameters.
// ---------------------------------------------------------------------------

/// An owned, typed parameter produced from a Python value.
///
/// `None` inside a variant is a **typed** SQL `NULL`, so a nullable column binds
/// with the right parameter type whatever the column's SQL type.
#[derive(Debug, Clone, PartialEq)]
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

/// One row to insert: the declared columns' values in table order (after time).
#[derive(Debug, Clone, Default, PartialEq)]
struct PyPgWriteRow {
    values: Vec<PgParam>,
}

impl PostgresSerialize for PyPgWriteRow {
    fn to_params(&self) -> Vec<Box<dyn ToSql + Sync + Send>> {
        self.values.iter().map(PgParam::boxed).collect()
    }
}

/// Extract the declared `columns` from a Python dict into an ordered write row.
///
/// Fails loudly: a missing key, an unsupported declared type, or a wrong-typed
/// value aborts the run — never a silently-inserted `NULL`. Pass an explicit
/// Python `None` to write SQL `NULL`.
fn dict_to_write_row(
    dict: &Bound<'_, PyDict>,
    columns: &[(String, String)],
) -> Result<PyPgWriteRow> {
    let values =
        columns
            .iter()
            .map(|(name, ty)| {
                let value = dict
                    .get_item(name)
                    .map_err(|e| anyhow!("postgres_write: reading key '{name}': {e}"))?
                    .ok_or_else(|| {
                        anyhow!(
                            "postgres_write: missing column '{name}' in dict \
                         (pass an explicit None to write SQL NULL)"
                        )
                    })?;
                // `Some(T)` from the Python value, or `None` for Python `None`.
                macro_rules! extract {
                    ($t:ty) => {
                        if value.is_none() {
                            None
                        } else {
                            Some(value.extract::<$t>().map_err(|e| {
                                anyhow!("postgres_write: column '{name}' ({ty}): {e}")
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
            .collect::<Result<Vec<_>>>()?;
    Ok(PyPgWriteRow { values })
}

/// Marshal one erased stream value — a Python `dict` — into a write row.
fn element_to_write_row(elem: &PyElement, columns: &[(String, String)]) -> Result<PyPgWriteRow> {
    if elem.is_none() {
        bail!("postgres_write: stream value is empty (the upstream produced no value)");
    }
    Python::attach(|py| {
        let obj = elem.object().bind(py);
        let dict = obj.cast::<PyDict>().map_err(|_| {
            anyhow!(
                "postgres_write: stream value must be a dict of the declared columns \
                 (or a list of such dicts), got {}",
                obj.get_type()
            )
        })?;
        dict_to_write_row(dict, columns)
    })
}

/// Write this stream to a PostgreSQL table.
///
/// Each tick's value is either a single `dict` of the declared columns, or a
/// `list`/`tuple` of such dicts for several rows at one timestamp. The graph
/// timestamp is prepended, so the target table's columns must line up as
/// `(timestamp, <columns in the declared order>)`.
///
/// `columns` is a list of `(name, sql_type)` pairs in table order; the accepted
/// types are `bool`, `int`/`int4`/`integer`, `long`/`int8`/`bigint`,
/// `float4`/`real`, `float`/`float8`/`double`, `text`/`str` and `bytes`/`bytea`.
/// A missing key, an unsupported type, or a wrong-typed value aborts the run
/// rather than inserting a silent `NULL` — pass an explicit `None` for SQL
/// `NULL`. The connection is opened lazily on the first write, so a connection
/// failure also aborts the run rather than raising at wiring. `buffer_size`
/// bounds the unwritten backlog (`None` = unbounded). Returns a terminal stream
/// whose value is `None`.
///
/// The input stays **erased** (`Burst<PyElement>`) rather than a concrete record
/// type because the row shape is only known from `columns` — which is in scope
/// here, inside the adapter, and not at the `typed_burst_input` seam. The
/// marshaling happens in the `try_map` below; everything downstream of it is
/// natively typed.
#[pyadapter(name = postgres_write)]
#[pyo3(signature = (conn_str, table, columns, buffer_size = None))]
fn write(
    stream: &Stream<Burst<PyElement>>,
    conn_str: String,
    table: String,
    columns: Vec<(String, String)>,
    buffer_size: Option<usize>,
) -> Result<Stream<()>> {
    let rows: Stream<Burst<PyPgWriteRow>> = stream.try_map(move |burst: &Burst<PyElement>| {
        burst
            .iter()
            .map(|elem| element_to_write_row(elem, &columns))
            .collect::<Result<Burst<PyPgWriteRow>>>()
    });
    PostgresSinkOps::postgres_write(
        &rows,
        PostgresConnection::new(conn_str),
        &table,
        buffer_size,
    )
}

/// SQL installing an `AFTER INSERT` trigger that fires `pg_notify(channel, '')`.
///
/// Run it against the database (with psycopg, say) to wire a table up for
/// `postgres_sub`. Idempotent.
#[pyfunction]
#[pyo3(name = "postgres_notify_trigger_sql")]
pub fn postgres_notify_trigger_sql(table: String, channel: String) -> String {
    pg_notify_trigger_sql(&table, &channel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyList;

    /// `columns` as the Python binding receives them.
    fn columns() -> Vec<(String, String)> {
        [("sym", "text"), ("price", "float"), ("qty", "long")]
            .into_iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect()
    }

    fn dict_element(py: Python<'_>, items: &[(&str, Py<PyAny>)]) -> PyElement {
        let dict = PyDict::new(py);
        for (k, v) in items {
            dict.set_item(k, v).unwrap();
        }
        PyElement::new(dict.into_any().unbind())
    }

    #[test]
    fn row_erases_to_a_dict() {
        let row = PyPgRow {
            columns: vec![
                ("sym".into(), PyPgValue::Text("AAPL".into())),
                ("price".into(), PyPgValue::Float(1.5)),
                ("qty".into(), PyPgValue::Int(7)),
                ("flag".into(), PyPgValue::Bool(true)),
                ("blob".into(), PyPgValue::Bytes(vec![1, 2])),
                ("missing".into(), PyPgValue::Null),
            ],
        };
        let element = PyElement::from(row);
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            assert_eq!(
                "AAPL",
                dict.get_item("sym")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap()
            );
            assert_eq!(
                1.5,
                dict.get_item("price")
                    .unwrap()
                    .unwrap()
                    .extract::<f64>()
                    .unwrap()
            );
            assert_eq!(
                7,
                dict.get_item("qty")
                    .unwrap()
                    .unwrap()
                    .extract::<i64>()
                    .unwrap()
            );
            assert!(
                dict.get_item("flag")
                    .unwrap()
                    .unwrap()
                    .extract::<bool>()
                    .unwrap()
            );
            assert_eq!(
                vec![1_u8, 2],
                dict.get_item("blob")
                    .unwrap()
                    .unwrap()
                    .extract::<Vec<u8>>()
                    .unwrap()
            );
            assert!(dict.get_item("missing").unwrap().unwrap().is_none());
        });
    }

    #[test]
    fn dict_marshals_to_declared_column_order() {
        Python::attach(|py| {
            // Deliberately out of table order: the declared `columns` decide.
            let element = dict_element(
                py,
                &[
                    ("qty", 7_i64.into_pyobject(py).unwrap().into_any().unbind()),
                    ("sym", "AAPL".into_pyobject(py).unwrap().into_any().unbind()),
                    (
                        "price",
                        1.5_f64.into_pyobject(py).unwrap().into_any().unbind(),
                    ),
                ],
            );
            let row = element_to_write_row(&element, &columns()).unwrap();
            assert_eq!(
                vec![
                    PgParam::Text(Some("AAPL".into())),
                    PgParam::Float8(Some(1.5)),
                    PgParam::Int8(Some(7)),
                ],
                row.values
            );
        });
    }

    #[test]
    fn explicit_none_is_a_typed_null() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[("sym", py.None()), ("price", py.None()), ("qty", py.None())],
            );
            let row = element_to_write_row(&element, &columns()).unwrap();
            assert_eq!(
                vec![
                    PgParam::Text(None),
                    PgParam::Float8(None),
                    PgParam::Int8(None)
                ],
                row.values
            );
        });
    }

    #[test]
    fn missing_column_errors_rather_than_writing_null() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[("sym", "AAPL".into_pyobject(py).unwrap().into_any().unbind())],
            );
            let err = element_to_write_row(&element, &columns()).unwrap_err();
            assert!(
                err.to_string().contains("missing column 'price'"),
                "unexpected error: {err}"
            );
        });
    }

    #[test]
    fn wrong_typed_value_errors() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[
                    ("sym", "AAPL".into_pyobject(py).unwrap().into_any().unbind()),
                    (
                        "price",
                        "not-a-number"
                            .into_pyobject(py)
                            .unwrap()
                            .into_any()
                            .unbind(),
                    ),
                    ("qty", 7_i64.into_pyobject(py).unwrap().into_any().unbind()),
                ],
            );
            let err = element_to_write_row(&element, &columns()).unwrap_err();
            assert!(
                err.to_string().contains("column 'price'"),
                "unexpected error: {err}"
            );
        });
    }

    #[test]
    fn unsupported_declared_type_errors() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[("id", "x".into_pyobject(py).unwrap().into_any().unbind())],
            );
            let cols = vec![("id".to_string(), "uuid".to_string())];
            let err = element_to_write_row(&element, &cols).unwrap_err();
            assert!(
                err.to_string().contains("unsupported column type 'uuid'"),
                "unexpected error: {err}"
            );
        });
    }

    #[test]
    fn non_dict_value_errors() {
        Python::attach(|py| {
            let element = PyElement::new(PyList::empty(py).into_any().unbind());
            let err = element_to_write_row(&element, &columns()).unwrap_err();
            assert!(
                err.to_string().contains("must be a dict"),
                "unexpected error: {err}"
            );
        });
    }

    #[test]
    fn empty_element_errors() {
        let err = element_to_write_row(&PyElement::none(), &columns()).unwrap_err();
        assert!(err.to_string().contains("empty"), "unexpected error: {err}");
    }

    #[test]
    fn query_is_wrapped_and_filtered_on_the_quoted_time_column() {
        let mut q = window_query("SELECT time, sym FROM trades;", "time");
        let sql = q((NanoTime::ZERO, NanoTime::new(1_000_000_000)), 0, 0);
        // The trailing `;` is stripped so the subquery wrap stays valid.
        assert!(
            sql.contains("FROM (SELECT time, sym FROM trades) AS sub"),
            "{sql}"
        );
        assert!(sql.contains(r#"WHERE sub."time" >="#), "{sql}");
        assert!(sql.contains(r#"ORDER BY sub."time""#), "{sql}");
    }

    #[test]
    fn cursor_query_filters_strictly_past_the_cursor() {
        let mut q = cursor_query("SELECT time, sym FROM trades", "time");
        let sql = q(NanoTime::new(1_000_000_000));
        assert!(sql.contains(r#"WHERE sub."time" >"#), "{sql}");
        assert!(!sql.contains(">="), "cursor must be strict: {sql}");
    }
}
