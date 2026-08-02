//! KDB+ historical read: [`kdb_read`] plus the shared K-object row access
//! ([`KdbExt`], [`Rows`], [`Row`]) and the [`KdbDeserialize`] trait that
//! [`kdb_read`], [`kdb_read_cached`](super::kdb_read_cached), and
//! [`kdb_sub`](super::kdb_sub) all decode rows with.

use super::{KdbConnection, Sym, SymbolInterner};
use crate::Burst;
use crate::adapters::common::{TimeWindow, WindowFilter, compute_validated_time_slices};
use crate::async_source::{RunParams, produce_async};
use crate::fluent::{GraphBuilder, Stream};
use anyhow::{Context, Result, bail};
use kdb_plus_fixed::ipc::error::Error as KdbError;
use kdb_plus_fixed::ipc::{ConnectionMethod, K, QStream};
use kdb_plus_fixed::qtype;
use log::info;
use wingfoil_next::{NanoTime, RunFor, RunMode};

/// Extension trait for extracting data from K objects.
pub trait KdbExt {
    /// Extract column names from a KDB table.
    ///
    /// For tables (qtype 98), the result is a flipped dictionary where the keys
    /// are column names.
    ///
    /// # Errors
    /// Returns an error if the K object is not a table.
    fn column_names(&self) -> Result<Vec<String>>;

    /// Get a row accessor for iterating over table rows.
    ///
    /// Tables are stored column-wise in KDB. This returns a [`Rows`] struct that
    /// provides zero-allocation row iteration via indexed access.
    ///
    /// # Errors
    /// Returns an error if the K object is not a table.
    fn rows(&self) -> Result<Rows>;

    /// Get element at index from a K list/vector.
    ///
    /// # Errors
    /// Returns an error if the index is out of bounds or the type is not a list.
    fn element_at(&self, index: usize) -> std::result::Result<K, KdbError>;
}

/// Row accessor for a KDB table.
///
/// Provides zero-allocation iteration by giving indexed access to column values.
pub struct Rows {
    columns: Vec<K>,
    n_rows: usize,
}

impl Rows {
    /// Returns the number of rows.
    pub fn len(&self) -> usize {
        self.n_rows
    }

    /// Returns true if there are no rows.
    pub fn is_empty(&self) -> bool {
        self.n_rows == 0
    }

    /// Build a [`Rows`] accessor directly from a list of column vectors.
    ///
    /// A tickerplant `upd` payload is a bare list of per-column vectors rather
    /// than a flipped table dictionary; this wraps that list so
    /// [`kdb_sub`](super::kdb_sub) can reuse the same indexed row access as
    /// [`kdb_read`].
    pub(super) fn from_column_list(columns: Vec<K>) -> Self {
        let n_rows = columns.first().map(K::len).unwrap_or(0);
        Rows { columns, n_rows }
    }

    /// Get a row by index.
    pub fn get(&self, index: usize) -> Option<Row<'_>> {
        if index < self.n_rows {
            Some(Row {
                columns: &self.columns,
                index,
            })
        } else {
            None
        }
    }

    /// Iterate over rows.
    pub fn iter(&self) -> RowIter<'_> {
        RowIter {
            columns: &self.columns,
            n_rows: self.n_rows,
            current: 0,
        }
    }
}

impl<'a> IntoIterator for &'a Rows {
    type Item = Row<'a>;
    type IntoIter = RowIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over rows in a KDB table.
pub struct RowIter<'a> {
    columns: &'a [K],
    n_rows: usize,
    current: usize,
}

impl<'a> Iterator for RowIter<'a> {
    type Item = Row<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current < self.n_rows {
            let row = Row {
                columns: self.columns,
                index: self.current,
            };
            self.current += 1;
            Some(row)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.n_rows - self.current;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for RowIter<'_> {}

/// A single row in a KDB table.
///
/// Provides indexed access to column values without allocation.
#[derive(Clone, Copy)]
pub struct Row<'a> {
    columns: &'a [K],
    index: usize,
}

impl Row<'_> {
    /// Get value at column index.
    pub fn get(&self, col: usize) -> std::result::Result<K, KdbError> {
        self.columns
            .get(col)
            .ok_or(KdbError::IndexOutOfBounds {
                index: col,
                length: self.columns.len(),
            })?
            .element_at(self.index)
    }

    /// Get a KDB timestamp column as [`NanoTime`].
    ///
    /// Reads the column value as an i64 (KDB nanoseconds since 2000-01-01) and
    /// converts it to a [`NanoTime`] (nanoseconds since the Unix epoch).
    pub fn get_timestamp(&self, col: usize) -> std::result::Result<NanoTime, KdbError> {
        Ok(NanoTime::from_kdb_timestamp(self.get(col)?.get_long()?))
    }

    /// Get an interned symbol from a symbol column, avoiding per-row String
    /// allocation.
    ///
    /// Accesses the underlying `Vec<String>` directly and interns the `&str`,
    /// bypassing `element_at` (which clones the String into a new K object).
    pub fn get_sym(
        &self,
        col: usize,
        interner: &mut SymbolInterner,
    ) -> std::result::Result<Sym, KdbError> {
        let column = self.columns.get(col).ok_or(KdbError::IndexOutOfBounds {
            index: col,
            length: self.columns.len(),
        })?;
        let strings = column
            .as_vec::<String>()
            .map_err(|_| KdbError::InvalidOperation {
                operator: "get_sym",
                operand_type: "K",
                expected: Some("symbol list"),
            })?;
        let s = strings.get(self.index).ok_or(KdbError::IndexOutOfBounds {
            index: self.index,
            length: strings.len(),
        })?;
        Ok(interner.intern(s))
    }

    /// Number of columns.
    pub fn len(&self) -> usize {
        self.columns.len()
    }

    /// Returns true if the row has no columns.
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}

impl KdbExt for K {
    fn column_names(&self) -> Result<Vec<String>> {
        if self.get_type() != qtype::TABLE {
            bail!("expected table (qtype 98), got qtype {}", self.get_type());
        }

        let dict = self.get_dictionary()?;
        let dict_parts = dict.as_vec::<K>()?;
        let keys = dict_parts
            .first()
            .ok_or_else(|| anyhow::anyhow!("table dictionary has no keys"))?;
        let symbols = keys.as_vec::<String>()?;
        Ok(symbols.clone())
    }

    fn rows(&self) -> Result<Rows> {
        if self.get_type() != qtype::TABLE {
            bail!("expected table (qtype 98), got qtype {}", self.get_type());
        }

        let dict = self.get_dictionary()?;
        let dict_parts = dict.as_vec::<K>()?;

        if dict_parts.len() < 2 {
            bail!("table dictionary missing values");
        }

        let values = &dict_parts[1];
        let columns = values.as_vec::<K>()?.clone();
        let n_rows = columns.first().map(|c| c.len()).unwrap_or(0);

        Ok(Rows { columns, n_rows })
    }

    fn element_at(&self, index: usize) -> std::result::Result<K, KdbError> {
        let ktype = self.get_type();
        let len = self.len();

        let result = match ktype {
            qtype::LONG_LIST | qtype::TIMESTAMP_LIST | qtype::TIMESPAN_LIST => self
                .as_vec::<i64>()
                .ok()
                .and_then(|v| v.get(index).map(|&x| K::new_long(x))),
            qtype::FLOAT_LIST => self
                .as_vec::<f64>()
                .ok()
                .and_then(|v| v.get(index).map(|&x| K::new_float(x))),
            qtype::SYMBOL_LIST => self
                .as_vec::<String>()
                .ok()
                .and_then(|v| v.get(index).map(|x| K::new_symbol(x.clone()))),
            qtype::STRING => self
                .as_vec::<u8>()
                .ok()
                .and_then(|v| v.get(index).map(|&x| K::new_byte(x))),
            qtype::INT_LIST | qtype::DATE_LIST | qtype::TIME_LIST => self
                .as_vec::<i32>()
                .ok()
                .and_then(|v| v.get(index).map(|&x| K::new_int(x))),
            qtype::SHORT_LIST => self
                .as_vec::<i16>()
                .ok()
                .and_then(|v| v.get(index).map(|&x| K::new_short(x))),
            qtype::BOOL_LIST => self
                .as_vec::<bool>()
                .ok()
                .and_then(|v| v.get(index).map(|&x| K::new_bool(x))),
            qtype::REAL_LIST => self
                .as_vec::<f32>()
                .ok()
                .and_then(|v| v.get(index).map(|&x| K::new_real(x))),
            qtype::COMPOUND_LIST => self.as_vec::<K>().ok().and_then(|v| v.get(index).cloned()),
            _ => {
                return Err(KdbError::InvalidOperation {
                    operator: "element_at",
                    operand_type: "K",
                    expected: Some("list type"),
                });
            }
        };

        result.ok_or(KdbError::IndexOutOfBounds { index, length: len })
    }
}

/// Turn a tickerplant `upd` payload into a [`Rows`] accessor.
///
/// The third element of a `(`upd; table; data)` message is either a table
/// (qtype 98) or a bare list of column vectors, depending on the tickerplant.
/// Both are normalised to [`Rows`] so [`kdb_sub`](super::kdb_sub) can decode them
/// with the same [`KdbDeserialize`] impls that [`kdb_read`] uses.
pub(super) fn upd_payload_rows(data: &K) -> Result<Rows> {
    if data.get_type() == qtype::TABLE {
        data.rows()
    } else {
        let columns = data
            .as_vec::<K>()
            .map_err(|_| {
                anyhow::anyhow!(
                    "kdb_sub: upd payload is neither a table nor a list of columns (qtype {})",
                    data.get_type()
                )
            })?
            .clone();
        Ok(Rows::from_column_list(columns))
    }
}

/// Trait for deserializing KDB row data into Rust types.
///
/// Implementors extract fields from the row using indexed column access and
/// return a `(NanoTime, Self)` tuple — the time is owned by the implementation
/// rather than the adapter, giving full control over which column carries the
/// timestamp.
///
/// Use [`Row::get_timestamp`] to extract a KDB timestamp column as [`NanoTime`].
pub trait KdbDeserialize: Sized {
    /// Deserialize a KDB row into `(NanoTime, Self)`.
    ///
    /// # Arguments
    /// * `row` - Row accessor providing indexed access to column values
    /// * `columns` - Column names from the table schema
    /// * `interner` - Symbol interner for deduplicating symbol strings via
    ///   [`Row::get_sym`]
    fn from_kdb_row(
        row: Row<'_>,
        columns: &[String],
        interner: &mut SymbolInterner,
    ) -> std::result::Result<(NanoTime, Self), KdbError>;
}

/// A slice to query: the caller-built query string and the on-graph
/// [`TimeWindow`] its rows are expected to fall in.
type Slice = (String, TimeWindow);

/// Lazily query each slice and yield its in-window rows as a `(NanoTime, T)`
/// stream — the streaming counterpart of legacy `chunk_stream`.
///
/// This is an `async_stream` generator: it calls `next_slice()` (→ runs the next
/// KDB query) only when polled past the previous slice's rows, so — driven by
/// [`produce_async`](crate::async_source::produce_async)'s
/// back-pressure — a slice is fetched only once the graph has room for it. Memory
/// is bounded to a slice plus the producer's look-ahead, and I/O pipelines with
/// graph compute (legacy's model).
///
/// A query, decode, or non-monotonic-time failure yields a trailing `Err` and
/// stops (aborting the run through the producer stream — legacy's per-row
/// abort). Rows outside the slice's [`TimeWindow`] are dropped via
/// [`WindowFilter`]. `prev_time` is reset each slice so time-of-day columns work
/// across date partitions (timestamps restart at midnight on each new date).
fn chunk_stream<T>(
    mut socket: QStream,
    mut next_slice: impl FnMut() -> Option<Slice> + Send + 'static,
) -> impl futures::Stream<Item = Result<(NanoTime, T)>> + Send + 'static
where
    T: KdbDeserialize + Send + 'static,
{
    async_stream::stream! {
        let mut interner = SymbolInterner::default();

        'outer: while let Some((query, window)) = next_slice() {
            info!("KDB query: {query}");
            let fetch_start = std::time::Instant::now();
            let result: K = match socket.send_sync_message(&query.as_str()).await {
                Ok(r) => r,
                Err(e) => { yield Err(e.into()); break; }
            };

            let (columns, rows) = match (result.column_names(), result.rows()) {
                (Ok(cols), Ok(rows)) => (cols, rows),
                (Err(e), _) | (_, Err(e)) => { yield Err(e); break; }
            };

            let row_count = rows.len();
            info!("KDB query: {} rows in {:?}", row_count, fetch_start.elapsed());

            let mut prev_time: Option<NanoTime> = None;
            let mut filter = WindowFilter::new("kdb_read", window);
            for row in &rows {
                let (time, record) = match T::from_kdb_row(row, &columns, &mut interner) {
                    Ok(r) => r,
                    Err(e) => { yield Err(e.into()); break 'outer; }
                };

                // Drop rows the query returned outside the run window (before
                // start_time, at/after end_time, or beyond the slice bounds).
                // Emitting them would drive the monotonic graph clock backwards.
                if !filter.keep(time) {
                    continue;
                }

                if let Some(prev) = prev_time
                    && time < prev
                {
                    yield Err(anyhow::anyhow!(
                        "KDB data is not sorted by time: got {time:?} after {prev:?}. \
                         Add `xasc` to your query to sort the data."
                    ));
                    break 'outer;
                }
                prev_time = Some(time);

                yield Ok((time, record));
            }
            filter.finish();
        }
    }
}

/// Read a time-partitioned KDB+ table, one query per time slice, as a
/// deterministic historical replay `Burst<T>` source.
///
/// The run's `[start, end)` window (from `RunMode::HistoricalFrom` +
/// `RunFor::Duration`, taken from `params`) is split into contiguous, half-open,
/// midnight-aligned slices of length `period`. `query_fn` is called once per
/// slice with `((t0, t1), date, iteration)` and must return a q query filtering
/// on `time >= (`timestamp$){t0}j, time < (`timestamp$){t1}j`, sorted by time
/// (`xasc`). The window is validated + sliced at **wiring** (a pure check); the
/// connect and slice queries run at the start of the run via
/// [`produce_async`](crate::async_source::produce_async), which
/// replays the decoded, in-window rows at their timestamps.
///
/// See the [module docs](super) for the window-clamp / `date` / `iteration`
/// semantics and the burst grouping. Run the graph with the same
/// `RunMode::HistoricalFrom` / `RunFor` described by `params`.
///
/// `buffer_size` bounds the producer→graph backlog as back-pressure (like
/// legacy): `Some(n)` caps the replay to ~`n` timestamp-groups of look-ahead, so
/// a slice is fetched only as the graph drains — bounded memory, pipelined I/O.
/// `None` is unbounded (a fast KDB feeding a slower graph accumulates a backlog).
///
/// # Errors
///
/// Returns an error at **wiring time** only if `params` does not describe a
/// bounded historical run (`RunMode::RealTime`, a zero start, `RunFor::Forever`,
/// or `RunFor::Cycles`) — the slice set would be unbounded or undefined. This is
/// a pure check; wiring does no I/O. The connect + slice queries run at the start
/// of the run, so a connection failure (host:port only — never the password), a
/// query failure, a decode failure, or a non-monotonic timestamp **aborts the
/// run**, not wiring.
pub fn kdb_read<T>(
    g: &GraphBuilder,
    params: RunParams,
    connection: KdbConnection,
    period: std::time::Duration,
    query_fn: impl FnMut((NanoTime, NanoTime), i32, usize) -> String + Send + 'static,
    buffer_size: Option<usize>,
) -> Result<Stream<Burst<T>>>
where
    T: KdbDeserialize + Clone + Default + Send + 'static,
{
    // Validate the run window and split it into slices at wiring — a pure,
    // fail-fast check with no I/O. A RealTime run yields ZERO, which the shared
    // validator rejects with a "use RunMode::HistoricalFrom" message.
    let start_time = match params.run_mode {
        RunMode::HistoricalFrom(t) => t,
        RunMode::RealTime => NanoTime::ZERO,
    };
    let end_time_result: Result<NanoTime> = match params.run_for {
        RunFor::Duration(d) => Ok(start_time + d),
        RunFor::Forever => Ok(NanoTime::MAX),
        RunFor::Cycles(_) => Err(anyhow::anyhow!("end_time not available for RunFor::Cycles")),
    };
    // Capture the concrete bound before the validator consumes the Result
    // (NanoTime is Copy); validation guarantees it is present (bounded, non-MAX).
    let end_time_bound = end_time_result.as_ref().ok().copied();
    let slices = compute_validated_time_slices("kdb_read", start_time, end_time_result, period)?;
    let end_time =
        end_time_bound.expect("compute_validated_time_slices accepted a bounded end_time");

    // Defer the connect + slice queries to the run via `produce_async`: nothing
    // touches the network at wiring, so a connection or query failure aborts the
    // *run* (not graph construction) — matching legacy's lazy `produce_async`
    // reader. The connect happens here (in the closure, before the stream); the
    // per-slice queries run lazily inside `chunk_stream` as the graph drains.
    produce_async(
        g,
        move |_p: RunParams| async move {
            let creds = connection.credentials_string();
            let socket = QStream::connect(
                ConnectionMethod::TCP,
                &connection.host,
                connection.port,
                &creds,
            )
            .await
            .with_context(|| format!("kdb_read: failed to connect to {}", connection.redacted()))?;

            let mut slices_iter = slices.into_iter();
            let mut query_fn = query_fn;
            // The query uses the period-aligned (t0, t1) for clean round-number
            // boundaries, but rows are clamped to the run's [start_time, end_time) so
            // out-of-window rows are dropped rather than aborting the run. `t0` may
            // precede `start_time` on the first slice; `t1` may exceed `end_time` on
            // the last.
            let slice_fn = move || -> Option<Slice> {
                let ((t0, t1), date, iteration) = slices_iter.next()?;
                let window = TimeWindow::clamp(t0, t1, start_time, end_time);
                let query = query_fn((t0, t1), date, iteration);
                Some((query, window))
            };
            Ok(chunk_stream::<T>(socket, slice_fn))
        },
        buffer_size,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use kdb_plus_fixed::qattribute;

    #[test]
    fn test_nanotime_from_kdb_timestamp() {
        // KDB timestamp 0 = 2000-01-01 00:00:00; Unix 2000-01-01 = 946684800 s.
        let nano = NanoTime::from_kdb_timestamp(0);
        assert_eq!(u64::from(nano), 946_684_800_000_000_000);

        let nano = NanoTime::from_kdb_timestamp(1_000_000_000); // 1s after KDB epoch
        assert_eq!(u64::from(nano), 946_684_801_000_000_000);
    }

    #[test]
    fn test_nanotime_kdb_timestamp_round_trip() {
        let original = NanoTime::new(1_000_000_000_000_000_000);
        assert_eq!(
            NanoTime::from_kdb_timestamp(original.to_kdb_timestamp()),
            original
        );

        let kdb_epoch = NanoTime::new(946_684_800_000_000_000);
        assert_eq!(kdb_epoch.to_kdb_timestamp(), 0);
    }

    #[test]
    fn upd_payload_from_column_list_wraps_bare_columns() {
        // A bare list of column vectors (not a flipped table) is normalised so
        // kdb_sub can decode it with the same Row access kdb_read uses.
        let data = K::new_compound_list(vec![
            K::new_long_list(vec![10i64, 20], qattribute::NONE),
            K::new_symbol_list(vec!["A".into(), "B".into()], qattribute::NONE),
        ]);
        let rows = upd_payload_rows(&data).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.get(0).unwrap().get(0).unwrap().get_long().unwrap(), 10);
    }

    /// Round-trip test for `KdbSerialize` + `KdbDeserialize` covering every
    /// supported KDB column type, ported verbatim from legacy `read.rs`.
    #[test]
    fn test_all_types_serde_round_trip() {
        use super::super::KdbSerialize;

        #[derive(Debug, Clone, Default)]
        struct AllTypesRecord {
            date: i32,
            timestamp: i64,
            int_val: i32,
            float_val: f64,
            sym: String,
            string_val: String,
            vec_int: Vec<i32>,
            vec_float: Vec<f64>,
        }

        impl KdbDeserialize for AllTypesRecord {
            fn from_kdb_row(
                row: Row<'_>,
                _columns: &[String],
                interner: &mut SymbolInterner,
            ) -> std::result::Result<(NanoTime, Self), KdbError> {
                let time = row.get_timestamp(1)?;
                Ok((
                    time,
                    AllTypesRecord {
                        date: row.get(0)?.get_int()?,
                        timestamp: row.get(1)?.get_long()?,
                        int_val: row.get(2)?.get_int()?,
                        float_val: row.get(3)?.get_float()?,
                        sym: row.get_sym(4, interner)?.to_string(),
                        string_val: row.get(5)?.as_string()?.to_string(),
                        vec_int: row.get(6)?.as_vec::<i32>()?.to_vec(),
                        vec_float: row.get(7)?.as_vec::<f64>()?.to_vec(),
                    },
                ))
            }
        }

        impl KdbSerialize for AllTypesRecord {
            fn to_kdb_row(&self) -> K {
                K::new_compound_list(vec![
                    K::new_int(self.date),
                    K::new_long(self.timestamp),
                    K::new_int(self.int_val),
                    K::new_float(self.float_val),
                    K::new_symbol(self.sym.clone()),
                    K::new_string(self.string_val.clone(), qattribute::NONE),
                    K::new_int_list(self.vec_int.clone(), qattribute::NONE),
                    K::new_float_list(self.vec_float.clone(), qattribute::NONE),
                ])
            }
        }

        let kdb_date: i32 = 7305;
        let kdb_ts: i64 = 3_600_000_000_000;
        let int_val: i32 = 42;
        let float_val: f64 = 1.234_567_891;
        let sym_str = "AAPL";
        let string_val = "hello";
        let vec_int = vec![10i32, 20, 30];
        let vec_float = vec![1.1f64, 2.2, 3.3];

        let header = K::new_symbol_list(
            [
                "date",
                "timestamp",
                "int_val",
                "float_val",
                "sym",
                "string_val",
                "vec_int",
                "vec_float",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            qattribute::NONE,
        );
        let columns = K::new_compound_list(vec![
            K::new_int_list(vec![kdb_date], qattribute::NONE),
            K::new_long_list(vec![kdb_ts], qattribute::NONE),
            K::new_int_list(vec![int_val], qattribute::NONE),
            K::new_float_list(vec![float_val], qattribute::NONE),
            K::new_symbol_list(vec![sym_str.to_string()], qattribute::NONE),
            K::new_compound_list(vec![K::new_string(
                string_val.to_string(),
                qattribute::NONE,
            )]),
            K::new_compound_list(vec![K::new_int_list(vec_int.clone(), qattribute::NONE)]),
            K::new_compound_list(vec![K::new_float_list(vec_float.clone(), qattribute::NONE)]),
        ]);
        let table = K::new_dictionary(header, columns).unwrap().flip().unwrap();

        let rows = table.rows().unwrap();
        assert_eq!(rows.len(), 1);

        let mut interner = SymbolInterner::default();
        let (_, record) =
            AllTypesRecord::from_kdb_row(rows.get(0).unwrap(), &[], &mut interner).unwrap();

        assert_eq!(record.date, kdb_date);
        assert_eq!(record.timestamp, kdb_ts);
        assert_eq!(record.int_val, int_val);
        assert!((record.float_val - float_val).abs() < 1e-10);
        assert_eq!(record.sym, sym_str);
        assert_eq!(record.string_val, string_val);
        assert_eq!(record.vec_int, vec_int);
        for (a, b) in record.vec_float.iter().zip(&vec_float) {
            assert!((a - b).abs() < 1e-10, "vec_float element mismatch");
        }

        let krow = record.to_kdb_row();
        assert_eq!(krow.get_type(), qtype::COMPOUND_LIST);
        let fields = krow.as_vec::<K>().unwrap();
        assert_eq!(fields.len(), 8);
        assert_eq!(fields[0].get_type(), qtype::INT_ATOM);
        assert_eq!(fields[1].get_type(), qtype::LONG_ATOM);
        assert_eq!(fields[4].get_type(), qtype::SYMBOL_ATOM);
        assert_eq!(fields[5].get_type(), qtype::STRING);
        assert_eq!(fields[6].get_type(), qtype::INT_LIST);
        assert_eq!(fields[7].get_type(), qtype::FLOAT_LIST);
        assert_eq!(fields[0].get_int().unwrap(), kdb_date);
        assert_eq!(fields[4].get_symbol().unwrap(), sym_str);
        assert_eq!(fields[5].as_string().unwrap(), string_val);
        assert_eq!(*fields[6].as_vec::<i32>().unwrap(), vec_int);
    }
}
