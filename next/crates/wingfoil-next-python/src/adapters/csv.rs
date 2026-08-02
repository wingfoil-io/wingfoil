//! Python bindings for the wingfoil-next **CSV** adapter
//! ([`wingfoil_next::adapters::csv`]).
//!
//! Two graph entry points, both `#[pyadapter]`-generated:
//!
//! | Python                  | Rust             | shape |
//! |-------------------------|------------------|-------|
//! | `csv_read(graph, …)`    | [`csv_read`]     | deterministic historical replay |
//! | `csv_write(stream, …)`  | [`CsvSinkOps`]   | file sink |
//!
//! # The dynamic edge
//!
//! The Rust adapter is **serde-typed**: a caller writes a record `struct` (or a
//! positional tuple) and the `csv` crate maps fields by name. Python has no such
//! struct, so both directions ride a positional `Vec<String>` and this module
//! supplies the column names:
//!
//! - **reads** open the file once at wiring to learn its header, then replay
//!   rows as `Vec<String>` and zip them back into a `dict` — so column *order*
//!   is preserved, which a `HashMap` record (what the legacy binding used) would
//!   have lost;
//! - **writes** take the caller's declared `columns` and marshal each `dict`
//!   into a positional row in that order.
//!
//! Values are `str` on both sides. CSV has no types, and guessing at
//! int/float/bool would make a round trip lossy in a way the caller cannot
//! control.
//!
//! # Why the sink needed an engine change
//!
//! [`csv_write`](CsvSinkOps::csv_write) derives its header from `T`'s serde
//! field names, and a positional `Vec<String>` has none — so a Python-written
//! file would have had **no header row at all**, which the legacy binding did
//! write. Rather than reimplement the sink here, this landed
//! [`csv_write_with_header`](CsvSinkOps::csv_write_with_header) on the Rust
//! trait: the same sink with the columns supplied explicitly. It is the escape
//! hatch any dynamic caller needs, not a Python-only concession.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **`csv_read` yields a list per tick, not a single dict.** Legacy
//!    `collapse()`d the burst and kept only the last row of any timestamp,
//!    silently dropping rows that shared one. Bursts erase to lists uniformly
//!    here, so the read is lossless. Call `[0]` for the single-row case. (This
//!    is the same deviation postgres took, for the same reason.)
//! 2. **Column order is preserved.** Legacy decoded into a `HashMap`, so the
//!    dict's key order was arbitrary. Here it is the file's header order.
//! 3. **A malformed timestamp aborts the run**, naming the row. Legacy silently
//!    substituted `NanoTime::ZERO`, which quietly reordered the replay.
//! 4. **New capability: `buffer_size`**, the replay's look-ahead bound, so a
//!    huge file is not read into memory up front.

use anyhow::{Result, anyhow, bail};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use wingfoil_next::NanoTime;
use wingfoil_next::adapters::csv::{CsvSinkOps, csv_read as csv_replay};
use wingfoil_next::prelude::{Burst, GraphBuilder, Stream, StreamOps};

use crate::adapters::common::RecordDict;
use crate::{PyElement, pyadapter};

/// The error-message prefix for the sink's marshaling failures.
const WHO: &str = "csv_write";

/// A replayed CSV row: the file's column names zipped with this row's fields.
///
/// Carries its own header (an `Rc` shared by every row of the read) so the
/// erasure step needs no side channel. Plain Rust data — no `Py<PyAny>` — so it
/// can cross from the adapter's producer task to the graph thread.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PyCsvRow {
    header: std::rc::Rc<Vec<String>>,
    fields: Vec<String>,
}

impl From<PyCsvRow> for PyElement {
    fn from(row: PyCsvRow) -> Self {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            for (name, value) in row.header.iter().zip(row.fields.iter()) {
                dict.set_item(name, value)
                    .expect("invariant: inserting a str key into a fresh PyDict cannot fail");
            }
            PyElement::new(dict.into_any().unbind())
        })
    }
}

/// Read the file's header row, so the replay can name its columns.
///
/// Done once at wiring — the same moment [`csv_read`] opens the file to fail
/// fast — so a missing file or a missing time column raises in Python rather
/// than aborting a later run. A header is required: without one there are no
/// column names to build a `dict` from, and no way to find `time_column`.
fn read_header(path: &str) -> Result<Vec<String>> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .map_err(|e| anyhow!("csv_read: failed to open {path}: {e}"))?;
    let header = reader
        .headers()
        .map_err(|e| anyhow!("csv_read: failed to read the header of {path}: {e}"))?;
    Ok(header.iter().map(str::to_string).collect())
}

/// The index of `time_column` in the header.
fn time_index(header: &[String], time_column: &str, path: &str) -> Result<usize> {
    header
        .iter()
        .position(|name| name == time_column)
        .ok_or_else(|| {
            anyhow!(
                "csv_read: no column '{time_column}' in {path}; the file's columns are {header:?}"
            )
        })
}

// ---------------------------------------------------------------------------
// Entry points.
// ---------------------------------------------------------------------------

/// Replay a CSV file as a deterministic historical source.
///
/// Each tick yields a `list` of `{column: value}` dicts — the rows sharing that
/// timestamp, in the file's column order, values as `str`. Call `[0]` for the
/// single-row case.
///
/// `time_column` names the column holding each row's timestamp as **integer
/// nanoseconds since the epoch**; it is looked up in the header at wiring, so a
/// misspelling raises here listing the file's actual columns. Row timestamps
/// must be **non-decreasing** — the replay rides the graph clock — and a row
/// whose timestamp does not parse aborts the run naming the row.
///
/// `buffer_size` bounds the replay's look-ahead (`None` = unbounded), so a huge
/// file is not read into memory up front. The file is opened at wiring, so a
/// missing one raises here rather than aborting a later run.
#[pyadapter(name = csv_read, source)]
#[pyo3(signature = (path, time_column, buffer_size = None))]
fn read(
    g: &GraphBuilder,
    path: String,
    time_column: String,
    buffer_size: Option<usize>,
) -> Result<Stream<Burst<PyCsvRow>>> {
    let header = read_header(&path)?;
    let index = time_index(&header, &time_column, &path)?;
    let header = std::rc::Rc::new(header);

    // The producer task runs off the graph thread, so `get_time` cannot hold the
    // `Rc` header — it only needs the index. A row whose timestamp does not
    // parse is stamped `ZERO` here and rejected by the `try_map` below, which is
    // where the error can actually be reported.
    let rows: Stream<Burst<Vec<String>>> = csv_replay(
        g,
        &path,
        move |fields: &Vec<String>| {
            fields
                .get(index)
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map_or(NanoTime::ZERO, NanoTime::new)
        },
        true,
        buffer_size,
    )?;

    let named = header.clone();
    Ok(rows.try_map(move |burst: &Burst<Vec<String>>| {
        burst
            .iter()
            .map(|fields| {
                // Re-check the timestamp on the graph thread, where an error can
                // be propagated. `get_time` above had to be infallible.
                let raw = fields.get(index).map(String::as_str).unwrap_or("");
                if raw.trim().parse::<u64>().is_err() {
                    bail!(
                        "csv_read: row has an unparseable '{}' value {raw:?} \
                         (expected integer nanoseconds); row = {fields:?}",
                        named[index],
                    );
                }
                Ok(PyCsvRow {
                    header: named.clone(),
                    fields: fields.clone(),
                })
            })
            .collect::<Result<Burst<PyCsvRow>>>()
    }))
}

/// Write this stream to a CSV file, **truncating** it first.
///
/// Each tick's value is either a single `dict` of the declared columns, or a
/// `list`/`tuple` of such dicts for several rows at one timestamp. The graph
/// timestamp is written as a leading `time` column, so the file's columns are
/// `(time, *columns)`.
///
/// `columns` is the list of column names, in order; each is looked up in every
/// dict and written as `str(value)`. A missing key aborts the run naming it —
/// pass an explicit empty string for a blank cell. Passing `columns=[]` writes
/// no header row.
///
/// Returns a terminal stream whose value is `None`.
#[pyadapter(name = csv_write)]
#[pyo3(signature = (path, columns))]
fn write(
    stream: &Stream<Burst<PyElement>>,
    path: String,
    columns: Vec<String>,
) -> Result<Stream<()>> {
    let cols = columns.clone();
    let rows: Stream<Burst<Vec<String>>> = stream.try_map(move |burst: &Burst<PyElement>| {
        burst
            .iter()
            .map(|elem| element_to_row(elem, &cols))
            .collect::<Result<Burst<Vec<String>>>>()
    });
    rows.csv_write_with_header(&path, &columns)
}

/// Marshal one erased stream value — a Python `dict` — into a positional row.
///
/// Values are stringified with `str()`, so any Python object with a sensible
/// `__str__` writes cleanly; only a *missing* column is an error.
fn element_to_row(elem: &PyElement, columns: &[String]) -> Result<Vec<String>> {
    Python::attach(|py| {
        let dict = crate::adapters::common::record_dict(elem, py, WHO, "of the declared columns")?;
        let record = RecordDict::new(&dict, WHO);
        columns
            .iter()
            .map(|name| {
                let value = record.get(name)?.ok_or_else(|| {
                    anyhow!(
                        "{WHO}: missing column '{name}' in dict \
                         (pass an empty string for a blank cell)"
                    )
                })?;
                value
                    .str()
                    .map(|s| s.to_string())
                    .map_err(|e| anyhow!("{WHO}: column '{name}' has no str(): {e}"))
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn dict_element(py: Python<'_>, items: &[(&str, Py<PyAny>)]) -> PyElement {
        let dict = PyDict::new(py);
        for (k, v) in items {
            dict.set_item(k, v).unwrap();
        }
        PyElement::new(dict.into_any().unbind())
    }

    fn str_obj(py: Python<'_>, s: &str) -> Py<PyAny> {
        s.into_pyobject(py).unwrap().into_any().unbind()
    }

    fn columns() -> Vec<String> {
        ["sym", "px"].into_iter().map(str::to_string).collect()
    }

    #[test]
    fn a_row_erases_to_a_dict_in_header_order() {
        let row = PyCsvRow {
            header: Rc::new(vec!["time".into(), "sym".into(), "px".into()]),
            fields: vec!["100".into(), "AAPL".into(), "1.5".into()],
        };
        let element = PyElement::from(row);
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let names: Vec<String> = dict
                .keys()
                .iter()
                .map(|k| k.extract::<String>().unwrap())
                .collect();
            assert_eq!(
                vec!["time", "sym", "px"],
                names,
                "header order is preserved"
            );
            assert_eq!(
                "AAPL",
                dict.get_item("sym")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap()
            );
        });
    }

    #[test]
    fn a_short_row_zips_only_the_columns_it_has() {
        // csv permits ragged rows; zip stops at the shorter side rather than
        // inventing empty cells.
        let row = PyCsvRow {
            header: Rc::new(vec!["a".into(), "b".into()]),
            fields: vec!["1".into()],
        };
        let element = PyElement::from(row);
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            assert_eq!(1, dict.len());
        });
    }

    #[test]
    fn a_dict_marshals_to_the_declared_column_order() {
        Python::attach(|py| {
            // Deliberately out of order: `columns` decides.
            let element = dict_element(
                py,
                &[("px", str_obj(py, "1.5")), ("sym", str_obj(py, "AAPL"))],
            );
            let row = element_to_row(&element, &columns()).unwrap();
            assert_eq!(vec!["AAPL".to_string(), "1.5".to_string()], row);
        });
    }

    #[test]
    fn non_string_values_are_stringified() {
        Python::attach(|py| {
            let element = dict_element(
                py,
                &[
                    ("sym", str_obj(py, "AAPL")),
                    ("px", 1.5_f64.into_pyobject(py).unwrap().into_any().unbind()),
                ],
            );
            let row = element_to_row(&element, &columns()).unwrap();
            assert_eq!(vec!["AAPL".to_string(), "1.5".to_string()], row);
        });
    }

    #[test]
    fn a_missing_column_errors() {
        Python::attach(|py| {
            let element = dict_element(py, &[("sym", str_obj(py, "AAPL"))]);
            let err = element_to_row(&element, &columns()).unwrap_err();
            assert!(
                err.to_string().contains("missing column 'px'"),
                "got: {err}"
            );
        });
    }

    #[test]
    fn a_non_dict_value_errors() {
        Python::attach(|py| {
            let element = PyElement::new(pyo3::types::PyList::empty(py).into_any().unbind());
            let err = element_to_row(&element, &columns()).unwrap_err();
            assert!(err.to_string().contains("must be a dict"), "got: {err}");
        });
    }

    #[test]
    fn time_index_finds_the_column_and_names_the_others_when_it_cannot() {
        let header = vec!["time".to_string(), "sym".to_string()];
        assert_eq!(0, time_index(&header, "time", "f.csv").unwrap());
        let err = time_index(&header, "ts", "f.csv").unwrap_err();
        assert!(err.to_string().contains("no column 'ts'"), "got: {err}");
        assert!(err.to_string().contains("sym"), "lists the actual columns");
    }
}
