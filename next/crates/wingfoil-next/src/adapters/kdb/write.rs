//! KDB+ write functionality for streaming data to q/kdb+ instances.

use std::sync::Arc;

use super::KdbConnection;
use crate::async_source::consume_async;
use crate::fluent::{Stream, StreamOps};
use crate::{Burst, burst};
use anyhow::{Context, Result};
use chrono::NaiveDateTime;
use kdb_plus_fixed::ipc::{ConnectionMethod, K, QStream};
use kdb_plus_fixed::qtype;
use wingfoil::NanoTime;

/// Trait for serializing Rust types to KDB row data.
///
/// Implementors create a K object representing a single row that can be inserted
/// into a KDB table.
///
/// **IMPORTANT**: Do NOT include the time field. Time is automatically extracted
/// from the graph tuple `(NanoTime, T)` and prepended to the row by the write
/// adapter. Your struct should only contain business data.
///
/// # Atom columns only
///
/// Each element of the returned compound list must be a **scalar atom** —
/// symbol, float, real, long, int, or bool. Vector- or nested-list-valued
/// columns are **not** writable: [`kdb_write`](KdbSinkOps::kdb_write) aborts the
/// run with an "unsupported KDB column type" error if a row carries one. This is
/// deliberately asymmetric with the read path, which *does* decode vector columns
/// (`Row::get(..).as_vec()`); the sink mirrors classic wingfoil, whose insert
/// builder is likewise atom-only.
pub trait KdbSerialize: Sized {
    /// Serialize self into a K object representing a row.
    ///
    /// The returned K object must be a compound list of **scalar atoms** — one
    /// per business column (excluding time), in the same order as they appear in
    /// the target table schema after the time column. Vector/nested columns are
    /// not supported on write (see the [trait docs](KdbSerialize)).
    ///
    /// # Note
    /// Do not include time in the returned K object — it is prepended
    /// automatically.
    fn to_kdb_row(&self) -> K;
}

/// One burst's worth of records sharing the burst's graph timestamp — the unit
/// [`consume_async`] drains per write, so an N-record burst is inserted as one
/// functional `insert` (matching classic's per-burst insert).
#[derive(Clone, Default)]
struct WriteBatch<T> {
    time: NanoTime,
    records: Vec<T>,
}

/// Fluent extension for writing `Burst<T>` streams to a KDB+ table.
///
/// Enabled with `use wingfoil_next::adapters::kdb::KdbSinkOps;`. Like classic, the
/// sink is **burst-only** (write single records via `constant(burst![record])` /
/// a `Stream<Burst<T>>`), because time is prepended per burst.
pub trait KdbSinkOps<T> {
    /// Write this stream to a KDB+ table.
    ///
    /// Each burst is inserted as one row-batch via a functional
    /// `insert[`table; (time_col; col1; …)]` query, the graph timestamp prepended
    /// as the first column. The target table's columns must line up positionally:
    /// `(time, <business columns in to_kdb_row() order>)`. Returns the sink
    /// `Stream<()>`.
    ///
    /// Only **scalar atom** columns are writable — a [`KdbSerialize`] impl that
    /// emits a vector/nested column aborts the run (see [`KdbSerialize`]).
    ///
    /// - `buffer_size`: back-pressure bound handed to
    ///   [`consume_async`](crate::async_source::consume_async) — `Some(n)` blocks
    ///   the graph once ~`n` bursts are unwritten; `None` is unbounded.
    ///
    /// The graph owns the tokio runtime (see the [module docs](super)).
    ///
    /// # Errors
    ///
    /// The connection is opened lazily on the first write (on the consumer task),
    /// so a connection failure aborts the *run* — not wiring — with context (the
    /// password is never included). A per-burst insert failure likewise aborts the
    /// run with context on a subsequent cycle.
    fn kdb_write(
        &self,
        conn: KdbConnection,
        table: &str,
        buffer_size: Option<usize>,
    ) -> Result<Stream<()>>;
}

impl<T> KdbSinkOps<T> for Stream<Burst<T>>
where
    T: KdbSerialize + Clone + Default + Send + 'static,
{
    fn kdb_write(
        &self,
        conn: KdbConnection,
        table: &str,
        buffer_size: Option<usize>,
    ) -> Result<Stream<()>> {
        let table_name = table.to_string();
        let g = self.graph();

        // The connection is established lazily on the consumer task on the first
        // write (see below), not here — so wiring opens no socket and a connect
        // failure surfaces during the run (via `consume_async`'s error channel).
        // `consume_async` runs each write to completion before the next (single
        // consumer), so this cell is never contended; the async Mutex only bridges
        // the `Fn` sink closure to the shared socket, off the graph path.
        let socket_cell: Arc<tokio::sync::Mutex<Option<QStream>>> =
            Arc::new(tokio::sync::Mutex::new(None));

        let (sink, flush) = consume_async(&g, buffer_size, move |batch: WriteBatch<T>| {
            let socket_cell = Arc::clone(&socket_cell);
            let conn = conn.clone();
            let table_name = table_name.clone();
            async move {
                if batch.records.is_empty() {
                    return Ok(());
                }
                let q = build_insert_query(&table_name, batch.time, &batch.records)?;
                if q.is_empty() {
                    // Rows carried no business columns — nothing to insert.
                    return Ok(());
                }

                // Connect on first use (deferred off the wiring path); cached for
                // subsequent bursts.
                let mut guard = socket_cell.lock().await;
                if guard.is_none() {
                    let creds = conn.credentials_string();
                    let socket =
                        QStream::connect(ConnectionMethod::TCP, &conn.host, conn.port, &creds)
                            .await
                            .with_context(|| {
                                format!("kdb_write: failed to connect to {}", conn.redacted())
                            })?;
                    *guard = Some(socket);
                }
                let socket = guard.as_mut().expect("socket initialised above");

                let response = socket
                    .send_sync_message(&q.as_str())
                    .await
                    .with_context(|| format!("kdb_write: insert into {table_name} failed"))?;
                if response.get_type() == qtype::ERROR {
                    anyhow::bail!(
                        "kdb_write: KDB insert error: {}",
                        response.get_error_string().unwrap_or("unknown")
                    );
                }
                Ok(())
            }
        })?;

        Ok(self
            .with_time()
            .map(
                |(time, burst): &(NanoTime, Burst<T>)| -> Burst<WriteBatch<T>> {
                    burst![WriteBatch {
                        time: *time,
                        records: burst.iter().cloned().collect(),
                    }]
                },
            )
            .for_each(sink)
            .finally(flush))
    }
}

/// Build the `insert[`table; (time_col; col1; …)]` q string for one burst.
///
/// Returns an empty `String` when the records carry no business columns (an empty
/// compound list) — the caller skips the burst in that case.
fn build_insert_query<T: KdbSerialize>(
    table_name: &str,
    time: NanoTime,
    records: &[T],
) -> Result<String> {
    // All records in a burst share the graph timestamp.
    let naive: NaiveDateTime = time.into();
    let ts_str = naive.format("%Y.%m.%dD%H:%M:%S%.9f").to_string();

    // Serialize every record, then transpose to column-major.
    let rows: Vec<K> = records.iter().map(|record| record.to_kdb_row()).collect();
    let columns = k_rows_to_columns(rows)?;
    if columns.is_empty() {
        return Ok(String::new());
    }
    let n = columns[0].len(); // number of rows (1 per burst in typical usage)

    // Build column q-string fragments with explicit type suffixes.
    let ts_frag = if n == 1 {
        format!("enlist {ts_str}")
    } else {
        std::iter::repeat_n(ts_str.as_str(), n)
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut col_frags: Vec<String> = vec![ts_frag];
    for col in columns {
        col_frags.push(format_kdb_column_q(&col)?);
    }

    Ok(format!(
        "insert[`{table_name}; ({cols})]",
        cols = col_frags.join("; ")
    ))
}

/// Transpose a burst of serialized rows (each the compound-list output of
/// [`KdbSerialize::to_kdb_row`]) into column-major K atoms.
///
/// Every row must be a compound list of the same width — the KDB table schema is
/// fixed, so a row that is not a compound list (a `KdbSerialize` impl returning a
/// bare atom) or a row of a different width than the first (a ragged burst) is a
/// programming error surfaced as an error here rather than corrupting the insert
/// or panicking on an out-of-bounds column index.
///
/// Returns an empty `Vec` when there are no rows, or when the rows carry no
/// columns (an empty compound list).
fn k_rows_to_columns(rows: Vec<K>) -> Result<Vec<Vec<K>>> {
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
        .collect::<Result<Vec<_>>>()?;

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

/// Format a column of same-typed K atoms as a q string fragment with correct type
/// suffixes.
///
/// Produces literals like `` enlist `$"AAPL" ``, `enlist 42.5f`, `enlist 100j`,
/// etc. For multi-row columns produces space-joined values with a single trailing
/// suffix (`100 200j`, `1.5 2.5f`): q applies the trailing type suffix
/// (`j`/`i`/`f`/`e`/`b`) to the whole space-separated literal, casting it to one
/// typed vector — so whole-number floats (which Rust's `{}` prints without a
/// decimal, e.g. `100`) still parse as floats rather than longs.
///
/// # Why `format!("{v}")` round-trips through q for floats
///
/// Rust's default `{}` formatting of `f64`/`f32` emits the shortest decimal that
/// round-trips to the same IEEE-754 bits and never uses exponent notation, and
/// q parses a decimal float literal to the nearest IEEE-754 double/real — so a
/// finite value serialised here is recovered by q with identical bits. Non-finite
/// values (`NaN`/`±inf`) are not valid q decimals and are handled separately by
/// [`q_float64_token`]/[`q_float32_token`] (q's native null/infinity tokens),
/// never reaching the `format!("{v}")` path.
fn format_kdb_column_q(atoms: &[K]) -> Result<String> {
    if atoms.is_empty() {
        return Ok("()".to_string());
    }
    let col_type = atoms[0].get_type();
    match col_type {
        qtype::SYMBOL_ATOM => {
            let syms: Result<Vec<String>> = atoms
                .iter()
                .map(|k| Ok(k.get_symbol()?.to_string()))
                .collect();
            let syms = syms?;
            // Build symbols via the string cast `` `$"..." `` rather than a
            // backtick literal `` `sym ``. A backtick literal terminates at the
            // first non-name character, so a symbol containing `-`, ` `, `+` etc.
            // (e.g. "BTC-USD") is otherwise mis-parsed by q. The string form is
            // valid for any symbol content.
            if syms.len() == 1 {
                Ok(format!("enlist `${}", q_string_literal(&syms[0])))
            } else {
                let parts: Vec<String> = syms.iter().map(|s| q_string_literal(s)).collect();
                Ok(format!("`$({})", parts.join(";")))
            }
        }
        qtype::FLOAT_ATOM => {
            let vals: Result<Vec<f64>> = atoms.iter().map(|k| Ok(k.get_float()?)).collect();
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
                // tokens parse unambiguously as one vector.
                let parts: Vec<String> = vals.iter().map(|v| q_float64_token(*v)).collect();
                if parts.len() == 1 {
                    Ok(format!("enlist {}", parts[0]))
                } else {
                    Ok(format!("`float$({})", parts.join(";")))
                }
            }
        }
        qtype::LONG_ATOM => {
            let vals: Result<Vec<i64>> = atoms.iter().map(|k| Ok(k.get_long()?)).collect();
            let vals = vals?;
            if vals.len() == 1 {
                Ok(format!("enlist {}j", vals[0]))
            } else {
                let parts: Vec<_> = vals.iter().map(|v| format!("{v}")).collect();
                Ok(format!("{}j", parts.join(" ")))
            }
        }
        qtype::INT_ATOM => {
            let vals: Result<Vec<i32>> = atoms.iter().map(|k| Ok(k.get_int()?)).collect();
            let vals = vals?;
            if vals.len() == 1 {
                Ok(format!("enlist {}i", vals[0]))
            } else {
                let parts: Vec<_> = vals.iter().map(|v| format!("{v}")).collect();
                Ok(format!("{}i", parts.join(" ")))
            }
        }
        qtype::BOOL_ATOM => {
            let vals: Result<Vec<bool>> = atoms.iter().map(|k| Ok(k.get_bool()?)).collect();
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
            let vals: Result<Vec<f32>> = atoms.iter().map(|k| Ok(k.get_real()?)).collect();
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
                // multi-value columns in an explicit `real$(...) cast.
                let parts: Vec<String> = vals.iter().map(|v| q_float32_token(*v)).collect();
                if parts.len() == 1 {
                    Ok(format!("enlist {}", parts[0]))
                } else {
                    Ok(format!("`real$({})", parts.join(";")))
                }
            }
        }
        other => anyhow::bail!(
            "kdb_write: unsupported KDB column type {other} — to_kdb_row must return \
             scalar atom columns only (symbol/float/real/long/int/bool); vector or \
             nested-list columns cannot be written"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kdb_plus_fixed::qattribute;

    #[test]
    fn symbols_with_special_chars_format_as_string_cast() {
        let one = format_kdb_column_q(&[K::new_symbol("BTC-USD".to_string())]).unwrap();
        assert_eq!(one, "enlist `$\"BTC-USD\"");

        let many = format_kdb_column_q(&[
            K::new_symbol("BTC-USD".to_string()),
            K::new_symbol("BTC-100K-3D-YES".to_string()),
        ])
        .unwrap();
        assert_eq!(many, "`$(\"BTC-USD\";\"BTC-100K-3D-YES\")");

        assert_eq!(
            format_kdb_column_q(&[K::new_symbol("AAPL".to_string())]).unwrap(),
            "enlist `$\"AAPL\""
        );
    }

    #[test]
    fn non_finite_floats_use_q_null_and_infinity() {
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
        assert_eq!(
            format_kdb_column_q(&[K::new_float(1.5), K::new_float(f64::NAN)]).unwrap(),
            "`float$(1.5;0n)"
        );
        assert_eq!(
            format_kdb_column_q(&[K::new_float(2.5)]).unwrap(),
            "enlist 2.5f"
        );
    }

    #[test]
    fn multi_value_finite_columns_use_typed_vector_literals() {
        // A whole-number float prints without a decimal (`100`), but the trailing
        // `f` casts the whole space-separated literal to a float vector in q — so
        // the column stays a float, not a long. Pins the reliance that
        // `format!("{v}")` + a type suffix yields a valid typed q vector.
        assert_eq!(
            format_kdb_column_q(&[K::new_float(100.0), K::new_float(200.5)]).unwrap(),
            "100 200.5f"
        );
        assert_eq!(
            format_kdb_column_q(&[K::new_real(1.5), K::new_real(2.5)]).unwrap(),
            "1.5 2.5e"
        );
        assert_eq!(
            format_kdb_column_q(&[K::new_long(1), K::new_long(2), K::new_long(3)]).unwrap(),
            "1 2 3j"
        );
        assert_eq!(
            format_kdb_column_q(&[K::new_int(10), K::new_int(20)]).unwrap(),
            "10 20i"
        );
    }

    #[test]
    fn unsupported_column_type_names_the_atom_only_limitation() {
        // A vector-valued column (here an int list) is not writable; the error
        // must point at the atom-only limitation rather than a bare type code.
        let err = format_kdb_column_q(&[K::new_int_list(vec![1, 2], qattribute::NONE)])
            .expect_err("a vector column must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("scalar atom columns only"),
            "unexpected: {msg}"
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
        let err =
            k_rows_to_columns(vec![K::new_float(1.0)]).expect_err("a bare atom is not a valid row");
        assert!(
            format!("{err}").contains("must return a compound list"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn ragged_burst_is_an_error_not_a_panic() {
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
        assert_eq!(columns.len(), 2);
        assert_eq!(columns[0].len(), 2);
        assert_eq!(columns[1].len(), 2);
    }

    #[test]
    fn build_insert_query_prepends_time_and_enlists_single_row() {
        #[derive(Clone, Default)]
        struct Trade {
            sym: String,
            price: f64,
        }
        impl KdbSerialize for Trade {
            fn to_kdb_row(&self) -> K {
                K::new_compound_list(vec![
                    K::new_symbol(self.sym.clone()),
                    K::new_float(self.price),
                ])
            }
        }
        let q = build_insert_query(
            "trades",
            NanoTime::from_kdb_timestamp(0),
            &[Trade {
                sym: "AAPL".to_string(),
                price: 1.5,
            }],
        )
        .unwrap();
        assert_eq!(
            q,
            "insert[`trades; (enlist 2000.01.01D00:00:00.000000000; enlist `$\"AAPL\"; enlist 1.5f)]"
        );
    }

    #[test]
    fn build_insert_query_empty_records_is_empty_string() {
        #[derive(Clone, Default)]
        struct Empty;
        impl KdbSerialize for Empty {
            fn to_kdb_row(&self) -> K {
                K::new_compound_list(vec![])
            }
        }
        let q = build_insert_query("t", NanoTime::from_kdb_timestamp(0), &[Empty]).unwrap();
        assert_eq!(q, "");
    }
}
