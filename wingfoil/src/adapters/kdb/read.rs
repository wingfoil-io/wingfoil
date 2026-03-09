//! KDB+ read functionality for streaming data from q/kdb+ instances.

use super::{KdbConnection, Sym, SymbolInterner};
use crate::nodes::produce_async;
use crate::types::*;
use anyhow::{Result, bail};
use kdb_plus_fixed::ipc::error::Error as KdbError;
use kdb_plus_fixed::ipc::{ConnectionMethod, K, QStream};
use kdb_plus_fixed::qtype;
use log::info;
use std::rc::Rc;

/// Extension trait for extracting data from K objects.
pub trait KdbExt {
    /// Extract column names from a KDB table.
    ///
    /// For tables (qtype 98), the result is a flipped dictionary where the keys are column names.
    ///
    /// # Errors
    /// Returns an error if the K object is not a table.
    fn column_names(&self) -> Result<Vec<String>>;

    /// Get a row accessor for iterating over table rows.
    ///
    /// Tables are stored column-wise in KDB. This returns a `Rows` struct
    /// that provides zero-allocation row iteration via indexed access.
    ///
    /// # Errors
    /// Returns an error if the K object is not a table.
    fn rows(&self) -> Result<Rows>;

    /// Get element at index from a K list/vector.
    ///
    /// # Errors
    /// Returns an error if the index is out of bounds or the type is not a list.
    fn element_at(&self, index: usize) -> Result<K, KdbError>;
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
    pub fn get(&self, col: usize) -> Result<K, KdbError> {
        self.columns
            .get(col)
            .ok_or(KdbError::IndexOutOfBounds {
                index: col,
                length: self.columns.len(),
            })?
            .element_at(self.index)
    }

    /// Get an interned symbol from a symbol column, avoiding per-row String allocation.
    ///
    /// Accesses the underlying `Vec<String>` directly and interns the `&str`,
    /// bypassing `element_at` (which clones the String into a new K object).
    pub fn get_sym(&self, col: usize, interner: &mut SymbolInterner) -> Result<Sym, KdbError> {
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

    /// Returns true if row has no columns.
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

    fn element_at(&self, index: usize) -> Result<K, KdbError> {
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

/// Trait for deserializing KDB row data into Rust types.
///
/// Implementors should extract fields from the row using indexed column access.
/// The `columns` parameter provides column names for reference.
///
/// **IMPORTANT**: Do NOT extract the time column in your implementation. Time is
/// automatically extracted by the adapter and propagated through the graph as part
/// of the `(NanoTime, T)` tuple. Your struct should only contain business data.
pub trait KdbDeserialize: Sized {
    /// Deserialize a KDB row into Self.
    ///
    /// # Arguments
    /// * `row` - Row accessor providing indexed access to column values
    /// * `columns` - Column names from the table schema
    /// * `interner` - Symbol interner for deduplicating symbol strings via [`Row::get_sym`]
    ///
    /// # Note
    /// Skip the time column - it's handled automatically by the adapter.
    fn from_kdb_row(
        row: Row<'_>,
        columns: &[String],
        interner: &mut SymbolInterner,
    ) -> Result<Self, KdbError>;
}

/// Core streaming loop driven by a caller-supplied query closure.
///
/// Calls `query_fn(last_count)` before each chunk:
/// - `None` on the first call
/// - `Some(n)` on subsequent calls, where `n` is the row count returned by the previous query
///
/// Returns `None` to stop, or `Some(query_string)` to execute next.
/// Also stops automatically when a query returns 0 rows.
///
/// `prev_time` is reset each chunk so time-of-day columns work correctly when
/// advancing across date partitions (timestamps restart at midnight on each new date).
fn chunk_stream<T>(
    mut socket: QStream,
    time_col: String,
    mut query_fn: impl FnMut(Option<usize>) -> Option<String> + Send + 'static,
) -> impl futures::Stream<Item = anyhow::Result<(NanoTime, T)>> + Send + 'static
where
    T: KdbDeserialize + Send + 'static,
{
    async_stream::stream! {
        let mut last_count: Option<usize> = None;
        let mut interner = SymbolInterner::default();

        while let Some(query) = query_fn(last_count) {
            info!("KDB query: {}", query);
            let fetch_start = std::time::Instant::now();
            let result: K = match socket.send_sync_message(&query.as_str()).await {
                Ok(r) => r,
                Err(e) => {
                    yield Err(anyhow::Error::new(e).context(format!("KDB query failed: {}", query)));
                    break;
                }
            };

            let (columns, rows) = match (result.column_names(), result.rows()) {
                (Ok(cols), Ok(rows)) => (cols, rows),
                (Err(e), _) | (_, Err(e)) => {
                    yield Err(anyhow::anyhow!("{}\nkdb query failed with\n{}", query, e));
                    break;
                }
            };

            let row_count = rows.len();
            info!("KDB query: {} rows in {:?}", row_count, fetch_start.elapsed());

            let time_col_idx = match columns.iter().position(|c| c == &time_col) {
                Some(idx) => idx,
                None => {
                    yield Err(anyhow::anyhow!(
                        "time column '{}' not found in result columns: {:?}",
                        time_col, columns
                    ));
                    break;
                }
            };

            let mut prev_time: Option<i64> = None;
            let mut row_error = false;
            for row in &rows {
                let time_kdb = match row.get(time_col_idx).and_then(|v| v.get_long()) {
                    Ok(t) => t,
                    Err(e) => {
                        yield Err(anyhow::Error::new(e).context(format!("failed to extract time from KDB row: {}", query)));
                        row_error = true;
                        break;
                    }
                };

                if let Some(prev) = prev_time
                    && time_kdb < prev
                {
                    yield Err(anyhow::anyhow!(
                        "KDB data is not sorted by time column '{}': got {} after {}. \
                        Add `{} xasc` to your query to sort the data.\nQuery: {}",
                        time_col, time_kdb, prev, time_col, query
                    ));
                    row_error = true;
                    break;
                }
                prev_time = Some(time_kdb);

                let time = NanoTime::from_kdb_timestamp(time_kdb);

                let record = match T::from_kdb_row(row, &columns, &mut interner) {
                    Ok(r) => r,
                    Err(e) => {
                        yield Err(anyhow::Error::new(e).context(format!("KDB deserialization failed: {}", query)));
                        row_error = true;
                        break;
                    }
                };

                yield Ok((time, record));
            }
            if row_error {
                break;
            }

            last_count = Some(row_count);
        }
    }
}

/// Stream data from KDB+ using a caller-supplied query closure for full control over
/// chunking strategy.
///
/// The closure receives `Option<usize>`:
/// - `None` on the first call — return the initial query
/// - `Some(n)` after each chunk — `n` is the row count from the last query
///
/// Return `Some(query_string)` to execute the next chunk, or `None` to stop.
/// A query that returns 0 rows calls the closure with `Some(0)` — the closure
/// decides whether to stop (return `None`) or skip ahead (return another query).
///
/// Use this directly when you need query-level control, such as advancing through date
/// partitions:
///
/// ```ignore
/// let dates = vec!["2024.01.01", "2024.01.02"];
/// let chunk = 10_000usize;
/// let mut di = 0usize;
/// let mut offset = 0usize;
///
/// kdb_read_chunks::<Trade, _>(conn, move |last_count| {
///     match last_count {
///         None => {}
///         Some(n) if n < chunk => { di += 1; offset = 0; } // date exhausted, advance
///         Some(n) => { offset += n; }                       // more on this date
///     }
///     if di >= dates.len() { return None; }
///     Some(format!("select[{},{}] from trades where date={}", offset, chunk, dates[di]))
/// }, "time")
/// ```
#[must_use]
pub fn kdb_read_chunks<T, F>(
    connection: KdbConnection,
    query_fn: F,
    time_col: &str,
) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + Send + KdbDeserialize + 'static,
    F: FnMut(Option<usize>) -> Option<String> + Send + 'static,
{
    let time_col = time_col.to_string();
    produce_async(move |_ctx| async move {
        let creds = connection.credentials_string();
        let socket = QStream::connect(
            ConnectionMethod::TCP,
            &connection.host,
            connection.port,
            &creds,
        )
        .await?;
        Ok(chunk_stream::<T>(socket, time_col, query_fn))
    })
}

fn compute_time_slices(
    start_time: NanoTime,
    end_time: NanoTime,
    period: std::time::Duration,
) -> Vec<((NanoTime, NanoTime), i32, usize)> {
    const DAY_NANOS: i64 = 86_400_000_000_000;
    let period_nanos = period.as_nanos() as i64;

    let start_kdb = start_time.to_kdb_timestamp();
    let end_kdb = end_time.to_kdb_timestamp();

    let start_day = start_kdb.div_euclid(DAY_NANOS);
    let end_day = end_kdb.div_euclid(DAY_NANOS);

    let mut result = Vec::new();

    for day in start_day..=end_day {
        let kdb_date = day as i32;
        let midnight_kdb = day * DAY_NANOS;
        let next_midnight_kdb = midnight_kdb + DAY_NANOS;
        let day_end_kdb = next_midnight_kdb - 1; // 23:59:59.999999999

        let mut iteration = 0usize;
        let mut slice_start = midnight_kdb;

        loop {
            let natural_end = slice_start + period_nanos;
            // Half-open intervals: slice ends at natural_end-1 so adjacent slices don't
            // overlap. For the last slice of a day, natural_end-1 >= day_end_kdb so the
            // min() clamps it to 23:59:59.999999999.
            let slice_end = (natural_end - 1).min(day_end_kdb);

            result.push((
                (
                    NanoTime::from_kdb_timestamp(slice_start),
                    NanoTime::from_kdb_timestamp(slice_end),
                ),
                kdb_date,
                iteration,
            ));

            if natural_end >= next_midnight_kdb {
                break;
            }

            slice_start = natural_end;
            iteration += 1;
        }
    }

    result
}

#[must_use]
pub fn kdb_read<T, F>(
    connection: KdbConnection,
    period: std::time::Duration,
    query_fn: F,
    time_col: &str,
) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + Send + KdbDeserialize + 'static,
    F: FnMut((NanoTime, NanoTime), i32, usize) -> String + Send + 'static,
{
    let time_col = time_col.to_string();
    produce_async(move |ctx| {
        let start_time = ctx.start_time;
        let end_time_result = ctx.end_time();

        async move {
            if start_time == NanoTime::ZERO {
                anyhow::bail!(
                    "kdb_read_time_sliced: start_time is NanoTime::ZERO; \
                    use RunMode::HistoricalFrom with an explicit start time"
                );
            }

            let end_time = match end_time_result {
                Ok(t) if t == NanoTime::MAX => anyhow::bail!(
                    "kdb_read_time_sliced requires RunFor::Duration; \
                    RunFor::Forever would generate an unbounded number of slices"
                ),
                Ok(t) => t,
                Err(_) => anyhow::bail!(
                    "kdb_read_time_sliced requires RunFor::Duration; \
                    RunFor::Cycles does not provide an end time"
                ),
            };

            let slices = compute_time_slices(start_time, end_time, period);

            let creds = connection.credentials_string();
            let socket = QStream::connect(
                ConnectionMethod::TCP,
                &connection.host,
                connection.port,
                &creds,
            )
            .await?;

            // Convert the slice-based iteration into the Option<usize> protocol that
            // chunk_stream expects. The row count from the previous query is ignored —
            // we always advance to the next slice unconditionally.
            // The stream stops when slices are exhausted.
            let mut slices_iter = slices.into_iter();
            let mut query_fn = query_fn;
            let slice_fn = move |_last_count: Option<usize>| -> Option<String> {
                let (within, date, iteration) = slices_iter.next()?;
                Some(query_fn(within, date, iteration))
            };

            Ok(chunk_stream::<T>(socket, time_col, slice_fn))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nanotime_from_kdb_timestamp() {
        // KDB timestamp 0 = 2000-01-01 00:00:00
        // Unix timestamp for 2000-01-01 = 946684800 seconds = 946684800000000000 nanos
        let kdb_time: i64 = 0;
        let nano = NanoTime::from_kdb_timestamp(kdb_time);
        assert_eq!(u64::from(nano), 946_684_800_000_000_000);

        // Test with 1 second after KDB epoch
        let kdb_time: i64 = 1_000_000_000; // 1 second in nanos
        let nano = NanoTime::from_kdb_timestamp(kdb_time);
        assert_eq!(u64::from(nano), 946_684_801_000_000_000);
    }

    #[test]
    fn test_nanotime_kdb_timestamp_round_trip() {
        // Test round-trip conversion: NanoTime -> KDB timestamp -> NanoTime
        let original = NanoTime::new(1_000_000_000_000_000_000); // Some Unix timestamp
        let kdb_ts = original.to_kdb_timestamp();
        let restored = NanoTime::from_kdb_timestamp(kdb_ts);
        assert_eq!(original, restored);

        // Test with KDB epoch (2000-01-01)
        let kdb_epoch = NanoTime::new(946_684_800_000_000_000);
        assert_eq!(kdb_epoch.to_kdb_timestamp(), 0);

        // Test with values after KDB epoch
        let after_epoch = NanoTime::new(946_684_801_000_000_000); // 1 second after
        assert_eq!(after_epoch.to_kdb_timestamp(), 1_000_000_000);
    }

    fn kdb_epoch() -> NanoTime {
        // 2000-01-01 midnight = kdb_date 0
        NanoTime::from_kdb_timestamp(0)
    }

    const DAY_NANOS: u64 = 86_400_000_000_000;

    #[test]
    fn test_compute_time_slices_no_stub() {
        // 8-hour period divides 24h evenly → 3 slices, no stub
        let epoch = kdb_epoch();
        let period = std::time::Duration::from_secs(8 * 3600);
        let start = epoch;
        let end = NanoTime::new(u64::from(epoch) + DAY_NANOS - 1);

        let slices = compute_time_slices(start, end, period);
        assert_eq!(slices.len(), 3, "expected 3 slices for 8h period");

        for &(_, date, _) in &slices {
            assert_eq!(date, 0);
        }

        let period_nanos = period.as_nanos() as u64;

        let (s0, e0) = slices[0].0;
        assert_eq!(u64::from(s0), u64::from(epoch));
        assert_eq!(u64::from(e0), u64::from(epoch) + period_nanos - 1);

        let (s1, e1) = slices[1].0;
        assert_eq!(u64::from(s1), u64::from(epoch) + period_nanos);
        assert_eq!(u64::from(e1), u64::from(epoch) + 2 * period_nanos - 1);

        // Last slice ends at 23:59:59.999999999
        let (s2, e2) = slices[2].0;
        assert_eq!(u64::from(s2), u64::from(epoch) + 2 * period_nanos);
        assert_eq!(u64::from(e2), u64::from(epoch) + DAY_NANOS - 1);

        assert_eq!(slices[0].2, 0);
        assert_eq!(slices[1].2, 1);
        assert_eq!(slices[2].2, 2);
    }

    #[test]
    fn test_compute_time_slices_with_stub() {
        // 5-hour period: 24h / 5h = 4 full slices + 4h stub → 5 slices total
        let epoch = kdb_epoch();
        let period = std::time::Duration::from_secs(5 * 3600);
        let start = epoch;
        let end = NanoTime::new(u64::from(epoch) + DAY_NANOS - 1);

        let slices = compute_time_slices(start, end, period);
        assert_eq!(slices.len(), 5, "expected 4 full + 1 stub = 5 slices");

        let period_nanos = period.as_nanos() as u64;

        // Stub: starts at 20h, ends at 23:59:59.999999999
        let (stub_start, stub_end) = slices[4].0;
        assert_eq!(u64::from(stub_start), u64::from(epoch) + 4 * period_nanos);
        assert_eq!(u64::from(stub_end), u64::from(epoch) + DAY_NANOS - 1);

        let stub_duration = u64::from(stub_end) - u64::from(stub_start) + 1;
        assert!(
            stub_duration < period_nanos,
            "stub should be shorter than a full period"
        );

        // Contiguous and non-overlapping: end of slice n + 1 == start of slice n+1
        for i in 0..4 {
            let end_i = u64::from(slices[i].0.1);
            let start_next = u64::from(slices[i + 1].0.0);
            assert_eq!(end_i + 1, start_next, "gap/overlap at slice {i}/{}", i + 1);
        }
    }

    #[test]
    fn test_compute_time_slices_two_days() {
        // Two days → iterations reset on day 1
        let epoch = kdb_epoch();
        let period = std::time::Duration::from_secs(12 * 3600); // 2 slices/day
        let start = epoch;
        let end = NanoTime::new(u64::from(epoch) + 2 * DAY_NANOS - 1);

        let slices = compute_time_slices(start, end, period);
        assert_eq!(slices.len(), 4); // 2 slices × 2 days

        assert_eq!(slices[0].1, 0);
        assert_eq!(slices[0].2, 0);
        assert_eq!(slices[1].1, 0);
        assert_eq!(slices[1].2, 1);
        assert_eq!(slices[2].1, 1);
        assert_eq!(slices[2].2, 0); // resets to 0 on new day
        assert_eq!(slices[3].1, 1);
        assert_eq!(slices[3].2, 1);
    }

    /// Round-trip test for `KdbSerialize` + `KdbDeserialize` covering every
    /// supported KDB column type:
    ///
    /// | Rust field   | KDB column type      | element_at extraction |
    /// |--------------|----------------------|-----------------------|
    /// | `date`       | INT_LIST (DATE_LIST) | `get_int()` → i32     |
    /// | `timestamp`  | LONG_LIST (TIMESTAMP_LIST) | `get_long()` → i64 |
    /// | `int_val`    | INT_LIST             | `get_int()` → i32     |
    /// | `float_val`  | FLOAT_LIST           | `get_float()` → f64   |
    /// | `sym`        | SYMBOL_LIST          | `get_sym()` → Sym     |
    /// | `string_val` | COMPOUND_LIST[STRING]| `as_string()` → &str  |
    /// | `vec_int`    | COMPOUND_LIST[INT_LIST]  | `as_vec::<i32>()` |
    /// | `vec_float`  | COMPOUND_LIST[FLOAT_LIST]| `as_vec::<f64>()` |
    #[test]
    fn test_all_types_serde_round_trip() {
        use super::super::KdbSerialize;
        use kdb_plus_fixed::qattribute;

        #[derive(Debug, Clone, Default)]
        struct AllTypesRecord {
            /// KDB date integer: days since 2000-01-01.
            date: i32,
            /// KDB timestamp integer: nanoseconds since 2000-01-01.
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
            ) -> Result<Self, KdbError> {
                Ok(AllTypesRecord {
                    date: row.get(0)?.get_int()?,
                    timestamp: row.get(1)?.get_long()?,
                    int_val: row.get(2)?.get_int()?,
                    float_val: row.get(3)?.get_float()?,
                    sym: row.get_sym(4, interner)?.to_string(),
                    // col 5: COMPOUND_LIST[STRING] — element_at returns the STRING K;
                    // as_string() extracts the &str directly from its internal symbol storage.
                    string_val: row.get(5)?.as_string()?.to_string(),
                    // col 6: COMPOUND_LIST[INT_LIST] — element_at returns the INT_LIST K
                    vec_int: row.get(6)?.as_vec::<i32>()?.to_vec(),
                    // col 7: COMPOUND_LIST[FLOAT_LIST] — element_at returns the FLOAT_LIST K
                    vec_float: row.get(7)?.as_vec::<f64>()?.to_vec(),
                })
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

        // ── Known test values ────────────────────────────────────────────────
        let kdb_date: i32 = 7305; // 2000.01.01 + 7305 days = 2020.01.01
        let kdb_ts: i64 = 3_600_000_000_000; // 2000.01.01D01:00:00
        let int_val: i32 = 42;
        let float_val: f64 = 1.234_567_891;
        let sym_str = "AAPL";
        let string_val = "hello";
        let vec_int = vec![10i32, 20, 30];
        let vec_float = vec![1.1f64, 2.2, 3.3];

        // ── Build a single-row KDB table ─────────────────────────────────────
        // INT_LIST and LONG_LIST are used for date/timestamp columns because
        // element_at handles DATE_LIST/INT_LIST and TIMESTAMP_LIST/LONG_LIST
        // identically (same underlying i32 / i64 storage).
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

        // ── Deserialize ───────────────────────────────────────────────────────
        let rows = table.rows().unwrap();
        assert_eq!(rows.len(), 1);

        let mut interner = SymbolInterner::default();
        let record =
            AllTypesRecord::from_kdb_row(rows.get(0).unwrap(), &[], &mut interner).unwrap();

        assert_eq!(record.date, kdb_date);
        assert_eq!(record.timestamp, kdb_ts);
        assert_eq!(record.int_val, int_val);
        assert!(
            (record.float_val - float_val).abs() < 1e-10,
            "float mismatch: {} vs {}",
            record.float_val,
            float_val
        );
        assert_eq!(record.sym, sym_str);
        assert_eq!(record.string_val, string_val);
        assert_eq!(record.vec_int, vec_int);
        assert_eq!(record.vec_float.len(), vec_float.len());
        for (a, b) in record.vec_float.iter().zip(&vec_float) {
            assert!((a - b).abs() < 1e-10, "vec_float element mismatch");
        }

        // ── Serialize and verify K types + values ────────────────────────────
        let krow = record.to_kdb_row();
        assert_eq!(krow.get_type(), qtype::COMPOUND_LIST);
        let fields = krow.as_vec::<K>().unwrap();
        assert_eq!(fields.len(), 8);

        assert_eq!(fields[0].get_type(), qtype::INT_ATOM, "date → INT_ATOM");
        assert_eq!(
            fields[1].get_type(),
            qtype::LONG_ATOM,
            "timestamp → LONG_ATOM"
        );
        assert_eq!(fields[2].get_type(), qtype::INT_ATOM, "int_val → INT_ATOM");
        assert_eq!(
            fields[3].get_type(),
            qtype::FLOAT_ATOM,
            "float_val → FLOAT_ATOM"
        );
        assert_eq!(
            fields[4].get_type(),
            qtype::SYMBOL_ATOM,
            "sym → SYMBOL_ATOM"
        );
        assert_eq!(fields[5].get_type(), qtype::STRING, "string_val → STRING");
        assert_eq!(fields[6].get_type(), qtype::INT_LIST, "vec_int → INT_LIST");
        assert_eq!(
            fields[7].get_type(),
            qtype::FLOAT_LIST,
            "vec_float → FLOAT_LIST"
        );

        assert_eq!(fields[0].get_int().unwrap(), kdb_date);
        assert_eq!(fields[1].get_long().unwrap(), kdb_ts);
        assert_eq!(fields[2].get_int().unwrap(), int_val);
        assert!((fields[3].get_float().unwrap() - float_val).abs() < 1e-10);
        assert_eq!(fields[4].get_symbol().unwrap(), sym_str);
        assert_eq!(fields[5].as_string().unwrap(), string_val);
        assert_eq!(*fields[6].as_vec::<i32>().unwrap(), vec_int);
        let svf = fields[7].as_vec::<f64>().unwrap();
        for (a, b) in svf.iter().zip(&vec_float) {
            assert!((a - b).abs() < 1e-10, "serialized vec_float mismatch");
        }
    }
}
