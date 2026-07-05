//! PostgreSQL database adapter for time-partitioned reads and streaming writes.
//!
//! Provides three graph nodes:
//! - [`postgres_read`] — producer that replays a historical table in contiguous,
//!   caller-defined time slices (one query per slice), driven by the run's
//!   `RunMode::HistoricalFrom` / `RunFor::Duration` window. Shares its slicing
//!   logic with the KDB+ adapter (`crate::adapters::time_slice`).
//! - [`postgres_sub`] — real-time producer that live-tails a table via
//!   `LISTEN`/`NOTIFY` (notification as wake-up, rows re-queried past a cursor).
//! - [`postgres_write`] — consumer that inserts each on-graph record, prepending
//!   the graph timestamp as the first column.
//!
//! Time is carried **on-graph** in tuples `(NanoTime, T)`, never inside the record
//! struct: on read it is extracted from a timestamp column into the tuple; on write
//! it is prepended to the row. Your struct should hold only business data.
//!
//! # Setup
//!
//! ```sh
//! docker run --rm -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres:16-alpine
//! ```
//!
//! # Reading (time-sliced)
//!
//! [`postgres_read`] calls `query_fn` once per slice with the half-open interval
//! `[t0, t1)`, the KDB-style date integer, and the slice iteration index. Filter on
//! `time >= t0 AND time < t1` and `ORDER BY time` so rows arrive time-ordered.
//!
//! ```ignore
//! use wingfoil::adapters::postgres::*;
//! use wingfoil::*;
//!
//! #[derive(Debug, Clone, Default)]
//! struct Trade { sym: String, price: f64, qty: i64 }
//!
//! impl PostgresDeserialize for Trade {
//!     fn from_row(row: &Row) -> anyhow::Result<(NanoTime, Self)> {
//!         Ok((
//!             row.get_nanotime(0)?, // col 0: time
//!             Trade { sym: row.try_get(1)?, price: row.try_get(2)?, qty: row.try_get(3)? },
//!         ))
//!     }
//! }
//!
//! let conn = PostgresConnection::new("host=localhost user=postgres password=postgres dbname=postgres");
//! postgres_read::<Trade, _>(conn, std::time::Duration::from_secs(3600), |(t0, t1), _date, _| {
//!     format!(
//!         "SELECT time, sym, price, qty FROM trades \
//!          WHERE time >= '{}' AND time < '{}' ORDER BY time",
//!         postgres_timestamp(t0), postgres_timestamp(t1),
//!     )
//! })
//!     .collapse()
//!     .print()
//!     .run(
//!         RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
//!         RunFor::Duration(std::time::Duration::from_secs(86400)),
//!     )
//!     .unwrap();
//! ```
//!
//! # Subscribing (real-time live tail)
//!
//! [`postgres_sub`] streams rows as they are inserted, using `LISTEN`/`NOTIFY` as a
//! wake-up signal and re-querying past a time cursor (so nothing is lost to NOTIFY's
//! payload limits). Install the trigger with [`postgres_notify_trigger_sql`], then:
//!
//! ```ignore
//! use wingfoil::adapters::postgres::*;
//! use wingfoil::*;
//!
//! let conn = PostgresConnection::new("host=localhost user=postgres password=postgres dbname=postgres");
//!
//! // One-time setup (psql or any client): install the notify trigger.
//! //   postgres_notify_trigger_sql("trades", "trades_feed")
//!
//! postgres_sub::<Trade, _>(conn, "trades_feed", NanoTime::now(), |cursor| {
//!     format!(
//!         "SELECT time, sym, price, qty FROM trades \
//!          WHERE time > '{}' ORDER BY time",
//!         postgres_timestamp(cursor),
//!     )
//! })
//!     .collapse()
//!     .print()
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```
//!
//! # Writing
//!
//! [`postgres_write`] (or the fluent `.postgres_write()` method) inserts each record,
//! prepending the graph timestamp as the first column. The target table's columns must
//! be `(time, <business columns in to_params() order>)`.
//!
//! ```ignore
//! use wingfoil::adapters::postgres::*;
//! use wingfoil::*;
//!
//! #[derive(Debug, Clone, Default)]
//! struct Trade { sym: String, price: f64, qty: i64 }
//!
//! impl PostgresSerialize for Trade {
//!     fn to_params(&self) -> Vec<Box<dyn ToSql + Sync + Send>> {
//!         vec![Box::new(self.sym.clone()), Box::new(self.price), Box::new(self.qty)]
//!     }
//! }
//!
//! let conn = PostgresConnection::new("host=localhost user=postgres password=postgres dbname=postgres");
//! constant(burst![Trade { sym: "AAPL".into(), price: 1.0, qty: 1 }])
//!     .postgres_write(conn, "trades")
//!     .run(RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)), RunFor::Cycles(1))
//!     .unwrap();
//! ```

mod read;
mod sub;
mod write;

#[cfg(all(test, feature = "postgres-integration-test"))]
mod integration_tests;

pub use read::*;
pub use sub::*;
pub use write::*;

/// Re-export of [`tokio_postgres::Row`] so callers can implement
/// [`PostgresDeserialize`] without depending on `tokio-postgres` directly.
pub use tokio_postgres::Row;
/// Re-export of [`tokio_postgres::types::ToSql`] for [`PostgresSerialize`] impls.
pub use tokio_postgres::types::ToSql;
/// Re-export of [`tokio_postgres::types::Type`] for dispatching on column SQL types
/// in [`PostgresDeserialize`] impls.
pub use tokio_postgres::types::Type;

/// Quote a PostgreSQL identifier: wrap in double quotes, doubling any embedded quotes.
///
/// Makes mixed-case, reserved-word, and special-character identifiers safe to splice
/// into SQL (`quote_ident("eventTime")` → `"eventTime"`). Note that quoting disables
/// PostgreSQL's lower-case folding, so the identifier must match the column/table name
/// exactly as stored in the catalog.
#[must_use]
pub fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Quote a possibly schema-qualified table name, quoting each dot-separated segment.
///
/// `quote_table("public.My Trades")` → `"public"."My Trades"`.
#[must_use]
pub fn quote_table(table: &str) -> String {
    table
        .split('.')
        .map(quote_ident)
        .collect::<Vec<_>>()
        .join(".")
}

/// PostgreSQL connection configuration.
///
/// Wraps a libpq-style connection string (see the [tokio-postgres config docs]).
///
/// [tokio-postgres config docs]: https://docs.rs/tokio-postgres/latest/tokio_postgres/config/struct.Config.html
#[derive(Debug, Clone)]
pub struct PostgresConnection {
    /// libpq-style connection string, e.g.
    /// `"host=localhost port=5432 user=postgres password=postgres dbname=postgres"`.
    pub conn_str: String,
}

impl PostgresConnection {
    /// Create a connection config from a libpq-style connection string.
    pub fn new(conn_str: impl Into<String>) -> Self {
        Self {
            conn_str: conn_str.into(),
        }
    }
}

impl From<&str> for PostgresConnection {
    fn from(conn_str: &str) -> Self {
        Self::new(conn_str)
    }
}

impl From<String> for PostgresConnection {
    fn from(conn_str: String) -> Self {
        Self::new(conn_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_from_str() {
        let conn: PostgresConnection = "host=localhost dbname=db".into();
        assert_eq!(conn.conn_str, "host=localhost dbname=db");
    }

    #[test]
    fn test_connection_new() {
        let conn = PostgresConnection::new("host=x".to_string());
        assert_eq!(conn.conn_str, "host=x");
    }

    #[test]
    fn test_quote_ident() {
        assert_eq!(quote_ident("time"), "\"time\"");
        assert_eq!(quote_ident("eventTime"), "\"eventTime\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    }

    #[test]
    fn test_quote_table() {
        assert_eq!(quote_table("trades"), "\"trades\"");
        assert_eq!(quote_table("public.My Trades"), "\"public\".\"My Trades\"");
    }
}
