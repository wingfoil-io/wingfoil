//! KDB+ read functionality for streaming data from q/kdb+ instances.

use super::{KdbConnection, Sym, SymbolInterner};
use crate::adapters::time_slice::compute_validated_time_slices;
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

    /// Get a KDB timestamp column as [`NanoTime`].
    ///
    /// Reads the column value as an i64 (KDB nanoseconds since 2000-01-01) and
    /// converts it to a [`NanoTime`] (nanoseconds since Unix epoch).
    pub fn get_timestamp(&self, col: usize) -> Result<NanoTime, KdbError> {
        Ok(NanoTime::from_kdb_timestamp(self.get(col)?.get_long()?))
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
/// Implementors extract fields from the row using indexed column access and return
/// a `(NanoTime, Self)` tuple — the time is owned by the implementation rather than
/// the adapter, giving full control over which column carries the timestamp.
///
/// Use [`Row::get_timestamp`] to extract a KDB timestamp column as [`NanoTime`].
pub trait KdbDeserialize: Sized {
    /// Deserialize a KDB row into `(NanoTime, Self)`.
    ///
    /// # Arguments
    /// * `row` - Row accessor providing indexed access to column values
    /// * `columns` - Column names from the table schema
    /// * `interner` - Symbol interner for deduplicating symbol strings via [`Row::get_sym`]
    fn from_kdb_row(
        row: Row<'_>,
        columns: &[String],
        interner: &mut SymbolInterner,
    ) -> Result<(NanoTime, Self), KdbError>;
}

/// Core streaming loop driven by a caller-supplied query closure.
///
/// Calls `query_fn()` before each chunk. Returns `None` to stop, or
/// `Some(query_string)` to execute next.
///
/// `prev_time` is reset each chunk so time-of-day columns work correctly when
/// advancing across date partitions (timestamps restart at midnight on each new date).
fn chunk_stream<T>(
    mut socket: QStream,
    mut query_fn: impl FnMut() -> Option<String> + Send + 'static,
) -> impl futures::Stream<Item = anyhow::Result<(NanoTime, T)>> + Send + 'static
where
    T: KdbDeserialize + Send + 'static,
{
    async_stream::stream! {
        let mut interner = SymbolInterner::default();

        'outer: while let Some(query) = query_fn() {
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
            for row in &rows {
                let (time, record) = match T::from_kdb_row(row, &columns, &mut interner) {
                    Ok(r) => r,
                    Err(e) => { yield Err(e.into()); break 'outer; }
                };

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
        }
    }
}

#[must_use]
pub fn kdb_read<T, F>(
    connection: KdbConnection,
    period: std::time::Duration,
    query_fn: F,
) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + Send + KdbDeserialize + 'static,
    F: FnMut((NanoTime, NanoTime), i32, usize) -> String + Send + 'static,
{
    produce_async(move |ctx| {
        let start_time = ctx.start_time;
        let end_time_result = ctx.end_time();

        async move {
            let slices =
                compute_validated_time_slices("kdb_read", start_time, end_time_result, period)?;

            let creds = connection.credentials_string();
            let socket = QStream::connect(
                ConnectionMethod::TCP,
                &connection.host,
                connection.port,
                &creds,
            )
            .await?;

            let mut slices_iter = slices.into_iter();
            let mut query_fn = query_fn;
            let slice_fn = move || -> Option<String> {
                let (within, date, iteration) = slices_iter.next()?;
                Some(query_fn(within, date, iteration))
            };

            Ok(chunk_stream::<T>(socket, slice_fn))
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
            ) -> Result<(NanoTime, Self), KdbError> {
                let time = row.get_timestamp(1)?;
                Ok((
                    time,
                    AllTypesRecord {
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
        let (_, record) =
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
