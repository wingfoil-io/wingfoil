//! PostgreSQL read functionality — time-partitioned streaming reads.

use super::PostgresConnection;
use crate::adapters::time_slice::compute_validated_time_slices;
use crate::nodes::produce_async;
use crate::types::*;
use anyhow::Context;
use chrono::NaiveDateTime;
use log::info;
use std::rc::Rc;
use tokio_postgres::{NoTls, Row};

/// Trait for deserializing a PostgreSQL row into `(NanoTime, Self)`.
///
/// The implementor owns time extraction (which column carries the timestamp) and
/// returns the business record separately — time lives on-graph, not in the struct.
/// Use [`PostgresRowExt::get_nanotime`] to read a `timestamp` column as [`NanoTime`],
/// and [`Row::try_get`] for the business columns.
pub trait PostgresDeserialize: Sized {
    /// Deserialize a row into `(NanoTime, Self)`.
    fn from_row(row: &Row) -> anyhow::Result<(NanoTime, Self)>;
}

/// Extension trait for extracting a [`NanoTime`] from a PostgreSQL timestamp column.
///
/// Supports both `timestamp` (read as `chrono::NaiveDateTime`, interpreted as UTC)
/// and `timestamptz` (read as `chrono::DateTime<Utc>`), converted to [`NanoTime`]
/// (nanoseconds since the Unix epoch).
pub trait PostgresRowExt {
    /// Read the `timestamp`/`timestamptz` column at index `idx` as a [`NanoTime`].
    fn get_nanotime(&self, idx: usize) -> anyhow::Result<NanoTime>;
    /// Read the `timestamp`/`timestamptz` column named `name` as a [`NanoTime`].
    fn get_nanotime_named(&self, name: &str) -> anyhow::Result<NanoTime>;
}

impl PostgresRowExt for Row {
    fn get_nanotime(&self, idx: usize) -> anyhow::Result<NanoTime> {
        if let Ok(dt) = self.try_get::<_, NaiveDateTime>(idx) {
            return Ok(dt.into());
        }
        let dt: chrono::DateTime<chrono::Utc> = self
            .try_get(idx)
            .with_context(|| format!("column {idx} is not a `timestamp`/`timestamptz`"))?;
        Ok(dt.naive_utc().into())
    }

    fn get_nanotime_named(&self, name: &str) -> anyhow::Result<NanoTime> {
        if let Ok(dt) = self.try_get::<_, NaiveDateTime>(name) {
            return Ok(dt.into());
        }
        let dt: chrono::DateTime<chrono::Utc> = self
            .try_get(name)
            .with_context(|| format!("column `{name}` is not a `timestamp`/`timestamptz`"))?;
        Ok(dt.naive_utc().into())
    }
}

/// Format a [`NanoTime`] as a PostgreSQL `timestamp` literal (`YYYY-MM-DD HH:MM:SS.ffffff`, UTC).
///
/// Convenience for building the `WHERE time >= '…' AND time < '…'` bounds inside a
/// `postgres_read` query closure. PostgreSQL `timestamp` resolves to microseconds, so
/// the sub-second part is rendered to 6 digits.
#[must_use]
pub fn postgres_timestamp(time: NanoTime) -> String {
    let dt: NaiveDateTime = time.into();
    dt.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

/// Read a time-partitioned PostgreSQL table, one query per time slice.
///
/// The run's `[start, end)` window (from `RunMode::HistoricalFrom` + `RunFor::Duration`)
/// is split into contiguous, half-open slices of length `period`. `query_fn` is called
/// once per slice with `((t0, t1), date, iteration)` and must return a SQL query filtering
/// on `time >= t0 AND time < t1`, ordered by time. Rows are streamed on-graph as
/// `Burst<T>` in time order; a non-monotonic timestamp aborts the run.
///
/// `date` is the KDB-style date integer of the day containing the slice — **days since
/// 2000-01-01** (not the Unix epoch) — matching the KDB+ adapter so date-partitioned
/// query logic ports across. `iteration` is the slice index within that day (resets to
/// 0 each new day). Most queries only need `t0`/`t1` and can ignore both.
///
/// # Requirements
/// - `RunMode::HistoricalFrom` with a non-zero start time.
/// - `RunFor::Duration` (not `Forever`/`Cycles`) so the slice set is bounded.
#[must_use]
pub fn postgres_read<T>(
    connection: impl Into<PostgresConnection>,
    period: std::time::Duration,
    query_fn: impl FnMut((NanoTime, NanoTime), i32, usize) -> String + Send + 'static,
) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + Send + PostgresDeserialize + 'static,
{
    let connection = connection.into();
    produce_async(move |ctx| {
        let start_time = ctx.start_time;
        let end_time_result = ctx.end_time();
        let connection = connection;
        let mut query_fn = query_fn;

        async move {
            let slices = compute_validated_time_slices(
                "postgres_read",
                start_time,
                end_time_result,
                period,
            )?;

            let (client, conn) = tokio_postgres::connect(&connection.conn_str, NoTls)
                .await
                .with_context(|| {
                    format!("postgres_read: failed to connect: {}", connection.conn_str)
                })?;
            // Drive the connection's protocol handling in the background; it ends
            // when `client` is dropped after the stream is exhausted.
            tokio::spawn(async move {
                if let Err(e) = conn.await {
                    log::error!("postgres connection error: {e}");
                }
            });

            Ok(async_stream::stream! {
                // Timestamps are full (not time-of-day), so ordering is enforced
                // across the whole read, not reset per slice.
                let mut prev_time: Option<NanoTime> = None;
                'outer: for (within, date, iteration) in slices {
                    let query = query_fn(within, date, iteration);
                    info!("postgres query: {query}");
                    let fetch_start = std::time::Instant::now();
                    let rows = match client.query(&query, &[]).await {
                        Ok(rows) => rows,
                        Err(e) => {
                            yield Err(anyhow::Error::new(e).context("postgres query failed"));
                            break;
                        }
                    };
                    info!("postgres query: {} rows in {:?}", rows.len(), fetch_start.elapsed());

                    for row in &rows {
                        let (time, record) = match T::from_row(row) {
                            Ok(r) => r,
                            Err(e) => { yield Err(e); break 'outer; }
                        };

                        if let Some(prev) = prev_time
                            && time < prev
                        {
                            yield Err(anyhow::anyhow!(
                                "postgres data is not sorted by time: got {time:?} after {prev:?}. \
                                Add `ORDER BY time` to your query."
                            ));
                            break 'outer;
                        }
                        prev_time = Some(time);

                        yield Ok((time, record));
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
    fn test_postgres_timestamp_format() {
        // 2000-01-01 00:00:00 UTC (KDB epoch). `%.6f` always renders 6 digits.
        let t = NanoTime::from_kdb_timestamp(0);
        assert_eq!(postgres_timestamp(t), "2000-01-01 00:00:00.000000");

        // One second + 500 microseconds later.
        let t = NanoTime::from_kdb_timestamp(1_000_000_500_000);
        assert_eq!(postgres_timestamp(t), "2000-01-01 00:16:40.000500");
    }
}
