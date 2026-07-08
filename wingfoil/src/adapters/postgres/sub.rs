//! PostgreSQL live-tail subscription via `LISTEN`/`NOTIFY`.

use super::{PostgresConnection, PostgresDeserialize, quote_ident, quote_table};
use crate::RunMode;
use crate::nodes::produce_async;
use crate::types::*;
use anyhow::Context;
use futures::StreamExt;
use futures::channel::mpsc;
use log::info;
use std::rc::Rc;
use tokio_postgres::{AsyncMessage, NoTls};

/// SQL that installs an `AFTER INSERT` trigger on `table` which fires
/// `pg_notify('<channel>', '')` once per insert statement.
///
/// Run this (e.g. via `psql` or any client) to wire a table up for
/// [`postgres_sub`]. The payload is intentionally empty — notifications are a
/// wake-up signal only; rows are always fetched by re-querying (see
/// [`postgres_sub`]), so nothing is lost to `NOTIFY`'s payload limits.
///
/// The statements are idempotent (`CREATE OR REPLACE` / `DROP TRIGGER IF EXISTS`).
#[must_use]
pub fn postgres_notify_trigger_sql(table: &str, channel: &str) -> String {
    let table_sql = quote_table(table);
    let fn_ident = quote_ident(&format!("{channel}_notify_fn"));
    let trg_ident = quote_ident(&format!("{channel}_notify_trg"));
    // Channel appears as a string literal inside pg_notify: escape single quotes.
    let chan_literal = channel.replace('\'', "''");
    format!(
        "CREATE OR REPLACE FUNCTION {fn_ident}() RETURNS trigger LANGUAGE plpgsql AS $$\n\
         BEGIN\n\
         \x20 PERFORM pg_notify('{chan_literal}', '');\n\
         \x20 RETURN NULL;\n\
         END $$;\n\
         DROP TRIGGER IF EXISTS {trg_ident} ON {table_sql};\n\
         CREATE TRIGGER {trg_ident} AFTER INSERT ON {table_sql}\n\
         FOR EACH STATEMENT EXECUTE FUNCTION {fn_ident}();"
    )
}

/// Live-tail a PostgreSQL table in real time using `LISTEN`/`NOTIFY`.
///
/// Complements [`postgres_read`](super::postgres_read) (bounded historical replay)
/// with an unbounded real-time source: rows inserted while the graph runs are
/// streamed on-graph as they commit.
///
/// # Pattern
///
/// Notifications are a **wake-up signal only** — the payload is ignored. The adapter:
///
/// 1. Opens a connection and issues `LISTEN <channel>` **before** the first query,
///    so an insert committed during startup is never missed.
/// 2. Runs `query_fn(cursor)` to catch up on everything past `start_from`
///    (`cursor` starts there), advancing `cursor` to the max row time seen.
/// 3. Sleeps until a notification arrives, coalesces any others that queued up,
///    and re-queries past the cursor. Repeat.
///
/// This is robust to `NOTIFY`'s lossiness (payload size caps, drops on reconnect):
/// rows always come from a query, decoded by the same [`PostgresDeserialize`]
/// impl `postgres_read` uses. Wire the table up with the SQL from
/// [`postgres_notify_trigger_sql`].
///
/// `query_fn` receives the cursor and must return SQL selecting rows with
/// `time > cursor`, ordered by time, e.g.
/// `format!("SELECT time, … FROM trades WHERE time > '{}' ORDER BY time", postgres_timestamp(cursor))`.
///
/// # Requirements
/// - `RunMode::RealTime` (use `postgres_read` for historical replay).
/// - The time column must be **strictly increasing** across inserts. The cursor
///   advances to the max row time seen and the next query filters `time > cursor`,
///   so a row inserted at a timestamp **equal to** (or before) the current cursor
///   is silently dropped. A `now()` default is only safe with a single writer and
///   sub-microsecond-distinct inserts; if two rows can share a timestamp, add a
///   tie-breaking column and widen the cursor logic, or rows will be lost at the
///   boundary.
#[must_use]
pub fn postgres_sub<T, F>(
    connection: impl Into<PostgresConnection>,
    channel: impl Into<String>,
    start_from: NanoTime,
    query_fn: F,
) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + Send + PostgresDeserialize + 'static,
    F: FnMut(NanoTime) -> String + Send + 'static,
{
    let connection = connection.into();
    let channel = channel.into();
    produce_async(move |ctx| {
        let run_mode = ctx.run_mode;
        let connection = connection;
        let channel = channel;
        let mut query_fn = query_fn;

        async move {
            if !matches!(run_mode, RunMode::RealTime) {
                anyhow::bail!(
                    "postgres_sub requires RunMode::RealTime; \
                    use postgres_read for historical replay"
                );
            }

            let (client, mut conn) = tokio_postgres::connect(&connection.conn_str, NoTls)
                .await
                .with_context(|| {
                    format!("postgres_sub: failed to connect: {}", connection.conn_str)
                })?;

            // Drive the connection and forward notification wake-ups. Polling
            // `poll_message` (rather than awaiting the plain connection future)
            // is what surfaces `AsyncMessage::Notification`s.
            let (tx, mut rx) = mpsc::unbounded::<()>();
            tokio::spawn(async move {
                let mut messages = futures::stream::poll_fn(move |cx| conn.poll_message(cx));
                while let Some(message) = messages.next().await {
                    match message {
                        Ok(AsyncMessage::Notification(_)) => {
                            if tx.unbounded_send(()).is_err() {
                                break; // subscriber gone
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            log::error!("postgres_sub connection error: {e}");
                            break;
                        }
                    }
                }
            });

            // LISTEN before the catch-up query so an insert committed in the
            // handoff window still produces a wake-up (watch-before-get).
            client
                .batch_execute(&format!("LISTEN {}", quote_ident(&channel)))
                .await
                .with_context(|| format!("postgres_sub: LISTEN on channel `{channel}` failed"))?;

            Ok(async_stream::stream! {
                let mut cursor = start_from;
                loop {
                    // Drain everything past the cursor. The first pass is the
                    // catch-up from `start_from`; later passes fetch what the
                    // wake-up announced.
                    let query = query_fn(cursor);
                    info!("postgres_sub query: {query}");
                    let rows = match client.query(&query, &[]).await {
                        Ok(rows) => rows,
                        Err(e) => {
                            yield Err(anyhow::Error::new(e).context("postgres_sub query failed"));
                            break;
                        }
                    };
                    if !rows.is_empty() {
                        info!("postgres_sub: {} new rows", rows.len());
                    }
                    for row in &rows {
                        let (time, record) = match T::from_row(row) {
                            Ok(r) => r,
                            Err(e) => { yield Err(e); return; }
                        };
                        if time > cursor {
                            cursor = time;
                        }
                        yield Ok((time, record));
                    }

                    // Wait for the next wake-up; coalesce any that queued while
                    // we were querying (they all mean the same thing: re-query).
                    match rx.next().await {
                        Some(()) => {
                            while rx.try_recv().is_ok() {}
                        }
                        None => {
                            yield Err(anyhow::anyhow!(
                                "postgres_sub: connection to postgres closed"
                            ));
                            break;
                        }
                    }
                }
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::{NodeOperators, StreamOperators};
    use crate::{RunFor, RunMode};

    #[derive(Debug, Clone, Default)]
    struct TestRow;

    impl PostgresDeserialize for TestRow {
        fn from_row(_row: &tokio_postgres::Row) -> anyhow::Result<(NanoTime, Self)> {
            Ok((NanoTime::ZERO, TestRow))
        }
    }

    #[test]
    fn test_sub_rejects_historical_mode() {
        // Bails before any connection is attempted.
        let result = postgres_sub::<TestRow, _>(
            "host=127.0.0.1 port=1 user=postgres dbname=postgres connect_timeout=1",
            "chan",
            NanoTime::ZERO,
            |_| String::new(),
        )
        .collapse()
        .collect()
        .run(
            RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
            RunFor::Cycles(1),
        );
        let err = result.expect_err("historical mode must be rejected");
        assert!(
            format!("{err:#}").contains("requires RunMode::RealTime"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn test_notify_trigger_sql_shape() {
        let sql = postgres_notify_trigger_sql("public.trades", "my_chan");
        assert!(sql.contains("pg_notify('my_chan', '')"));
        assert!(sql.contains("ON \"public\".\"trades\""));
        assert!(sql.contains("CREATE TRIGGER \"my_chan_notify_trg\""));
        assert!(sql.contains("FOR EACH STATEMENT"));

        // Single quotes in the channel are escaped in the literal.
        let sql = postgres_notify_trigger_sql("t", "we'ird");
        assert!(sql.contains("pg_notify('we''ird', '')"));
    }
}
