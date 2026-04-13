//! Python bindings for the CSV adapter.

use crate::py_element::PyElement;
use crate::py_stream::PyStream;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;
use wingfoil::adapters::csv::csv_read;
use wingfoil::{Burst, NanoTime, Node, Stream, StreamOperators};

type CsvRow = HashMap<String, String>;

/// Read a CSV file into a stream of dicts.
///
/// Each tick yields a dict where keys are column headers and values are strings.
/// Rows sharing the same timestamp are collapsed to one-per-tick.
///
/// Args:
///     path: Path to the CSV file (headers required)
///     time_column: Name of the column containing the timestamp
///                  (parsed as integer nanoseconds since epoch)
#[pyfunction]
pub fn py_csv_read(path: String, time_column: String) -> PyStream {
    let stream: Rc<dyn Stream<Burst<CsvRow>>> = csv_read(
        &path,
        move |row: &CsvRow| {
            row.get(&time_column)
                .and_then(|s| s.parse::<u64>().ok())
                .map(NanoTime::new)
                .unwrap_or(NanoTime::ZERO)
        },
        true,
    );

    let py_stream = stream.collapse().map(|row: CsvRow| {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            for (key, value) in &row {
                dict.set_item(key, value).unwrap();
            }
            PyElement::new(dict.into_any().unbind())
        })
    });

    PyStream(py_stream)
}

/// State for the CSV writer: column order and a buffered writer.
struct CsvWriteState {
    file: std::io::BufWriter<std::fs::File>,
    headers: Option<Vec<String>>,
}

impl CsvWriteState {
    fn new(path: &str) -> Self {
        let file = std::fs::File::create(path)
            .unwrap_or_else(|e| panic!("failed to open CSV file for writing: {e}"));
        Self {
            file: std::io::BufWriter::new(file),
            headers: None,
        }
    }

    fn write_row(&mut self, header_row: &[String], values: &[String]) {
        let line = values
            .iter()
            .enumerate()
            .fold(String::new(), |mut acc, (i, v)| {
                if i > 0 {
                    acc.push(',');
                }
                // Simple CSV escaping: quote if contains comma, quote, or newline
                if v.contains(',') || v.contains('"') || v.contains('\n') {
                    acc.push('"');
                    acc.push_str(&v.replace('"', "\"\""));
                    acc.push('"');
                } else {
                    acc.push_str(v);
                }
                acc
            });
        let _ = writeln!(self.file, "{line}");
        let _ = header_row; // used by caller to set headers, not needed here
    }
}

/// Inner implementation for the `.csv_write()` stream method.
///
/// Writes each dict as a CSV row. Headers are inferred from the first dict's keys.
/// A `time` column is prepended with the graph time in nanoseconds.
pub fn py_csv_write_inner(stream: &Rc<dyn Stream<PyElement>>, path: String) -> Rc<dyn Node> {
    let state: RefCell<CsvWriteState> = RefCell::new(CsvWriteState::new(&path));

    stream.for_each(move |elem, time| {
        Python::attach(|py| {
            let obj = elem.as_ref().bind(py);
            if let Ok(dict) = obj.cast::<PyDict>() {
                let mut guard = state.borrow_mut();

                if guard.headers.is_none() {
                    let keys: Vec<String> = dict
                        .keys()
                        .iter()
                        .map(|k| k.extract::<String>().unwrap_or_default())
                        .collect();
                    let mut header_row = vec!["time".to_string()];
                    header_row.extend(keys.clone());
                    guard.write_row(&[], &header_row);
                    guard.headers = Some(keys);
                }

                let h = guard.headers.clone().unwrap();
                let time_nanos: u64 = time.into();
                let mut values = vec![time_nanos.to_string()];
                for key in &h {
                    let v = dict
                        .get_item(key)
                        .ok()
                        .flatten()
                        .map(|v| v.str().map(|s| s.to_string()).unwrap_or_default())
                        .unwrap_or_default();
                    values.push(v);
                }
                guard.write_row(&[], &values);
            }
        });
    })
}
