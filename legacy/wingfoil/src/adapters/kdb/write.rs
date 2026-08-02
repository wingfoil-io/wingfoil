//! KDB+ write functionality for streaming data to q/kdb+ instances.

use super::KdbConnection;
use crate::nodes::{FutStream, RunParams, StreamOperators};
use crate::types::*;
use chrono::NaiveDateTime;
use futures::StreamExt;
use kdb_plus_fixed::ipc::{ConnectionMethod, K, QStream};
use kdb_plus_fixed::qtype;
use std::pin::Pin;
use std::rc::Rc;

/// Trait for serializing Rust types to KDB row data.
///
/// Implementors should create a K object representing a single row
/// that can be inserted into a KDB table.
///
/// **IMPORTANT**: Do NOT include the time field in your implementation. Time is
/// automatically extracted from the graph tuple `(NanoTime, T)` and prepended
/// to the row by the write adapter. Your struct should only contain business data.
pub trait KdbSerialize: Sized {
    /// Serialize self into a K object representing a row.
    ///
    /// The returned K object should be a compound list containing
    /// the column values (excluding time) in the same order as they
    /// appear in the target table schema after the time column.
    ///
    /// # Note
    /// Do not include time in the returned K object - it will be prepended automatically.
    fn to_kdb_row(&self) -> K;
}

/// Write stream data to a KDB+ table.
///
/// This function connects to a KDB+ instance and inserts records from the
/// upstream stream into the specified table. Each record is inserted using
/// a K object functional query: `(insert; `tablename; row_values)`.
///
/// # Type Parameters
/// * `T` - The record type, must implement `Element`, `Send`, and `KdbSerialize`
///
/// # Arguments
/// * `connection` - KDB connection configuration
/// * `table_name` - Name of the target table
/// * `upstream` - Stream of record batches to insert
///
/// # Returns
/// A Node that drives the write operation.
///
/// # Example
///
/// ```ignore
/// use wingfoil::adapters::kdb::*;
/// use wingfoil::*;
/// use std::time::Duration;
///
/// // Note: Trade struct does not include time - time is on-graph
/// #[derive(Debug, Clone, Default)]
/// struct Trade {
///     sym: String,
///     price: f64,
///     size: i64,
/// }
///
/// impl KdbSerialize for Trade {
///     fn to_kdb_row(&self) -> K {
///         // Return business data only - time will be prepended automatically
///         K::new_compound_list(vec![
///             K::new_symbol(self.sym.clone()),
///             K::new_float(self.price),
///             K::new_long(self.size),
///         ])
///     }
/// }
///
/// fn example() {
///     let conn = KdbConnection::new("localhost", 5000);
///     let trades: Rc<dyn Stream<Burst<Trade>>> = /* ... */
/// #       panic!("example code");
///     // Time from graph tuples will be prepended when writing to KDB
///     kdb_write(conn, "trades", &trades)
///         .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(10)))
///         .unwrap();
/// }
/// ```
#[must_use]
pub fn kdb_write<T>(
    connection: KdbConnection,
    table_name: impl Into<String>,
    upstream: &Rc<dyn Stream<Burst<T>>>,
) -> Rc<dyn Node>
where
    T: Element + Send + KdbSerialize + 'static,
{
    let table_name = table_name.into();

    let consumer = Box::new(
        move |_ctx: RunParams, source: Pin<Box<dyn FutStream<Burst<T>>>>| {
            kdb_write_consumer(connection, table_name, source)
        },
    );

    upstream.consume_async(consumer)
}

async fn kdb_write_consumer<T>(
    connection: KdbConnection,
    table_name: String,
    mut source: Pin<Box<dyn FutStream<Burst<T>>>>,
) -> anyhow::Result<()>
where
    T: Element + Send + KdbSerialize + 'static,
{
    let creds = connection.credentials_string();

    // Connect to KDB
    let mut socket = QStream::connect(
        ConnectionMethod::TCP,
        &connection.host,
        connection.port,
        &creds,
    )
    .await?;

    // Process incoming bursts
    while let Some((time, batch)) = source.next().await {
        // Compute timestamp string once per burst — all records share the same time.
        let naive: NaiveDateTime = time.into();
        let ts_str = naive.format("%Y.%m.%dD%H:%M:%S%.9f").to_string();

        // Serialize every record in the burst, then transpose to column-major.
        // Time is tracked separately as a string; to_kdb_row() has business fields.
        let rows: Vec<K> = batch
            .into_iter()
            .map(|record| record.to_kdb_row())
            .collect();
        let columns = k_rows_to_columns(rows)?;
        if columns.is_empty() {
            // Empty burst, or rows carried no business columns — nothing to insert.
            continue;
        }
        let n = columns[0].len(); // number of rows (1 per burst in typical usage)

        // Build column q-string fragments with explicit type suffixes.
        let ts_frag = if n == 1 {
            format!("enlist {ts_str}")
        } else {
            // All rows in a burst share the same timestamp.
            std::iter::repeat_n(ts_str.as_str(), n)
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut col_frags: Vec<String> = vec![ts_frag];
        for col in columns {
            col_frags.push(format_kdb_column_q(&col)?);
        }

        // Send as a q string: insert[`table; (ts_col; sym_col; price_col; qty_col)]
        let q = format!(
            "insert[`{table_name}; ({cols})]",
            cols = col_frags.join("; ")
        );
        let response = socket.send_sync_message(&q.as_str()).await?;
        if response.get_type() == qtype::ERROR {
            anyhow::bail!(
                "KDB insert error: {}",
                response.get_error_string().unwrap_or("unknown")
            );
        }
    }
    Ok(())
}

/// Transpose a burst of serialized rows (each the compound-list output of
/// [`KdbSerialize::to_kdb_row`]) into column-major K atoms.
///
/// Every row must be a compound list of the same width — the KDB table schema is
/// fixed, so a row that is not a compound list (a `KdbSerialize` impl returning a
/// bare atom) or a row of a different width than the first (a ragged burst) is a
/// programming error that would otherwise corrupt the insert or panic on an
/// out-of-bounds column index. Both are surfaced as errors here instead.
///
/// Returns an empty `Vec` when there are no rows, or when the rows carry no
/// columns (an empty compound list) — the caller skips the burst in that case.
fn k_rows_to_columns(rows: Vec<K>) -> anyhow::Result<Vec<Vec<K>>> {
    let serialized: Vec<Vec<K>> = rows
        .into_iter()
        .map(|row| {
            let row_type = row.get_type();
            row.as_vec::<K>().map(|v| v.to_vec()).map_err(|_| {
                anyhow::anyhow!(
                    "kdb_write: KdbSerialize::to_kdb_row must return a compound list, \
                     got K type {row_type}"
                )
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    if serialized.is_empty() {
        return Ok(Vec::new());
    }

    let n_cols = serialized[0].len();
    if let Some(bad) = serialized.iter().position(|r| r.len() != n_cols) {
        anyhow::bail!(
            "kdb_write: ragged burst — row {bad} has {} columns, expected {n_cols} \
             (all rows must share the table schema)",
            serialized[bad].len()
        );
    }

    let n = serialized.len();
    let mut columns: Vec<Vec<K>> = (0..n_cols).map(|_| Vec::with_capacity(n)).collect();
    for row in serialized {
        for (col_idx, val) in row.into_iter().enumerate() {
            columns[col_idx].push(val);
        }
    }
    Ok(columns)
}

/// q token for a `float` (64-bit) value, mapping non-finite values to q's native
/// null/infinity literals (`0n`, `0w`, `-0w`) instead of Rust's `NaN`/`inf`,
/// which are not valid q.
fn q_float64_token(v: f64) -> String {
    if v.is_nan() {
        "0n".to_string()
    } else if v == f64::INFINITY {
        "0w".to_string()
    } else if v == f64::NEG_INFINITY {
        "-0w".to_string()
    } else {
        format!("{v}")
    }
}

/// q token for a `real` (32-bit) value, mapping non-finite values to q's native
/// null/infinity literals (`0Ne`, `0we`, `-0we`).
fn q_float32_token(v: f32) -> String {
    if v.is_nan() {
        "0Ne".to_string()
    } else if v == f32::INFINITY {
        "0we".to_string()
    } else if v == f32::NEG_INFINITY {
        "-0we".to_string()
    } else {
        format!("{v}")
    }
}

/// Quote a symbol's text as a q string literal (`"..."`), escaping backslash and
/// double-quote so any symbol content is representable. Used to build symbols via
/// the string cast `` `$"..." ``, which — unlike a backtick literal — accepts
/// symbols containing `-`, spaces, and other non-name characters.
fn q_string_literal(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Format a column of same-typed K atoms as a q string fragment with correct type suffixes.
///
/// Produces literals like `` enlist `$"AAPL" ``, `enlist 42.5f`, `enlist 100j`, etc.
/// For multi-row columns produces space-joined values with a single trailing suffix.
fn format_kdb_column_q(atoms: &[K]) -> anyhow::Result<String> {
    if atoms.is_empty() {
        return Ok("()".to_string());
    }
    let col_type = atoms[0].get_type();
    match col_type {
        qtype::SYMBOL_ATOM => {
            let syms: anyhow::Result<Vec<String>> = atoms
                .iter()
                .map(|k| Ok(k.get_symbol()?.to_string()))
                .collect();
            let syms = syms?;
            // Build symbols via the string cast `` `$"..." `` rather than a
            // backtick literal `` `sym ``. A backtick literal terminates at the
            // first non-name character, so a symbol containing `-`, ` `, `+`
            // etc. (e.g. "BTC-USD") is otherwise mis-parsed by q — `` `BTC-USD ``
            // lexes as `` `BTC `` minus the variable `USD`. The string form is
            // valid for any symbol content.
            if syms.len() == 1 {
                Ok(format!("enlist `${}", q_string_literal(&syms[0])))
            } else {
                let parts: Vec<String> = syms.iter().map(|s| q_string_literal(s)).collect();
                Ok(format!("`$({})", parts.join(";")))
            }
        }
        qtype::FLOAT_ATOM => {
            let vals: anyhow::Result<Vec<f64>> = atoms.iter().map(|k| Ok(k.get_float()?)).collect();
            let vals = vals?;
            if vals.iter().all(|v| v.is_finite()) {
                if vals.len() == 1 {
                    Ok(format!("enlist {}f", vals[0]))
                } else {
                    let parts: Vec<_> = vals.iter().map(|v| format!("{v}")).collect();
                    Ok(format!("{}f", parts.join(" ")))
                }
            } else {
                // A non-finite value is present. `0n`/`0w`/`-0w` are already float
                // atoms, so a single value needs no suffix; multi-value columns are
                // wrapped in an explicit `float$(...) cast so the null/infinity
                // tokens parse unambiguously as one vector rather than via a
                // trailing type suffix.
                let parts: Vec<String> = vals.iter().map(|v| q_float64_token(*v)).collect();
                if parts.len() == 1 {
                    Ok(format!("enlist {}", parts[0]))
                } else {
                    Ok(format!("`float$({})", parts.join(";")))
                }
            }
        }
        qtype::LONG_ATOM => {
            let vals: anyhow::Result<Vec<i64>> = atoms.iter().map(|k| Ok(k.get_long()?)).collect();
            let vals = vals?;
            if vals.len() == 1 {
                Ok(format!("enlist {}j", vals[0]))
            } else {
                let parts: Vec<_> = vals.iter().map(|v| format!("{v}")).collect();
                Ok(format!("{}j", parts.join(" ")))
            }
        }
        qtype::INT_ATOM => {
            let vals: anyhow::Result<Vec<i32>> = atoms.iter().map(|k| Ok(k.get_int()?)).collect();
            let vals = vals?;
            if vals.len() == 1 {
                Ok(format!("enlist {}i", vals[0]))
            } else {
                let parts: Vec<_> = vals.iter().map(|v| format!("{v}")).collect();
                Ok(format!("{}i", parts.join(" ")))
            }
        }
        qtype::BOOL_ATOM => {
            let vals: anyhow::Result<Vec<bool>> = atoms.iter().map(|k| Ok(k.get_bool()?)).collect();
            let vals = vals?;
            if vals.len() == 1 {
                Ok(format!("enlist {}b", if vals[0] { 1 } else { 0 }))
            } else {
                let parts: Vec<_> = vals
                    .iter()
                    .map(|v| format!("{}", if *v { 1 } else { 0 }))
                    .collect();
                Ok(format!("{}b", parts.join(" ")))
            }
        }
        qtype::REAL_ATOM => {
            let vals: anyhow::Result<Vec<f32>> = atoms.iter().map(|k| Ok(k.get_real()?)).collect();
            let vals = vals?;
            if vals.iter().all(|v| v.is_finite()) {
                if vals.len() == 1 {
                    Ok(format!("enlist {}e", vals[0]))
                } else {
                    let parts: Vec<_> = vals.iter().map(|v| format!("{v}")).collect();
                    Ok(format!("{}e", parts.join(" ")))
                }
            } else {
                // Non-finite present — `0Ne`/`0we`/`-0we` are real atoms; wrap
                // multi-value columns in an explicit `real$(...) cast (see the
                // float arm for the rationale).
                let parts: Vec<String> = vals.iter().map(|v| q_float32_token(*v)).collect();
                if parts.len() == 1 {
                    Ok(format!("enlist {}", parts[0]))
                } else {
                    Ok(format!("`real$({})", parts.join(";")))
                }
            }
        }
        other => anyhow::bail!("unsupported KDB column type {other} in kdb_write"),
    }
}

/// Extension trait for writing streams to KDB+ tables.
///
/// This trait provides a fluent API for writing `Burst<T>` streams
/// (output of `combine`, `kdb_read`, etc.) to KDB+ tables.
pub trait KdbWriteOperators<T: Element> {
    /// Write this stream to a KDB+ table.
    ///
    /// # Arguments
    /// * `conn` - KDB connection configuration
    /// * `table` - Name of the target table
    ///
    /// # Returns
    /// A Node that drives the write operation.
    #[must_use]
    fn kdb_write(self: &Rc<Self>, conn: KdbConnection, table: &str) -> Rc<dyn Node>;
}

impl<T: Element + Send + KdbSerialize + 'static> KdbWriteOperators<T> for dyn Stream<Burst<T>> {
    fn kdb_write(self: &Rc<Self>, conn: KdbConnection, table: &str) -> Rc<dyn Node> {
        kdb_write(conn, table, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::burst;
    use kdb_plus_fixed::qtype;

    #[test]
    fn test_kdb_serialize_trait() {
        // Test that KdbSerialize trait can be implemented
        // Note: No time field - time is managed by the graph
        #[derive(Debug, Clone, Default)]
        struct TestRecord {
            sym: String,
            price: f64,
            size: i64,
        }

        impl KdbSerialize for TestRecord {
            fn to_kdb_row(&self) -> K {
                // Return business data only - time will be prepended by write adapter
                K::new_compound_list(vec![
                    K::new_symbol(self.sym.clone()),
                    K::new_float(self.price),
                    K::new_long(self.size),
                ])
            }
        }

        let record = TestRecord {
            sym: "AAPL".to_string(),
            price: 185.50,
            size: 100,
        };

        let row = record.to_kdb_row();
        assert_eq!(row.get_type(), qtype::COMPOUND_LIST);
    }

    #[test]
    fn symbols_with_special_chars_format_as_string_cast() {
        // A hyphenated symbol must not become a backtick literal (`` `BTC-USD ``
        // is mis-parsed by q as `` `BTC `` minus `USD`). It must use `` `$"..." ``.
        let one = format_kdb_column_q(&[K::new_symbol("BTC-USD".to_string())]).unwrap();
        assert_eq!(one, "enlist `$\"BTC-USD\"");

        let many = format_kdb_column_q(&[
            K::new_symbol("BTC-USD".to_string()),
            K::new_symbol("BTC-100K-3D-YES".to_string()),
        ])
        .unwrap();
        assert_eq!(many, "`$(\"BTC-USD\";\"BTC-100K-3D-YES\")");

        // Plain alphanumeric symbols still round-trip through the same path.
        assert_eq!(
            format_kdb_column_q(&[K::new_symbol("AAPL".to_string())]).unwrap(),
            "enlist `$\"AAPL\""
        );
    }

    #[test]
    fn non_finite_floats_use_q_null_and_infinity() {
        // NaN/±inf must become q's native tokens, never Rust's "NaN"/"inf",
        // which are invalid q and would make the whole insert fail.
        assert_eq!(
            format_kdb_column_q(&[K::new_float(f64::NAN)]).unwrap(),
            "enlist 0n"
        );
        assert_eq!(
            format_kdb_column_q(&[K::new_float(f64::INFINITY)]).unwrap(),
            "enlist 0w"
        );
        assert_eq!(
            format_kdb_column_q(&[K::new_float(f64::NEG_INFINITY)]).unwrap(),
            "enlist -0w"
        );
        // A mixed finite/non-finite column casts explicitly.
        assert_eq!(
            format_kdb_column_q(&[K::new_float(1.5), K::new_float(f64::NAN)]).unwrap(),
            "`float$(1.5;0n)"
        );
        // Finite-only columns are unchanged.
        assert_eq!(
            format_kdb_column_q(&[K::new_float(2.5)]).unwrap(),
            "enlist 2.5f"
        );
    }

    #[test]
    fn non_finite_reals_use_q_null_and_infinity() {
        assert_eq!(
            format_kdb_column_q(&[K::new_real(f32::NAN)]).unwrap(),
            "enlist 0Ne"
        );
        assert_eq!(
            format_kdb_column_q(&[K::new_real(f32::INFINITY)]).unwrap(),
            "enlist 0we"
        );
        assert_eq!(
            format_kdb_column_q(&[K::new_real(1.0), K::new_real(f32::NEG_INFINITY)]).unwrap(),
            "`real$(1;-0we)"
        );
    }

    #[test]
    fn non_compound_row_is_an_error_not_a_silent_drop() {
        // A KdbSerialize impl that returns a bare atom instead of a compound
        // list must fail loudly rather than serialize an empty row.
        let err =
            k_rows_to_columns(vec![K::new_float(1.0)]).expect_err("a bare atom is not a valid row");
        assert!(
            format!("{err}").contains("must return a compound list"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn ragged_burst_is_an_error_not_a_panic() {
        // Rows of differing widths previously panicked on an out-of-bounds
        // column index; now they surface as an error.
        let rows = vec![
            K::new_compound_list(vec![K::new_symbol("A".to_string()), K::new_float(1.0)]),
            K::new_compound_list(vec![K::new_symbol("B".to_string())]),
        ];
        let err = k_rows_to_columns(rows).expect_err("ragged rows must error");
        assert!(
            format!("{err}").contains("ragged burst"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn well_formed_burst_transposes_to_columns() {
        let rows = vec![
            K::new_compound_list(vec![K::new_symbol("A".to_string()), K::new_float(1.0)]),
            K::new_compound_list(vec![K::new_symbol("B".to_string()), K::new_float(2.0)]),
        ];
        let columns = k_rows_to_columns(rows).unwrap();
        assert_eq!(columns.len(), 2); // two columns
        assert_eq!(columns[0].len(), 2); // two rows each
        assert_eq!(columns[1].len(), 2);
    }

    #[test]
    fn test_kdb_write_node_creation() {
        use crate::nodes::constant;

        // Test that kdb_write creates a valid node
        #[derive(Debug, Clone, Default)]
        struct TestTrade {
            sym: String,
            price: f64,
        }

        impl KdbSerialize for TestTrade {
            fn to_kdb_row(&self) -> K {
                K::new_compound_list(vec![
                    K::new_symbol(self.sym.clone()),
                    K::new_float(self.price),
                ])
            }
        }

        let conn = KdbConnection::new("localhost", 5000);

        // Create a constant stream that produces TinyVec batches
        let trade = TestTrade {
            sym: "TEST".to_string(),
            price: 100.0,
        };
        let stream = constant(burst![trade]);

        // Verify we can create the kdb_write node (doesn't require actual KDB connection)
        let _node = kdb_write(conn, "test_table", &stream);
        // Node creation succeeds - actual connection would be attempted during run()
    }
}
