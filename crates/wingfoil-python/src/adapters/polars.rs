//! Python bindings for the wingfoil **polars** adapter
//! ([`wingfoil::adapters::polars`]).
//!
//! Two graph entry points, both `#[pyadapter]`-generated:
//!
//! | Python                     | Rust                                | shape |
//! |----------------------------|-------------------------------------|-------|
//! | `polars_read(graph, …)`    | [`polars_read`]                     | deterministic historical replay of a Parquet / IPC file |
//! | `polars_write(stream, …)`  | [`PolarsSinkOps::polars_write_with_options`] | Parquet / IPC file sink |
//!
//! # The dynamic edge
//!
//! A replayed [`PolarsRow`] erases to a **`dict`** in the frame's column order;
//! a burst to a `list` of them. Values map by dtype: null → `None`, boolean →
//! `bool`, every integer width → `int`, float → `float`, string / categorical →
//! `str`, binary → `bytes`, and `Datetime` → `int` **nanoseconds since the
//! epoch** (not a `datetime`, which would truncate to microseconds). Any other
//! dtype aborts the run naming the column, the dtype and the supported set.
//! Rows are converted in a `try_map` on the graph thread into [`PyPolarsRow`],
//! plain Rust data, and become Python objects only at the erasure seam — one
//! GIL attach per burst.
//!
//! On the way in, `polars_write` takes a `dict` (or a `list` of dicts for one
//! instant) whose **keys are the columns, in order**; `None` / `bool` / `int` /
//! `float` / `str` / `bytes` map to Null / Boolean / Int64 / Float64 / String /
//! Binary. The engine sink fixes each column's dtype from its first non-null
//! value, so a column must keep one Python type (write `1.0`, not `1`, into a
//! float column) — a mismatch aborts the run naming the column.
//!
//! # Deviations
//!
//! There is no legacy `py_polars.rs` — the Rust adapter is wingfoil-only, so the
//! binding has no parity oracle. What it leaves out of the Rust surface, and
//! why:
//!
//! 1. **No in-memory `DataFrame` in or out.** Handing a Python `polars.DataFrame`
//!    across needs `pyo3-polars`, which pins its own pyo3 and polars versions
//!    against ours. Files are the interchange instead: `df.write_parquet(p)`
//!    then `polars_read(graph, p, …)`, and `polars_write(stream, p)` then
//!    `pl.read_parquet(p)`. So `polars_collect` is not bound.
//! 2. **The format is a string** (`format="parquet"` / `"ipc"`), not an enum
//!    class, per the `/bind-adapter` selector convention; omitted, it comes
//!    from the extension.

use std::cell::RefCell;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyString};
use wingfoil::adapters::polars::{
    AnyValue, PolarsFormat, PolarsRow, PolarsSinkOps, PolarsSinkOptions, PolarsSource, Schema,
    SchemaRef, TimeUnit, polars_read as rust_polars_read,
};
use wingfoil::prelude::{Burst, GraphBuilder, Stream, StreamOps};

use crate::{PyElement, pyadapter};

/// The error-message prefix for the sink's marshaling failures.
const WHO: &str = "polars_write";

/// One cell of a replayed row, as plain Rust data.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum PyPolarsValue {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
}

impl PyPolarsValue {
    /// Decode one cell, failing loudly on a dtype with no Python mapping here.
    fn decode(column: &str, value: &AnyValue<'static>) -> Result<Self> {
        if let Some(s) = value.get_str() {
            return Ok(Self::Str(s.to_string()));
        }
        Ok(match value {
            AnyValue::Null => Self::Null,
            AnyValue::Boolean(v) => Self::Bool(*v),
            AnyValue::Int8(v) => Self::Int(i64::from(*v)),
            AnyValue::Int16(v) => Self::Int(i64::from(*v)),
            AnyValue::Int32(v) => Self::Int(i64::from(*v)),
            AnyValue::Int64(v) => Self::Int(*v),
            AnyValue::UInt8(v) => Self::UInt(u64::from(*v)),
            AnyValue::UInt16(v) => Self::UInt(u64::from(*v)),
            AnyValue::UInt32(v) => Self::UInt(u64::from(*v)),
            AnyValue::UInt64(v) => Self::UInt(*v),
            AnyValue::Float32(v) => Self::Float(f64::from(*v)),
            AnyValue::Float64(v) => Self::Float(*v),
            AnyValue::Binary(v) => Self::Bytes(v.to_vec()),
            AnyValue::BinaryOwned(v) => Self::Bytes(v.clone()),
            AnyValue::Datetime(v, unit, _) | AnyValue::DatetimeOwned(v, unit, _) => {
                let scale = match unit {
                    TimeUnit::Nanoseconds => 1,
                    TimeUnit::Microseconds => 1_000,
                    TimeUnit::Milliseconds => 1_000_000,
                };
                Self::Int(v.checked_mul(scale).ok_or_else(|| {
                    anyhow!(
                        "polars_read: column '{column}': datetime {v} overflows i64 nanoseconds"
                    )
                })?)
            }
            other => bail!(
                "polars_read: column '{column}' has dtype {}, which has no Python mapping; \
                 supported are null, bool, integers, floats, str / categorical, binary and \
                 datetime",
                other.dtype()
            ),
        })
    }

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
            Self::Null => py.None(),
            Self::Bool(v) => boxed!(*v),
            Self::Int(v) => boxed!(*v),
            Self::UInt(v) => boxed!(*v),
            Self::Float(v) => boxed!(*v),
            Self::Str(v) => boxed!(v.as_str()),
            Self::Bytes(v) => PyBytes::new(py, v).into_any().unbind(),
        }
    }
}

/// A replayed row: the column names (shared by every row of one read) and one
/// decoded value per column. Plain Rust data — no `Py<PyAny>`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PyPolarsRow {
    names: Arc<[String]>,
    values: Vec<PyPolarsValue>,
}

impl PyPolarsRow {
    fn decode(row: &PolarsRow, names: &Arc<[String]>) -> Result<Self> {
        let values = names
            .iter()
            .zip(row.values())
            .map(|(name, value)| PyPolarsValue::decode(name, value))
            .collect::<Result<_>>()?;
        Ok(Self {
            names: names.clone(),
            values,
        })
    }
}

impl From<PyPolarsRow> for PyElement {
    fn from(row: PyPolarsRow) -> Self {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            for (name, value) in row.names.iter().zip(&row.values) {
                dict.set_item(name, value.to_py(py))
                    .expect("invariant: inserting a str key into a fresh PyDict cannot fail");
            }
            PyElement::new(dict.into_any().unbind())
        })
    }
}

/// `"parquet"` / `"ipc"` → the format; anything else is an error listing both.
fn format_kind(format: &str) -> Result<PolarsFormat> {
    match format {
        "parquet" => Ok(PolarsFormat::Parquet),
        "ipc" => Ok(PolarsFormat::Ipc),
        other => bail!("unknown format {other:?}; expected \"parquet\" or \"ipc\""),
    }
}

/// The source for `path`, with `format` overriding its extension.
fn source(path: String, format: Option<&str>) -> Result<PolarsSource> {
    Ok(match format.map(format_kind).transpose()? {
        None => PolarsSource::from(path),
        Some(PolarsFormat::Parquet) => PolarsSource::Parquet(path.into()),
        Some(PolarsFormat::Ipc) => PolarsSource::Ipc(path.into()),
    })
}

// ---------------------------------------------------------------------------
// Entry points.
// ---------------------------------------------------------------------------

/// Replay a Parquet or Arrow IPC file as a deterministic historical source.
///
/// Each tick yields a `list` of `{column: value}` dicts — the rows sharing that
/// instant, in the file's column order. Call `[0]` for the single-row case.
///
/// `time_column` names the column that drives the graph clock: a `Datetime`
/// (any unit) or an `Int64` / `UInt64` of nanoseconds since the epoch. It is **not**
/// repeated in the dicts — it is the tick time; chain `.with_time()` to read
/// it. The file's footer is read at wiring, so a missing column or another
/// dtype raises here. The rows are streamed one Parquet row group / IPC record
/// batch at a time during the run, and their times are checked as they are
/// read: a null, a negative or a decreasing time fails the run naming the row.
/// Run the graph historically from at or before the first row's time.
///
/// Values: null → `None`, bool, integers → `int`, floats → `float`, str /
/// categorical → `str`, binary → `bytes`, datetime → `int` nanoseconds. A
/// column of any other dtype aborts the run naming it.
///
/// `format` is `"parquet"` or `"ipc"`; omitted, the extension decides
/// (`.parquet`/`.pq`, `.arrow`/`.ipc`/`.feather`). `buffer_size` bounds the
/// replay's look-ahead (`None` = unbounded). The file is opened at wiring, so
/// a missing or unreadable one raises here.
#[pyadapter(name = polars_read, source)]
#[pyo3(signature = (path, time_column, format = None, buffer_size = None))]
fn read(
    g: &GraphBuilder,
    path: String,
    time_column: String,
    format: Option<String>,
    buffer_size: Option<usize>,
) -> Result<Stream<Burst<PyPolarsRow>>> {
    let rows = rust_polars_read(
        g,
        source(path, format.as_deref())?,
        &time_column,
        buffer_size,
    )?;
    // Column names are fixed by the read; resolve them from the first row and
    // share them across every dict built after.
    let names: std::cell::RefCell<Option<Arc<[String]>>> = std::cell::RefCell::new(None);
    Ok(rows.try_map(move |burst: &Burst<PolarsRow>| {
        burst
            .iter()
            .map(|row| {
                let names = names
                    .borrow_mut()
                    .get_or_insert_with(|| {
                        row.schema().iter_names().map(|n| n.to_string()).collect()
                    })
                    .clone();
                PyPolarsRow::decode(row, &names)
            })
            .collect::<Result<Burst<PyPolarsRow>>>()
    }))
}

/// Write this stream to a Parquet or Arrow IPC file when the run ends.
///
/// Each tick's value is a `dict` — or a `list`/`tuple` of dicts for several
/// rows at one instant — whose keys are the columns, in order; every row must
/// have the same keys in the same order. `None` / `bool` / `int` / `float` /
/// `str` / `bytes` become Null / Boolean / Int64 / Float64 / String / Binary
/// columns, and a column must keep one Python type throughout (a mix aborts
/// the run naming it).
///
/// The graph time of each row is written as a leading `Datetime[ns]` column
/// named `time_column` (default `"time"`); pass `None` to omit it. `format` is
/// `"parquet"` or `"ipc"`; omitted, the extension decides. The directory is
/// probed at wiring (so an unwritable path raises here) without touching
/// `path`, and the file is written once, at the end of the run — after an
/// abort too, with the rows seen so far — through a temp file renamed over
/// `path`, so `path` is never left empty or half-written.
///
/// Returns a terminal stream whose value is `None`.
#[pyadapter(name = polars_write)]
#[pyo3(signature = (path, time_column = Some("time".to_string()), format = None))]
fn write(
    stream: &Stream<Burst<PyElement>>,
    path: String,
    time_column: Option<String>,
    format: Option<String>,
) -> Result<Stream<()>> {
    let options = PolarsSinkOptions {
        time_column,
        format: format.as_deref().map(format_kind).transpose()?,
    };
    // The last row's schema, reused while the columns and dtypes match, so
    // consecutive rows share one `Arc` and the sink's per-row schema check is
    // a pointer comparison rather than a name-and-dtype walk.
    let last_schema: RefCell<Option<SchemaRef>> = RefCell::new(None);
    let rows: Stream<Burst<PolarsRow>> = stream.try_map(move |burst: &Burst<PyElement>| {
        let mut last = last_schema.borrow_mut();
        Python::attach(|py| {
            burst
                .iter()
                .map(|elem| element_to_row(elem, py, &mut last))
                .collect::<Result<Burst<PolarsRow>>>()
        })
    });
    rows.polars_write_with_options(&path, options)
}

/// Marshal one erased stream value — a Python `dict` — into a row. `last` is
/// the previous row's schema: reused (same `Arc`) when this dict has the same
/// column names and dtypes in the same order, replaced otherwise.
fn element_to_row(
    elem: &PyElement,
    py: Python<'_>,
    last: &mut Option<SchemaRef>,
) -> Result<PolarsRow> {
    let dict = crate::adapters::common::record_dict(elem, py, WHO, "of column values")?;
    let mut fields = Vec::with_capacity(dict.len());
    let mut values = Vec::with_capacity(dict.len());
    for (key, value) in dict.iter() {
        let name: String = key
            .extract()
            .map_err(|_| anyhow!("{WHO}: column names must be str, got {}", key.get_type()))?;
        let value = py_to_any_value(&name, &value)?;
        fields.push((name.into(), value.dtype()));
        values.push(value);
    }
    let reusable = last.as_ref().is_some_and(|schema| {
        schema.len() == fields.len()
            && schema
                .iter()
                .zip(&fields)
                .all(|((n, d), (name, dtype))| n == name && d == dtype)
    });
    let schema = match last {
        Some(schema) if reusable => schema.clone(),
        _ => last.insert(Arc::new(Schema::from_iter(fields))).clone(),
    };
    PolarsRow::new(schema, values)
}

/// One Python value → a cell. `bool` is checked before `int`, since Python's
/// `bool` is an `int` subclass.
fn py_to_any_value(column: &str, value: &Bound<'_, PyAny>) -> Result<AnyValue<'static>> {
    if value.is_none() {
        Ok(AnyValue::Null)
    } else if value.is_instance_of::<PyBool>() {
        Ok(AnyValue::Boolean(value.extract()?))
    } else if value.is_instance_of::<PyInt>() {
        let v: i64 = value
            .extract()
            .map_err(|_| anyhow!("{WHO}: column '{column}': int {value} does not fit in i64"))?;
        Ok(AnyValue::Int64(v))
    } else if value.is_instance_of::<PyFloat>() {
        Ok(AnyValue::Float64(value.extract()?))
    } else if value.is_instance_of::<PyString>() {
        let v: String = value.extract()?;
        Ok(AnyValue::StringOwned(v.into()))
    } else if value.is_instance_of::<PyBytes>() {
        Ok(AnyValue::BinaryOwned(value.extract()?))
    } else {
        bail!(
            "{WHO}: column '{column}' has a {} value; supported are None, bool, int, float, \
             str and bytes",
            value.get_type()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyList;
    use wingfoil::adapters::polars::DataType;

    /// The dtype a written column takes for a Python value.
    fn dtype_of(column: &str, value: &Bound<'_, PyAny>) -> Result<DataType> {
        py_to_any_value(column, value).map(|v| v.dtype())
    }

    fn row(names: &[&str], values: Vec<AnyValue<'static>>) -> PolarsRow {
        let schema = Schema::from_iter(
            names
                .iter()
                .zip(&values)
                .map(|(n, v)| ((*n).into(), v.dtype())),
        );
        PolarsRow::new(Arc::new(schema), values).unwrap()
    }

    fn names(ns: &[&str]) -> Arc<[String]> {
        ns.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_row_decodes_every_supported_dtype() {
        let r = row(
            &["n", "b", "i", "u", "f", "s", "y", "t"],
            vec![
                AnyValue::Null,
                AnyValue::Boolean(true),
                AnyValue::Int32(-3),
                AnyValue::UInt64(u64::MAX),
                AnyValue::Float32(1.5),
                AnyValue::StringOwned("AAPL".into()),
                AnyValue::BinaryOwned(vec![1, 2]),
                AnyValue::DatetimeOwned(7, TimeUnit::Microseconds, None),
            ],
        );
        let decoded =
            PyPolarsRow::decode(&r, &names(&["n", "b", "i", "u", "f", "s", "y", "t"])).unwrap();
        assert_eq!(
            vec![
                PyPolarsValue::Null,
                PyPolarsValue::Bool(true),
                PyPolarsValue::Int(-3),
                PyPolarsValue::UInt(u64::MAX),
                PyPolarsValue::Float(1.5),
                PyPolarsValue::Str("AAPL".into()),
                PyPolarsValue::Bytes(vec![1, 2]),
                PyPolarsValue::Int(7_000),
            ],
            decoded.values
        );
    }

    #[test]
    fn an_unsupported_dtype_errors_naming_the_column() {
        let list = AnyValue::List(
            wingfoil::adapters::polars::polars::prelude::Series::new_empty(
                "x".into(),
                &DataType::Int64,
            ),
        );
        let r = row(&["xs"], vec![list]);
        let err = PyPolarsRow::decode(&r, &names(&["xs"])).unwrap_err();
        let err = err.to_string();
        assert!(err.contains("column 'xs'"), "got: {err}");
        assert!(err.contains("supported are"), "got: {err}");
    }

    #[test]
    fn a_row_erases_to_a_dict_in_column_order() {
        let decoded = PyPolarsRow {
            names: names(&["zeta", "alpha"]),
            values: vec![PyPolarsValue::Int(1), PyPolarsValue::Str("a".into())],
        };
        let element = PyElement::from(decoded);
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let keys: Vec<String> = dict
                .keys()
                .iter()
                .map(|k| k.extract::<String>().unwrap())
                .collect();
            assert_eq!(vec!["zeta", "alpha"], keys);
        });
    }

    #[test]
    fn a_dict_marshals_to_a_row_with_inferred_dtypes() {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("sym", "AAPL").unwrap();
            dict.set_item("px", 1.5).unwrap();
            dict.set_item("qty", 10).unwrap();
            dict.set_item("live", true).unwrap();
            dict.set_item("note", py.None()).unwrap();
            dict.set_item("raw", PyBytes::new(py, b"x")).unwrap();
            let r =
                element_to_row(&PyElement::new(dict.into_any().unbind()), py, &mut None).unwrap();
            let dtypes: Vec<(String, DataType)> = r
                .schema()
                .iter()
                .map(|(n, d)| (n.to_string(), d.clone()))
                .collect();
            assert_eq!(
                vec![
                    ("sym".to_string(), DataType::String),
                    ("px".to_string(), DataType::Float64),
                    ("qty".to_string(), DataType::Int64),
                    ("live".to_string(), DataType::Boolean),
                    ("note".to_string(), DataType::Null),
                    ("raw".to_string(), DataType::Binary),
                ],
                dtypes
            );
        });
    }

    #[test]
    fn consecutive_rows_with_the_same_columns_share_one_schema() {
        Python::attach(|py| {
            let dict = |px: Bound<'_, PyAny>| {
                let d = PyDict::new(py);
                d.set_item("sym", "A").unwrap();
                d.set_item("px", px).unwrap();
                PyElement::new(d.into_any().unbind())
            };
            let float = |v: f64| v.into_pyobject(py).unwrap().into_any();
            let mut last = None;
            let a = element_to_row(&dict(float(1.0)), py, &mut last).unwrap();
            let b = element_to_row(&dict(float(2.0)), py, &mut last).unwrap();
            assert!(Arc::ptr_eq(a.schema(), b.schema()), "same columns reuse");

            // A dtype change (None -> Null) builds a fresh schema and caches it.
            let c = element_to_row(&dict(py.None().into_bound(py)), py, &mut last).unwrap();
            assert!(!Arc::ptr_eq(b.schema(), c.schema()));
            assert!(Arc::ptr_eq(c.schema(), last.as_ref().unwrap()));
        });
    }

    #[test]
    fn bool_is_not_marshaled_as_int() {
        Python::attach(|py| {
            let t = true.into_pyobject(py).unwrap().to_owned().into_any();
            assert_eq!(DataType::Boolean, dtype_of("b", &t).unwrap());
        });
    }

    #[test]
    fn an_unsupported_value_or_an_oversized_int_errors() {
        Python::attach(|py| {
            let list = PyList::empty(py).into_any();
            let err = dtype_of("xs", &list).unwrap_err().to_string();
            assert!(
                err.contains("column 'xs'") && err.contains("list"),
                "got: {err}"
            );

            let big = py.eval(c"2**70", None, None).unwrap();
            let err = dtype_of("n", &big).unwrap_err().to_string();
            assert!(err.contains("does not fit in i64"), "got: {err}");
        });
    }

    #[test]
    fn a_non_dict_value_errors() {
        Python::attach(|py| {
            let element = PyElement::new(PyList::empty(py).into_any().unbind());
            let err = element_to_row(&element, py, &mut None).unwrap_err();
            assert!(err.to_string().contains("must be a dict"), "got: {err}");
        });
    }

    #[test]
    fn format_strings_round_trip_and_an_unknown_one_errors() {
        assert_eq!(PolarsFormat::Parquet, format_kind("parquet").unwrap());
        assert_eq!(PolarsFormat::Ipc, format_kind("ipc").unwrap());
        let err = format_kind("csv").unwrap_err().to_string();
        assert!(
            err.contains("\"parquet\"") && err.contains("\"ipc\""),
            "got: {err}"
        );
    }
}
