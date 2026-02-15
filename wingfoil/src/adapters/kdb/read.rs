//! KDB+ read functionality for streaming data from q/kdb+ instances.

use super::{KdbConnection, Sym, SymbolInterner};
use crate::nodes::produce_async;
use crate::types::*;
use anyhow::{Result, bail};
use kdb_plus_fixed::ipc::error::Error as KdbError;
use kdb_plus_fixed::ipc::{ConnectionMethod, K, QStream};
use kdb_plus_fixed::qtype;
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

/// Read data from a KDB+ database with time-based chunking to handle large result sets.
///
/// This function connects to a KDB+ instance and streams data in chunks, ensuring
/// bounded memory usage regardless of the total result size. It uses time-based
/// pagination to read rows sequentially within the specified time range.
///
/// Start and end times are derived from the graph's [`RunMode`] and [`RunFor`]:
/// - Start time comes from `RunMode::HistoricalFrom(t)` or current time for `RealTime`
/// - End time is `start + duration` for `RunFor::Duration`, or unbounded otherwise
///
/// # Type Parameters
/// * `T` - The record type, must implement `Element`, `Send`, and `KdbDeserialize`
///
/// # Arguments
/// * `connection` - KDB connection configuration
/// * `query` - The base q query to execute (e.g., "select from trades" or "select from trades where sym=`AAPL")
/// * `time_col` - Name of the time column used for chunking (also used to extract time from rows)
/// * `rows_per_chunk` - Maximum number of rows to fetch per query (controls memory usage)
///
/// # Memory Usage
/// Memory usage is bounded by `rows_per_chunk`. For example, with 10,000 rows per chunk:
/// - Small records (~100 bytes): ~1 MB per chunk
/// - Large records (~1 KB): ~10 MB per chunk
///
/// # Returns
/// A stream of records bundled by timestamp using TinyVec.
///
/// # Example
///
/// ```ignore
/// let conn = KdbConnection::new("localhost", 5000);
///
/// // Read trades - time range from RunMode/RunFor
/// let stream = kdb_read(
///     conn,
///     "select from trades",           // Base query
///     "time",                          // Time column name
///     10000                            // 10K rows per chunk
/// );
///
/// // Run with historical mode and duration
/// stream.run(
///     RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
///     RunFor::Duration(Duration::from_secs(86400))  // 24 hours
/// );
/// ```
#[must_use]
pub fn kdb_read<T>(
    connection: KdbConnection,
    query: impl Into<String>,
    time_col: impl Into<String>,
    rows_per_chunk: usize,
) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + Send + KdbDeserialize + 'static,
{
    let query = query.into();
    let time_col = time_col.into();

    produce_async(move |ctx| {
        // Derive start/end times from RunParams
        let start_time = ctx.start_time.to_kdb_timestamp();
        let end_time = ctx.end_time().map(|t| t.to_kdb_timestamp());

        async move {
            let end_time = end_time?;
            let creds = connection.credentials_string();

            // Connect to KDB
            let mut socket = QStream::connect(
                ConnectionMethod::TCP,
                &connection.host,
                connection.port,
                &creds,
            )
            .await?;

            // Stream rows in chunks
            Ok(async_stream::stream! {
                let mut current_time = start_time;
                let mut interner = SymbolInterner::default();

                loop {
                    // Build chunk query with time constraint and row limit
                    let chunk_query = format!(
                        "select[{}] from ({}) where {} >= {}, {} <= {}",
                        rows_per_chunk,
                        query,
                        time_col,
                        current_time,
                        time_col,
                        end_time
                    );

                    let result: K = match socket.send_sync_message(&chunk_query.as_str()).await {
                        Ok(r) => r,
                        Err(e) => {
                            yield Err(anyhow::Error::new(e).context(format!("KDB query failed: {}", chunk_query)));
                            break;
                        }
                    };

                    // Extract metadata once per chunk
                    let (columns, rows) = match (result.column_names(), result.rows()) {
                        (Ok(cols), Ok(rows)) => (cols, rows),
                        (Err(e), _) | (_, Err(e)) => {
                            yield Err(anyhow::anyhow!("{}\nkdb query failed with\n{}", chunk_query, e));
                            break;
                        }
                    };

                    let row_count = rows.len();
                    if row_count == 0 {
                        break;  // No more data
                    }

                    // Find time column index once per chunk
                    let time_col_idx = match columns.iter().position(|c| c == &time_col) {
                        Some(idx) => idx,
                        None => {
                            yield Err(anyhow::anyhow!("time column '{}' not found", time_col));
                            break;
                        }
                    };

                    // Process rows and track last timestamp
                    let mut prev_time: Option<i64> = None;
                    let mut last_time = current_time;
                    let mut row_error = false;
                    for row in &rows {
                        // Extract time from KDB row directly
                        let time_kdb = match row.get(time_col_idx).and_then(|v| v.get_long()) {
                            Ok(t) => t,
                            Err(e) => {
                                yield Err(anyhow::Error::new(e).context("failed to extract time from KDB row"));
                                row_error = true;
                                break;
                            }
                        };

                        // Check for unsorted data
                        if let Some(prev) = prev_time
                            && time_kdb < prev
                        {
                            yield Err(anyhow::anyhow!(
                                "KDB data is not sorted by time column '{}': got {} after {}. \
                                Add `{} xasc` to your query to sort the data.",
                                time_col, time_kdb, prev, time_col
                            ));
                            row_error = true;
                            break;
                        }
                        prev_time = Some(time_kdb);

                        let time = NanoTime::from_kdb_timestamp(time_kdb);

                        // Deserialize record (without time)
                        let record = match T::from_kdb_row(row, &columns, &mut interner) {
                            Ok(r) => r,
                            Err(e) => {
                                yield Err(anyhow::Error::new(e).context("KDB deserialization failed"));
                                row_error = true;
                                break;
                            }
                        };

                        last_time = time_kdb;
                        yield Ok((time, record));
                    }
                    if row_error {
                        break;
                    }

                    // If we got fewer rows than requested, we're done
                    if row_count < rows_per_chunk {
                        break;
                    }

                    // Start next chunk right after the last row
                    current_time = last_time + 1;

                    // Safety check: don't go past end_time
                    if current_time > end_time {
                        break;
                    }
                }
            })
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
}
