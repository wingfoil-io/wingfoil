//! PostgreSQL write functionality — streaming inserts of on-graph records.

use super::PostgresConnection;
use crate::nodes::{FutStream, RunParams, StreamOperators};
use crate::types::*;
use anyhow::Context;
use chrono::NaiveDateTime;
use futures::StreamExt;
use std::pin::Pin;
use std::rc::Rc;
use tokio_postgres::NoTls;
use tokio_postgres::types::ToSql;

/// Trait for serializing a Rust record into PostgreSQL column values.
///
/// Return the **business** column values only, in the same order they appear in the
/// target table *after* the time column. The graph timestamp is prepended
/// automatically by [`postgres_write`] as the first inserted column — do not include it.
///
/// Each value is returned as an owned `Box<dyn ToSql>`, so this allocates once per
/// column per row. That is a deliberate trade for an open type set (any `ToSql` type
/// binds) and an object-safe method; it is a non-issue at the low-frequency write rates
/// this adapter targets, where the network round-trip dwarfs the allocation. Do not
/// reach for a zero-alloc borrowing design unless a hot write path proves it necessary.
pub trait PostgresSerialize {
    /// Owned, boxed column values (excluding time) in table column order.
    fn to_params(&self) -> Vec<Box<dyn ToSql + Sync + Send>>;
}

/// Insert an on-graph `Burst<T>` stream into a PostgreSQL table.
///
/// Each record is inserted as one row `(time, <to_params()…>)`, with the graph
/// timestamp prepended as the first column. The target table's columns must line up
/// positionally: `(timestamp, <business columns in to_params() order>)`.
///
/// # Type Parameters
/// * `T` — record type; must implement [`Element`], `Send`, and [`PostgresSerialize`].
#[must_use]
pub fn postgres_write<T>(
    connection: impl Into<PostgresConnection>,
    table_name: impl Into<String>,
    upstream: &Rc<dyn Stream<Burst<T>>>,
) -> Rc<dyn Node>
where
    T: Element + Send + PostgresSerialize + 'static,
{
    let connection = connection.into();
    let table_name = table_name.into();

    let consumer = Box::new(
        move |_ctx: RunParams, source: Pin<Box<dyn FutStream<Burst<T>>>>| {
            postgres_write_consumer(connection, table_name, source)
        },
    );

    upstream.consume_async(consumer)
}

async fn postgres_write_consumer<T>(
    connection: PostgresConnection,
    table_name: String,
    mut source: Pin<Box<dyn FutStream<Burst<T>>>>,
) -> anyhow::Result<()>
where
    T: Element + Send + PostgresSerialize + 'static,
{
    let (client, conn) = tokio_postgres::connect(&connection.conn_str, NoTls)
        .await
        .with_context(|| {
            format!(
                "postgres_write: failed to connect: {}",
                connection.redacted()
            )
        })?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            log::error!("postgres connection error: {e}");
        }
    });

    // Identifier-quoted (per dot-separated segment) so mixed-case and
    // reserved-word table names work; also closes the string-interpolation hole.
    let table_sql = super::quote_table(&table_name);

    // Prepared once the column count is known (from the first record).
    let mut prepared: Option<tokio_postgres::Statement> = None;

    while let Some((time, batch)) = source.next().await {
        if batch.is_empty() {
            continue;
        }
        // All records in a burst share the graph timestamp.
        let ts: NaiveDateTime = time.into();

        // Serialize the whole burst up front so the inserts below can borrow.
        let rows: Vec<Vec<Box<dyn ToSql + Sync + Send>>> =
            batch.iter().map(|record| record.to_params()).collect();
        let n = rows[0].len() + 1; // + time column

        let stmt = match &prepared {
            Some(s) => s,
            None => {
                let placeholders = (1..=n)
                    .map(|i| format!("${i}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!("INSERT INTO {table_sql} VALUES ({placeholders})");
                let s = client
                    .prepare(&sql)
                    .await
                    .with_context(|| format!("postgres_write: failed to prepare `{sql}`"))?;
                prepared.insert(s)
            }
        };

        // Launch every insert in the burst concurrently: tokio-postgres pipelines
        // in-flight extended-protocol requests over the single connection, so an
        // N-record burst costs ~1 round trip instead of N sequential ones.
        let client = &client;
        let inserts = rows.iter().map(|values| {
            let mut params: Vec<&(dyn ToSql + Sync)> = Vec::with_capacity(n);
            params.push(&ts);
            for value in values {
                params.push(value.as_ref());
            }
            async move { client.execute(stmt, &params).await }
        });
        futures::future::try_join_all(inserts)
            .await
            .with_context(|| format!("postgres_write: insert into `{table_name}` failed"))?;
    }

    Ok(())
}

/// Fluent extension for writing `Burst<T>` streams to a PostgreSQL table.
pub trait PostgresWriteOperators<T: Element> {
    /// Write this stream to a PostgreSQL table (time prepended as the first column).
    #[must_use]
    fn postgres_write(
        self: &Rc<Self>,
        conn: impl Into<PostgresConnection>,
        table: &str,
    ) -> Rc<dyn Node>;
}

impl<T: Element + Send + PostgresSerialize + 'static> PostgresWriteOperators<T>
    for dyn Stream<Burst<T>>
{
    fn postgres_write(
        self: &Rc<Self>,
        conn: impl Into<PostgresConnection>,
        table: &str,
    ) -> Rc<dyn Node> {
        postgres_write(conn, table, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::burst;
    use crate::nodes::constant;

    #[derive(Debug, Clone, Default)]
    struct TestTrade {
        sym: String,
        price: f64,
        qty: i64,
    }

    impl PostgresSerialize for TestTrade {
        fn to_params(&self) -> Vec<Box<dyn ToSql + Sync + Send>> {
            vec![
                Box::new(self.sym.clone()),
                Box::new(self.price),
                Box::new(self.qty),
            ]
        }
    }

    #[test]
    fn test_postgres_write_node_creation() {
        // Node creation must not require a live connection.
        let stream = constant(burst![TestTrade {
            sym: "TEST".to_string(),
            price: 100.0,
            qty: 1,
        }]);
        let _node = postgres_write("host=localhost dbname=db", "trades", &stream);
    }

    #[test]
    fn test_to_params_len() {
        let trade = TestTrade {
            sym: "AAPL".into(),
            price: 1.0,
            qty: 2,
        };
        assert_eq!(trade.to_params().len(), 3);
    }
}
