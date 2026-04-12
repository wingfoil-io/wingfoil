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

        // Serialize every record in the burst (time prepended, business fields appended).
        let serialized: Vec<Vec<K>> = batch
            .into_iter()
            .map(|record| {
                // Timestamp is tracked separately as a string; to_kdb_row() has business fields.
                record
                    .to_kdb_row()
                    .as_vec::<K>()
                    .map(|v| v.to_vec())
                    .unwrap_or_default()
            })
            .collect();

        if serialized.is_empty() {
            continue;
        }

        // Transpose to column-major and format as q string column fragments.
        // Time column is pre-formatted; business columns come from KdbSerialize.
        let n = serialized.len(); // number of rows (1 per burst in typical usage)
        let n_cols = serialized[0].len();
        let mut columns: Vec<Vec<K>> = (0..n_cols).map(|_| Vec::with_capacity(n)).collect();
        for row in serialized {
            for (col_idx, val) in row.into_iter().enumerate() {
                columns[col_idx].push(val);
            }
        }

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

/// Format a column of same-typed K atoms as a q string fragment with correct type suffixes.
///
/// Produces literals like `enlist`AAPL`, `enlist 42.5f`, `enlist 100j`, etc.
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
            if syms.len() == 1 {
                Ok(format!("enlist`{}", syms[0]))
            } else {
                Ok(syms
                    .iter()
                    .map(|s| format!("`{s}"))
                    .collect::<Vec<_>>()
                    .join(""))
            }
        }
        qtype::FLOAT_ATOM => {
            let vals: anyhow::Result<Vec<f64>> = atoms.iter().map(|k| Ok(k.get_float()?)).collect();
            let vals = vals?;
            if vals.len() == 1 {
                Ok(format!("enlist {}f", vals[0]))
            } else {
                let parts: Vec<_> = vals.iter().map(|v| format!("{v}")).collect();
                Ok(format!("{}f", parts.join(" ")))
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
            if vals.len() == 1 {
                Ok(format!("enlist {}e", vals[0]))
            } else {
                let parts: Vec<_> = vals.iter().map(|v| format!("{v}")).collect();
                Ok(format!("{}e", parts.join(" ")))
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
