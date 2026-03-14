//! KDB+ write functionality for streaming data to q/kdb+ instances.

use super::KdbConnection;
use crate::nodes::{FutStream, StreamOperators};
use crate::types::*;
use chrono::NaiveDateTime;
use futures::StreamExt;
use kdb_plus_fixed::ipc::{ConnectionMethod, K, QStream};
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

    let consumer = Box::new(move |source: Pin<Box<dyn FutStream<Burst<T>>>>| {
        kdb_write_consumer(connection, table_name, source)
    });

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

    // Pre-build the reusable insert query components.
    // `insert` must be a symbol, not a string, for the KDB+ functional query form.
    let insert_k = K::new_symbol("insert".to_string());
    let table_k = K::new_symbol(table_name);

    // Process incoming bursts
    while let Some((time, batch)) = source.next().await {
        // Compute timestamp once per burst — all records in a burst share the same time.
        let naive: NaiveDateTime = time.into();
        let kdb_time = K::new_timestamp(naive.and_utc());

        // Serialize every record in the burst (time prepended, business fields appended).
        let serialized: Vec<Vec<K>> = batch
            .into_iter()
            .map(|record| {
                let mut fields = vec![kdb_time.clone()];
                if let Ok(row_fields) = record.to_kdb_row().as_vec::<K>() {
                    fields.extend(row_fields.iter().cloned());
                }
                fields
            })
            .collect();

        if serialized.is_empty() {
            continue;
        }

        // Transpose row-oriented data to column-oriented for a single bulk insert:
        //   (insert; `table; ((t0;t1;...); (sym0;sym1;...); (price0;price1;...)))
        let n_cols = serialized[0].len();
        let mut columns: Vec<Vec<K>> = (0..n_cols)
            .map(|_| Vec::with_capacity(serialized.len()))
            .collect();
        for row in serialized {
            for (col_idx, val) in row.into_iter().enumerate() {
                columns[col_idx].push(val);
            }
        }
        let data = K::new_compound_list(columns.into_iter().map(K::new_compound_list).collect());

        let query = K::new_compound_list(vec![insert_k.clone(), table_k.clone(), data]);
        socket.send_sync_message(&query).await?;
    }
    Ok(())
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
        let mut batch: Burst<TestTrade> = Burst::new();
        batch.push(trade);

        let stream = constant(batch);

        // Verify we can create the kdb_write node (doesn't require actual KDB connection)
        let _node = kdb_write(conn, "test_table", &stream);
        // Node creation succeeds - actual connection would be attempted during run()
    }
}
