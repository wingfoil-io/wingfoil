//! Python bindings for the wingfoil-next **KDB+** adapter
//! ([`wingfoil_next::adapters::kdb`]).
//!
//! Three graph entry points, all `#[pyadapter]`-generated:
//!
//! | Python                  | Rust             | shape |
//! |-------------------------|------------------|-------|
//! | `kdb_read(graph, …)`    | [`kdb_read`]     | historical, time-sliced replay |
//! | `kdb_sub(graph, …)`     | [`kdb_sub`]      | real-time tickerplant tail |
//! | `kdb_write(stream, …)`  | [`KdbSinkOps`]   | batched table insert |
//!
//! # The dynamic edge
//!
//! A Rust caller writes a record `struct` and implements [`KdbDeserialize`] /
//! [`KdbSerialize`] on it. Python has no such struct, so — exactly as postgres
//! does — this supplies two dynamic record types:
//!
//! - **reads** decode into [`PyKdbRow`], column name/value pairs dispatched on
//!   each value's *actual* KDB type, which erases to a Python **`dict`**;
//! - **writes** take the caller's declared `columns` (name + KDB type, in table
//!   order) and marshal each Python `dict` into a `PyKdbWriteRow`.
//!
//! Both intermediates are plain Rust data (no `Py<PyAny>` inside), so they can
//! cross to the adapter's worker thread.
//!
//! Every source is burst-shaped, so each tick yields a Python **`list` of row
//! dicts** — the same-instant group, losslessly.
//!
//! # Temporal values stay integers
//!
//! A KDB timestamp/timespan column decodes to its **raw `int`** (nanoseconds
//! since the KDB epoch, 2000-01-01), and date/time/minute/second columns to
//! their raw `int` too — the same as the legacy binding. Converting to Python
//! `datetime` would have to guess a timezone and would not round-trip, so the
//! raw value is handed over and interpreting it is the caller's call. The row's
//! *graph* timestamp is separate and is what schedules the tick.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **`kdb_read` yields a list per tick, not a single dict.** Legacy
//!    `collapse()`d the burst, keeping only the last row of any timestamp and
//!    silently dropping the rest. Bursts erase to lists uniformly here, so the
//!    read is lossless. Call `[0]` for the single-row case.
//! 2. **The run window is an argument.** [`kdb_read`] slices its queries at
//!    *wiring* time and a Python `Graph` does not know its run mode until
//!    `run()`, so `start_nanos` / `duration_nanos` are explicit and must match
//!    the eventual `graph.run(...)`.
//! 3. **An unsupported column type is an error**, not a `Debug`-rendered
//!    string. Legacy's fallback arm turned an unhandled type into
//!    `format!("{k:?}")`, which reads as a plausible value in the dict.
//! 4. **New capabilities**: `kdb_sub` (the tickerplant tail, which legacy never
//!    bound), credentials, and the `buffer_size` back-pressure bound.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use wingfoil_next::NanoTime;
use wingfoil_next::adapters::kdb::{
    K, KdbConnection, KdbCredentials, KdbDeserialize, KdbError, KdbSerialize, KdbSinkOps, Row,
    SymbolInterner, kdb_read as kdb_replay, kdb_sub as kdb_tail, qtype,
};
use wingfoil_next::prelude::{Burst, GraphBuilder, Stream, StreamOps};

use crate::adapters::common::{RecordDict, historical_params, record_dict, run_mode};
use crate::{PyElement, pyadapter};

/// The error-message prefix for the sink's marshaling failures.
const WHO: &str = "kdb_write";

// ---------------------------------------------------------------------------
// Reading: a row -> a Python dict.
// ---------------------------------------------------------------------------

/// One decoded KDB value on its way to Python.
///
/// Deliberately *not* a `Py<PyAny>`: rows are decoded on the adapter's worker
/// and handed to the graph across a channel, so the intermediate must be plain
/// `Send` Rust data.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum PyKdbValue {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Symbol(String),
}

impl PyKdbValue {
    fn to_py(&self, py: Python<'_>) -> Py<PyAny> {
        /// Box a scalar. Primitive `IntoPyObject` conversions are infallible.
        macro_rules! boxed {
            ($v:expr) => {
                $v.into_pyobject(py)
                    .map(|b| b.to_owned().into_any().unbind())
                    .expect("invariant: scalar -> PyObject conversion is infallible")
            };
        }
        match self {
            PyKdbValue::Null => py.None(),
            PyKdbValue::Bool(v) => boxed!(*v),
            PyKdbValue::Int(v) => boxed!(*v),
            PyKdbValue::Float(v) => boxed!(*v),
            PyKdbValue::Symbol(v) => boxed!(v.as_str()),
        }
    }
}

/// A deserialized KDB row as column name/value pairs — the dynamic stand-in for
/// the record `struct` a Rust caller would write. Erases to a Python `dict`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PyKdbRow {
    columns: Vec<(String, PyKdbValue)>,
}

impl PyKdbRow {
    /// The decoded columns, in the order the query selected them.
    pub fn columns(&self) -> &[(String, PyKdbValue)] {
        &self.columns
    }
}

/// Decode one column, dispatching on the value's **actual** KDB type.
///
/// An unsupported type is an **error** — not a `Debug` rendering — so a schema
/// surprise (a nested/vector column, say) aborts on the first row instead of
/// producing dict entries that look like values but are debug strings.
fn column_value(k: &K, name: &str) -> std::result::Result<PyKdbValue, KdbError> {
    let value = match k.get_type() {
        qtype::LONG_ATOM => PyKdbValue::Int(k.get_long()?),
        qtype::INT_ATOM => PyKdbValue::Int(i64::from(k.get_int()?)),
        qtype::SHORT_ATOM => PyKdbValue::Int(i64::from(k.get_short()?)),
        qtype::FLOAT_ATOM => PyKdbValue::Float(k.get_float()?),
        qtype::REAL_ATOM => PyKdbValue::Float(f64::from(k.get_real()?)),
        qtype::BOOL_ATOM => PyKdbValue::Bool(k.get_bool()?),
        qtype::SYMBOL_ATOM => PyKdbValue::Symbol(k.get_symbol()?.to_string()),
        // Temporal columns keep their raw KDB representation (see the module
        // docs): i64 nanoseconds for timestamp/timespan, i32 for the rest.
        qtype::TIMESTAMP_ATOM | qtype::TIMESPAN_ATOM => PyKdbValue::Int(k.get_long()?),
        qtype::DATE_ATOM
        | qtype::MONTH_ATOM
        | qtype::TIME_ATOM
        | qtype::MINUTE_ATOM
        | qtype::SECOND_ATOM => PyKdbValue::Int(i64::from(k.get_int()?)),
        qtype::DATETIME_ATOM => PyKdbValue::Float(k.get_float()?),
        other => {
            // `KdbError`'s constructors are crate-private, but the `Object`
            // variant is public and carries a `K` — and `K::new_error` is
            // exactly kdb's own "this is an error, here is why" value, so the
            // message survives `Display` intact.
            return Err(KdbError::Object(K::new_error(format!(
                "kdb_read: unsupported column type {other} for column `{name}`; supported: \
                 bool, short/int/long, real/float, symbol, and the temporal atoms"
            ))));
        }
    };
    Ok(value)
}

impl KdbDeserialize for PyKdbRow {
    fn from_kdb_row(
        row: Row<'_>,
        columns: &[String],
        _interner: &mut SymbolInterner,
    ) -> std::result::Result<(NanoTime, Self), KdbError> {
        // Convention (as classic): the query wrapper `xcols`-es the time column
        // first, so column 0 is always the row's timestamp.
        let time = row.get_timestamp(0)?;
        let mut decoded = Vec::with_capacity(columns.len().saturating_sub(1));
        for (index, name) in columns.iter().enumerate().skip(1) {
            decoded.push((name.clone(), column_value(&row.get(index)?, name)?));
        }
        Ok((time, PyKdbRow { columns: decoded }))
    }
}

impl From<PyKdbRow> for PyElement {
    fn from(row: PyKdbRow) -> Self {
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
// Writing: a Python dict -> a K row.
// ---------------------------------------------------------------------------

/// One column's value, already typed per the caller's declaration.
#[derive(Debug, Clone, PartialEq)]
enum KdbParam {
    Symbol(String),
    Float(f64),
    Long(i64),
    Int(i32),
    Bool(bool),
}

impl KdbParam {
    fn to_k(&self) -> K {
        match self {
            KdbParam::Symbol(v) => K::new_symbol(v.clone()),
            KdbParam::Float(v) => K::new_float(*v),
            KdbParam::Long(v) => K::new_long(*v),
            KdbParam::Int(v) => K::new_int(*v),
            KdbParam::Bool(v) => K::new_bool(*v),
        }
    }
}

/// One row to insert: the declared columns' values in table order (after time).
#[derive(Debug, Clone, Default, PartialEq)]
struct PyKdbWriteRow {
    values: Vec<KdbParam>,
}

impl KdbSerialize for PyKdbWriteRow {
    fn to_kdb_row(&self) -> K {
        K::new_compound_list(self.values.iter().map(KdbParam::to_k).collect())
    }
}

/// Marshal one erased stream value — a Python `dict` — into a write row.
///
/// Fails loudly: a missing key, an unsupported declared type, or a wrong-typed
/// value aborts the run rather than inserting a silently-defaulted cell.
fn element_to_row(elem: &PyElement, columns: &[(String, String)]) -> Result<PyKdbWriteRow> {
    Python::attach(|py| {
        let dict = record_dict(elem, py, WHO, "of the declared columns")?;
        let record = RecordDict::new(&dict, WHO);
        let values = columns
            .iter()
            .map(|(name, ty)| {
                let value = record
                    .get(name)?
                    .ok_or_else(|| anyhow!("{WHO}: missing column '{name}' in dict"))?;
                let typed = |what: &'static str| {
                    move |e: PyErr| anyhow!("{WHO}: column '{name}' ({ty}) must be {what}: {e}")
                };
                Ok(match ty.as_str() {
                    "symbol" | "sym" | "str" => {
                        KdbParam::Symbol(value.extract().map_err(typed("a str"))?)
                    }
                    "float" | "real" => KdbParam::Float(value.extract().map_err(typed("a float"))?),
                    "long" => KdbParam::Long(value.extract().map_err(typed("an int"))?),
                    "int" => KdbParam::Int(value.extract().map_err(typed("an int"))?),
                    "bool" => KdbParam::Bool(value.extract().map_err(typed("a bool"))?),
                    other => bail!(
                        "{WHO}: unsupported column type '{other}' for column '{name}'; \
                         supported: symbol/sym/str, float/real, long, int, bool"
                    ),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(PyKdbWriteRow { values })
    })
}

// ---------------------------------------------------------------------------
// Query construction.
// ---------------------------------------------------------------------------

/// The historical slice query: the caller's query wrapped, filtered on the time
/// column, and `xcols`-ed so the time column is column 0 whatever the original
/// select ordered.
fn window_query(
    query: &str,
    time_col: &str,
) -> impl FnMut((NanoTime, NanoTime), i32, usize) -> String + Send + 'static {
    let query = query.trim().trim_end_matches(';').trim_end().to_string();
    let tc = time_col.to_string();
    move |(t0, t1), _date, _iter| {
        format!(
            "`{tc} xcols select from ({query}) \
             where {tc} >= (`timestamp$){t0}j, {tc} < (`timestamp$){t1}j",
            t0 = t0.to_kdb_timestamp(),
            t1 = t1.to_kdb_timestamp(),
        )
    }
}

/// Build the connection, attaching credentials only when both are supplied.
fn connection(
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
) -> Result<KdbConnection> {
    let mut conn = KdbConnection::new(host, port);
    match (username, password) {
        (Some(username), Some(password)) => {
            conn.credentials = Some(KdbCredentials { username, password });
        }
        (None, None) => {}
        _ => bail!("kdb: username and password must be given together, or neither"),
    }
    Ok(conn)
}

// ---------------------------------------------------------------------------
// Entry points.
// ---------------------------------------------------------------------------

/// Read a KDB+ table as a bounded historical replay.
///
/// `query` is wrapped and filtered per time slice on `time_col`, then
/// `xcols`-ed so the time column comes first — so unlike the postgres binding,
/// the query need *not* select it first itself. Each tick yields a `list` of
/// `{column: value}` dicts — the rows sharing that timestamp.
///
/// `start_nanos` / `duration_nanos` describe the run this source is wired for
/// and must match the eventual `graph.run(...)`; the window is validated and
/// sliced at wiring, so a bad one raises here. `chunk_secs` is the slice width.
/// `buffer_size` bounds the replay's look-ahead (`None` = unbounded).
///
/// Temporal columns arrive as raw integers (see the module docs).
#[pyadapter(name = kdb_read, source)]
#[pyo3(signature = (
    host, port, query, time_col, start_nanos, duration_nanos,
    chunk_secs = 3600, buffer_size = None, username = None, password = None,
))]
#[allow(clippy::too_many_arguments)]
fn read(
    g: &GraphBuilder,
    host: String,
    port: u16,
    query: String,
    time_col: String,
    start_nanos: u64,
    duration_nanos: u64,
    chunk_secs: u64,
    buffer_size: Option<usize>,
    username: Option<String>,
    password: Option<String>,
) -> Result<Stream<Burst<PyKdbRow>>> {
    kdb_replay::<PyKdbRow>(
        g,
        historical_params(start_nanos, duration_nanos),
        connection(host, port, username, password)?,
        Duration::from_secs(chunk_secs),
        window_query(&query, &time_col),
        buffer_size,
    )
}

/// Subscribe to a KDB+ tickerplant's live tail.
///
/// Each tick yields a `list` of `{column: value}` dicts — the rows the
/// tickerplant published between graph cycles. `symbols` is the tickerplant's
/// symbol filter (`"`"` for all).
///
/// `realtime` declares the run mode this source is wired for and must match the
/// eventual `graph.run(realtime=…)`. The tail is live and unbounded with no
/// historical timeline, so `realtime=False` raises here — use `kdb_read`.
///
/// This has no legacy `wingfoil-python` equivalent.
#[pyadapter(name = kdb_sub, source)]
#[pyo3(signature = (
    host, port, table, symbols = "`".to_string(), realtime = true,
    username = None, password = None,
))]
fn sub(
    g: &GraphBuilder,
    host: String,
    port: u16,
    table: String,
    symbols: String,
    realtime: bool,
    username: Option<String>,
    password: Option<String>,
) -> Result<Stream<Burst<PyKdbRow>>> {
    kdb_tail::<PyKdbRow>(
        g,
        run_mode(realtime),
        connection(host, port, username, password)?,
        table,
        symbols,
    )
}

/// Write this stream to a KDB+ table.
///
/// Each tick's value is either a single `dict` of the declared columns, or a
/// `list`/`tuple` of such dicts for several rows at one timestamp. The graph
/// timestamp is prepended, so the target table's columns must line up as
/// `(time, <columns in the declared order>)`.
///
/// `columns` is a list of `(name, kdb_type)` pairs in table order; the accepted
/// types are `symbol`/`sym`/`str`, `float`/`real`, `long`, `int` and `bool`. A
/// missing key, an unsupported type, or a wrong-typed value aborts the run.
/// Only scalar atom columns are writable, which the Rust sink enforces.
///
/// The connection is opened lazily on the first write, so a connection failure
/// aborts the run rather than raising at wiring. `buffer_size` bounds the
/// unwritten backlog. Returns a terminal stream whose value is `None`.
#[pyadapter(name = kdb_write)]
#[pyo3(signature = (
    host, port, table, columns, buffer_size = None, username = None, password = None,
))]
fn write(
    stream: &Stream<Burst<PyElement>>,
    host: String,
    port: u16,
    table: String,
    columns: Vec<(String, String)>,
    buffer_size: Option<usize>,
    username: Option<String>,
    password: Option<String>,
) -> Result<Stream<()>> {
    let conn = connection(host, port, username, password)?;
    let rows: Stream<Burst<PyKdbWriteRow>> = stream.try_map(move |burst: &Burst<PyElement>| {
        burst
            .iter()
            .map(|elem| element_to_row(elem, &columns))
            .collect::<Result<Burst<PyKdbWriteRow>>>()
    });
    rows.kdb_write(conn, &table, buffer_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns() -> Vec<(String, String)> {
        [("sym", "symbol"), ("price", "float"), ("qty", "long")]
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

    fn obj<'py, T: pyo3::IntoPyObjectExt<'py>>(py: Python<'py>, v: T) -> Py<PyAny> {
        v.into_py_any(py).unwrap()
    }

    #[test]
    fn a_row_erases_to_a_dict_in_column_order() {
        let row = PyKdbRow {
            columns: vec![
                ("sym".into(), PyKdbValue::Symbol("AAPL".into())),
                ("price".into(), PyKdbValue::Float(1.5)),
                ("qty".into(), PyKdbValue::Int(7)),
                ("live".into(), PyKdbValue::Bool(true)),
                ("missing".into(), PyKdbValue::Null),
            ],
        };
        let element = PyElement::from(row);
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let names: Vec<String> = dict
                .keys()
                .iter()
                .map(|k| k.extract::<String>().unwrap())
                .collect();
            assert_eq!(vec!["sym", "price", "qty", "live", "missing"], names);
            let get = |k: &str| dict.get_item(k).unwrap().unwrap();
            assert_eq!("AAPL", get("sym").extract::<String>().unwrap());
            assert_eq!(1.5, get("price").extract::<f64>().unwrap());
            assert_eq!(7, get("qty").extract::<i64>().unwrap());
            assert!(get("live").extract::<bool>().unwrap());
            assert!(get("missing").is_none());
        });
    }

    #[test]
    fn a_supported_column_decodes_on_its_actual_type() {
        // The dispatch is on the *value's* type tag, not a declared schema.
        assert_eq!(
            PyKdbValue::Int(7),
            column_value(&K::new_long(7), "qty").unwrap()
        );
        assert_eq!(
            PyKdbValue::Float(1.5),
            column_value(&K::new_float(1.5), "px").unwrap()
        );
        assert_eq!(
            PyKdbValue::Symbol("AAPL".into()),
            column_value(&K::new_symbol("AAPL".into()), "sym").unwrap()
        );
    }

    #[test]
    fn an_unsupported_column_type_errors_rather_than_debug_rendering() {
        // Legacy's fallback arm turned an unhandled type into
        // `format!("{k:?}")` — a debug string sitting in the dict where a
        // value should be. A `char` column reaches the dispatch (unlike, say,
        // a `byte` column, which the row accessor rejects first) and so is the
        // case that distinguishes the two.
        let err = column_value(&K::new_char('x'), "flag").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("unsupported column type"), "{message}");
        assert!(message.contains("flag"), "{message}");
    }

    #[test]
    fn a_dict_marshals_to_the_declared_column_order() {
        Python::attach(|py| {
            // Deliberately out of table order: `columns` decides.
            let element = dict_element(
                py,
                &[
                    ("qty", obj(py, 7_i64)),
                    ("sym", obj(py, "AAPL")),
                    ("price", obj(py, 1.5_f64)),
                ],
            );
            let row = element_to_row(&element, &columns()).unwrap();
            assert_eq!(
                vec![
                    KdbParam::Symbol("AAPL".into()),
                    KdbParam::Float(1.5),
                    KdbParam::Long(7),
                ],
                row.values
            );
        });
    }

    #[test]
    fn a_missing_column_errors() {
        Python::attach(|py| {
            let element = dict_element(py, &[("sym", obj(py, "AAPL"))]);
            let err = element_to_row(&element, &columns()).unwrap_err();
            assert!(err.to_string().contains("missing column 'price'"), "{err}");
        });
    }

    #[test]
    fn a_wrong_typed_value_errors() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[
                    ("sym", obj(py, "AAPL")),
                    ("price", obj(py, "not-a-number")),
                    ("qty", obj(py, 7_i64)),
                ],
            );
            let err = element_to_row(&element, &columns()).unwrap_err();
            assert!(err.to_string().contains("column 'price'"), "{err}");
        });
    }

    #[test]
    fn an_unsupported_declared_type_errors() {
        Python::attach(|py| {
            let element = dict_element(py, &[("id", obj(py, "x"))]);
            let cols = vec![("id".to_string(), "guid".to_string())];
            let err = element_to_row(&element, &cols).unwrap_err();
            assert!(
                err.to_string().contains("unsupported column type 'guid'"),
                "{err}"
            );
        });
    }

    #[test]
    fn a_non_dict_value_errors() {
        Python::attach(|py| {
            let element = PyElement::new(pyo3::types::PyList::empty(py).into_any().unbind());
            let err = element_to_row(&element, &columns()).unwrap_err();
            assert!(err.to_string().contains("must be a dict"), "{err}");
        });
    }

    #[test]
    fn the_query_is_wrapped_xcols_and_time_filtered() {
        let mut q = window_query("select from trade;", "time");
        let sql = q((NanoTime::ZERO, NanoTime::new(1_000_000_000)), 0, 0);
        // The trailing `;` is stripped so the wrap stays valid, and `xcols`
        // guarantees the time column is column 0 for the decoder.
        assert!(
            sql.starts_with("`time xcols select from (select from trade)"),
            "{sql}"
        );
        assert!(sql.contains("where time >="), "{sql}");
        assert!(sql.contains("time <"), "{sql}");
    }

    #[test]
    fn credentials_must_be_given_together() {
        assert!(connection("h".into(), 1, None, None).is_ok());
        assert!(
            connection("h".into(), 1, Some("u".into()), Some("p".into()))
                .unwrap()
                .credentials
                .is_some()
        );
        assert!(connection("h".into(), 1, Some("u".into()), None).is_err());
        assert!(connection("h".into(), 1, None, Some("p".into())).is_err());
    }
}
