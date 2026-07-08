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
/// The first slice begins at the period boundary at or before `start_time`, so when
/// `start_time` is not period-aligned a `time >= t0` filter can return rows earlier
/// than `start_time` (and the final slice's `t1` can reach past `end_time`). Rows
/// outside the run's `[start_time, end_time)` window are **dropped** with a per-slice
/// warning rather than emitted — emitting a row before the graph clock would abort the
/// run. The query still uses the period-aligned `(t0, t1)` for clean boundaries.
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
            // `compute_validated_time_slices` consumes `end_time_result`; capture the
            // concrete bound first (NanoTime is Copy) so emitted rows can be clamped to
            // the run's `[start_time, end_time)` window below. Validation guarantees the
            // value is present (bounded, non-MAX) by the time we unwrap it.
            let end_time_bound = end_time_result.as_ref().ok().copied();
            let slices = compute_validated_time_slices(
                "postgres_read",
                start_time,
                end_time_result,
                period,
            )?;
            let end_time =
                end_time_bound.expect("compute_validated_time_slices accepted a bounded end_time");

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
                'outer: for ((t0, t1), date, iteration) in slices {
                    let query = query_fn((t0, t1), date, iteration);
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

                    // The query uses the period-aligned (t0, t1) for clean round-number
                    // boundaries, but the first slice's t0 can precede start_time (and the
                    // last slice's t1 exceed end_time), so a `time >= t0` filter may return
                    // rows outside the run window. Clamp to [start_time, end_time) and drop
                    // the rest: emitting a row before the graph clock aborts the run, and a
                    // row past end_time would drive the monotonic check to reject a later slice.
                    let lo = t0.max(start_time);
                    let hi = t1.min(end_time);
                    let mut dropped: usize = 0;

                    for row in &rows {
                        let (time, record) = match T::from_row(row) {
                            Ok(r) => r,
                            Err(e) => { yield Err(e); break 'outer; }
                        };

                        if time < lo || time >= hi {
                            dropped += 1;
                            continue;
                        }

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

                    if dropped > 0 {
                        log::warn!(
                            "postgres_read: dropped {dropped} row(s) outside the requested \
                            window [{lo:?}, {hi:?}); the query returned data beyond the slice \
                            it was asked to fill"
                        );
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
