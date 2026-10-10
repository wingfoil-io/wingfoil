//! postgres adapter — time-partitioned historical **reads** ([`postgres_read`]),
//! a real-time `LISTEN`/`NOTIFY` live-tail **source** ([`postgres_sub`]), and a
//! streaming **sink** ([`PostgresSinkOps::postgres_write`]) for PostgreSQL,
//! using the async `tokio-postgres` client. It ports the legacy
//! `wingfoil::adapters::postgres` module onto the Op model.
//!
//! Time is carried **on-graph** in tuples `(NanoTime, T)`, never inside the
//! record struct: on read it is extracted from a timestamp column into the
//! tuple; on write it is prepended as the first inserted column. Your struct
//! holds only business data.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`statistics`](crate::adapters::statistics)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude). Bring in
//! what you need explicitly:
//!
//! - **Sources** — the free builder functions [`postgres_read`] (bounded
//!   historical replay, one query per time slice) and [`postgres_sub`] (a
//!   realtime live tail) on a [`GraphBuilder`], emitting `Stream<Burst<T>>`; or
//!   [`postgres_source`] — a single **mode-agnostic** source that dispatches to
//!   the right one at `run()` based on the run's [`RunMode`], so the graph need
//!   not choose real-time vs historical at wiring time.
//! - **Sink** — the [`PostgresSinkOps`] extension trait on `Stream<Burst<T>>`,
//!   enabled with `use wingfoil::adapters::postgres::PostgresSinkOps;`.
//!
//! # Reading (time-sliced historical replay)
//!
//! [`postgres_read`] splits the run's `[start, end)` window (from
//! `RunMode::HistoricalFrom` + `RunFor::Duration`) into contiguous, half-open,
//! midnight-aligned slices of length `period` via the shared
//! [`compute_validated_time_slices`](crate::adapters::common) slicer (the same
//! routine the KDB+ reader will reuse), and calls `query_fn` once per slice with
//! `((t0, t1), date, iteration)`. Each query must filter on `time >= t0 AND time
//! < t1` and `ORDER BY time`. The window is validated + sliced at **wiring** (a
//! pure check, no I/O); the connect and slice queries then run at the **start of
//! the run** via [`produce_async`](crate::async_source::produce_async), which
//! replays the decoded, in-window rows at their timestamps — deterministic, and
//! with no network I/O at graph construction.
//!
//! The first slice begins at the period boundary at or before `start_time`, so
//! when `start_time` is not period-aligned a `time >= t0` filter can return rows
//! earlier than `start_time` (and the final slice's `t1` can reach past
//! `end_time`). Rows outside the run's `[start_time, end_time)` window are
//! **dropped** with a per-slice warning via the shared
//! [`WindowFilter`](crate::adapters::common) rather than emitted — delivering a
//! row before the graph clock would abort the run. `date` is the KDB-style date
//! integer (**days since 2000-01-01**, not the Unix epoch), matching the KDB+
//! adapter; `iteration` is the slice index within that day. A non-monotonic
//! timestamp aborts the run (add `ORDER BY time`).
//!
//! # Subscribing (real-time live tail)
//!
//! [`postgres_sub`] streams rows as they are inserted, using `LISTEN`/`NOTIFY`
//! as a **wake-up signal only** (the payload is ignored) and re-querying past a
//! time cursor, so nothing is lost to `NOTIFY`'s payload limits. `LISTEN` is
//! issued **before** the catch-up query (watch-before-get, as `etcd_sub`), so an
//! insert committed during startup is not missed. Install the trigger with
//! [`postgres_notify_trigger_sql`]. It is a live, unbounded, wall-clock-stamped
//! stream with no historical timeline to replay, so it **rejects
//! `RunMode::HistoricalFrom` at wiring time** (use [`postgres_read`] for
//! historical replay) and runs under [`RunMode::RealTime`] only.
//!
//! # One source, either mode ([`postgres_source`])
//!
//! `RunMode` is a *run-time* choice — build the graph once, pick real-time vs
//! historical at `run()`. But `postgres_read`/`postgres_sub` are mode-locked,
//! forcing that choice into wiring (and duplicating downstream wiring across two
//! source functions). [`postgres_source`] closes that gap: it takes a
//! [`PostgresSourceConfig`] carrying an optional historical half and an optional
//! live half, and dispatches on the run's [`RunMode`] at wiring —
//! `HistoricalFrom` → [`postgres_read`], `RealTime` → [`postgres_sub`]. Supply
//! both halves and the graph is fully mode-agnostic (flip the mode at `run()`,
//! wiring unchanged); supply one and the other mode errors at wiring naming the
//! missing half. The two primitives stay public for callers that only ever need
//! one mode. See the deviation register's "B2 — agreed plan".
//!
//! # Sink
//!
//! [`PostgresSinkOps::postgres_write`] connects lazily on the first write (on
//! the consumer task, so a connection error surfaces during the run, not at
//! wiring) and inserts each record as one row
//! `(time, <to_params()…>)`, the graph timestamp prepended as the first column.
//! Writes are driven off the graph thread with
//! [`consume_async`](crate::async_source::consume_async); within a burst all
//! inserts are pipelined over the single connection (~1 round trip per burst).
//!
//! # Deviations from legacy
//!
//! Every legacy *capability* (time-sliced read, live tail, streaming write, the
//! `PostgresDeserialize`/`PostgresSerialize`/`PostgresRowExt` traits, the quoting
//! and timestamp helpers, and the password-redacting connection config) is
//! preserved. The surface differs in four deliberate ways, mirroring the
//! [`etcd`](crate::adapters::etcd) / [`redis`](crate::adapters::redis) ports:
//!
//! 1. **The graph owns the tokio runtime.** `postgres_read` / `postgres_sub` /
//!    `postgres_write` no longer take a `&Handle`: the `GraphBuilder` owns one
//!    runtime, created lazily on first async use and dropped at teardown, shared
//!    by every async adapter — replacing legacy's hidden never-dropped global
//!    (see `docs/decisions/runtime-ownership.md`; embed in your own runtime with
//!    [`GraphBuilder::with_async_runtime`](crate::fluent::GraphBuilder::with_async_runtime)).
//!    [`postgres_read`] takes a [`RunParams`] (it needs the run's `[start, end)`
//!    window to slice queries at wiring — the slicing is pure; the queries
//!    themselves run at the start of the run); the live [`postgres_sub`] takes
//!    only a [`RunMode`] (to reject a historical run at wiring — its producer
//!    derives its `RunParams` from the actual run at start). The **sink** drives
//!    its client with [`Handle::block_on`](tokio::runtime::Handle::block_on) at
//!    teardown, so the graph must be built, run, and dropped from a **non-async
//!    thread** (`main`, a `#[test]` fn).
//! 2. **The reader defers its connect + queries to the run, like legacy.** Both
//!    wingfoil and legacy run `postgres_read` through
//!    [`produce_async`](crate::async_source::produce_async), so wiring does no
//!    I/O and a connection or slice-query error aborts the *run*, not graph
//!    construction (as does a decode or non-monotonic-time error). The run window
//!    is still validated + sliced at wiring (a pure, fail-fast check). Slices are
//!    queried **lazily, one at a time** (an `async_stream` generator), so with a
//!    `buffer_size` bound the replay stays bounded in memory and pipelines query
//!    I/O with graph compute — legacy's model, not an up-front collection.
//!    [`postgres_read`] takes a `buffer_size` for that back-pressure (`None` =
//!    unbounded); [`postgres_source`] threads one through its
//!    [`historical`](PostgresSourceConfig::historical) half.
//! 3. **The sink is a trait only and pipelines per burst via `consume_async`.**
//!    Legacy exposed a free `postgres_write` fn *and* a `PostgresWriteOperators`
//!    trait; wingfoil folds the entry point into [`PostgresSinkOps`]. Like legacy,
//!    the connection is opened **lazily inside the consumer** on the first write
//!    (so wiring opens no socket and a connect failure surfaces during the run),
//!    and it rides the shared `consume_async` (which surfaces a write error on a
//!    subsequent cycle — postgres has no per-write conditional that must abort
//!    synchronously, so the off-thread sink fits).
//! 4. **The sink takes a `buffer_size`.** Exposed for the `consume_async`
//!    back-pressure bound (`None` = unbounded), as the redis sinks do.
//!
//! # Setup
//!
//! ```sh
//! docker run --rm -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres:16-alpine
//! ```

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::NaiveDateTime;
use futures::StreamExt;
use futures::channel::mpsc;
use log::info;
use tokio_postgres::{AsyncMessage, NoTls, Statement};
use wingfoil::{NanoTime, RunFor, RunMode};

use crate::adapters::common::{TimeWindow, WindowFilter, compute_validated_time_slices};
use crate::async_source::{RunParams, consume_async, produce_async};
use crate::fluent::{GraphBuilder, Stream, StreamOps};
use crate::{Burst, burst};

/// Re-export of [`tokio_postgres::Row`] so callers can implement
/// [`PostgresDeserialize`] without depending on `tokio-postgres` directly.
pub use tokio_postgres::Row;
/// Re-exports of [`tokio_postgres::types::ToSql`] (for [`PostgresSerialize`]
/// impls) and [`tokio_postgres::types::Type`] (for dispatching on a column's SQL
/// type in [`PostgresDeserialize`] impls).
pub use tokio_postgres::types::{ToSql, Type};

// ---------------------------------------------------------------------------
// Identifier quoting
// ---------------------------------------------------------------------------

/// Quote a PostgreSQL identifier: wrap in double quotes, doubling any embedded quotes.
///
/// Makes mixed-case, reserved-word, and special-character identifiers safe to
/// splice into SQL (`quote_ident("eventTime")` → `"eventTime"`). Note that
/// quoting disables PostgreSQL's lower-case folding, so the identifier must match
/// the column/table name exactly as stored in the catalog.
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

// ---------------------------------------------------------------------------
// Connection config
// ---------------------------------------------------------------------------

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

    /// Render the connection string with the `password` value masked, for safe
    /// inclusion in error messages and logs.
    ///
    /// libpq connection strings are space-separated `key=value` pairs, so the
    /// `password=...` token is replaced with `password=***` while every other
    /// field is preserved. A value has no way to contain an unescaped space, so
    /// splitting on whitespace is sufficient. Used at every `connect()` error
    /// site so the DSN's `password=...` never reaches a log or an aborted-run
    /// error (parity with legacy PR #433).
    #[must_use]
    pub fn redacted(&self) -> String {
        self.conn_str
            .split_whitespace()
            .map(|kv| {
                if kv
                    .split_once('=')
                    .is_some_and(|(k, _)| k.eq_ignore_ascii_case("password"))
                {
                    "password=***"
                } else {
                    kv
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
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

impl From<&String> for PostgresConnection {
    fn from(conn_str: &String) -> Self {
        Self::new(conn_str.clone())
    }
}

// ---------------------------------------------------------------------------
// Timestamp / row helpers
// ---------------------------------------------------------------------------

/// Format a [`NanoTime`] as a PostgreSQL `timestamp` literal
/// (`YYYY-MM-DD HH:MM:SS.ffffff`, UTC).
///
/// Convenience for building the `WHERE time >= '…' AND time < '…'` bounds inside
/// a [`postgres_read`] query closure. PostgreSQL `timestamp` resolves to
/// microseconds, so the sub-second part is rendered to 6 digits.
#[must_use]
pub fn postgres_timestamp(time: NanoTime) -> String {
    let dt: NaiveDateTime = time.into();
    dt.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

/// Trait for deserializing a PostgreSQL row into `(NanoTime, Self)`.
///
/// The implementor owns time extraction (which column carries the timestamp) and
/// returns the business record separately — time lives on-graph, not in the
/// struct. Use [`PostgresRowExt::get_nanotime`] to read a `timestamp` column as
/// [`NanoTime`], and [`Row::try_get`] for the business columns.
pub trait PostgresDeserialize: Sized {
    /// Deserialize a row into `(NanoTime, Self)`.
    fn from_row(row: &Row) -> Result<(NanoTime, Self)>;
}

/// Extension trait for extracting a [`NanoTime`] from a PostgreSQL timestamp column.
///
/// Supports both `timestamp` (read as `chrono::NaiveDateTime`, interpreted as
/// UTC) and `timestamptz` (read as `chrono::DateTime<Utc>`), converted to
/// [`NanoTime`] (nanoseconds since the Unix epoch).
pub trait PostgresRowExt {
    /// Read the `timestamp`/`timestamptz` column at index `idx` as a [`NanoTime`].
    fn get_nanotime(&self, idx: usize) -> Result<NanoTime>;
    /// Read the `timestamp`/`timestamptz` column named `name` as a [`NanoTime`].
    fn get_nanotime_named(&self, name: &str) -> Result<NanoTime>;
}

impl PostgresRowExt for Row {
    fn get_nanotime(&self, idx: usize) -> Result<NanoTime> {
        if let Ok(dt) = self.try_get::<_, NaiveDateTime>(idx) {
            return Ok(dt.into());
        }
        let dt: chrono::DateTime<chrono::Utc> = self
            .try_get(idx)
            .with_context(|| format!("column {idx} is not a `timestamp`/`timestamptz`"))?;
        Ok(dt.naive_utc().into())
    }

    fn get_nanotime_named(&self, name: &str) -> Result<NanoTime> {
        if let Ok(dt) = self.try_get::<_, NaiveDateTime>(name) {
            return Ok(dt.into());
        }
        let dt: chrono::DateTime<chrono::Utc> = self
            .try_get(name)
            .with_context(|| format!("column `{name}` is not a `timestamp`/`timestamptz`"))?;
        Ok(dt.naive_utc().into())
    }
}

// ---------------------------------------------------------------------------
// Source: time-sliced historical read
// ---------------------------------------------------------------------------

/// Read a time-partitioned PostgreSQL table, one query per time slice, as a
/// deterministic historical replay `Burst<T>` source.
///
/// The run's `[start, end)` window (from `RunMode::HistoricalFrom` +
/// `RunFor::Duration`, taken from `params`) is split into contiguous, half-open,
/// midnight-aligned slices of length `period`. `query_fn` is called once per
/// slice with `((t0, t1), date, iteration)` and must return a SQL query filtering
/// on `time >= t0 AND time < t1`, ordered by time. The window is validated +
/// sliced at wiring (a pure check); the connect and the per-slice queries then
/// run at the **start of the run** via
/// [`produce_async`](crate::async_source::produce_async), which
/// replays the decoded, in-window rows at their timestamps.
///
/// `buffer_size` bounds the producer→graph backlog as back-pressure (like
/// legacy): `Some(n)` caps the replay to ~`n` timestamp-groups of look-ahead, so
/// a slice is fetched only as the graph drains — bounded memory, pipelined query
/// I/O; `None` is unbounded.
///
/// See the module docs for the window-clamp / `date` / `iteration` semantics.
/// Run the graph with the same `RunMode::HistoricalFrom` / `RunFor` described by
/// `params`.
///
/// # Errors
///
/// Returns an error at **wiring time** only if `params` does not describe a
/// bounded historical run (`RunMode::RealTime`, a zero start, `RunFor::Forever`,
/// or `RunFor::Cycles`) — the slice set would be unbounded or undefined (message
/// from the shared validator). This is a pure check; wiring does no I/O.
///
/// The connect + slice queries run at the start of the run (via
/// [`produce_async`](crate::async_source::produce_async)), so a connection
/// failure (with the password redacted) or a slice-query failure **aborts the
/// run**, not wiring — as does a row that fails to decode or a non-monotonic
/// timestamp.
pub fn postgres_read<T>(
    g: &GraphBuilder,
    params: RunParams,
    connection: impl Into<PostgresConnection>,
    period: Duration,
    query_fn: impl FnMut((NanoTime, NanoTime), i32, usize) -> String + Send + 'static,
    buffer_size: Option<usize>,
) -> Result<Stream<Burst<T>>>
where
    T: PostgresDeserialize + Clone + Default + Send + 'static,
{
    let connection = connection.into();
    // Validate the run window and split it into slices at wiring — a pure,
    // fail-fast check with no I/O. The authoritative run start (a RealTime run
    // yields ZERO, which the shared validator rejects with a "use
    // RunMode::HistoricalFrom" message).
    let start_time = match params.run_mode {
        RunMode::HistoricalFrom(t) => t,
        RunMode::RealTime => NanoTime::ZERO,
    };
    let end_time_result: Result<NanoTime> = match params.run_for {
        RunFor::Duration(d) => Ok(start_time + d),
        RunFor::Forever => Ok(NanoTime::MAX),
        RunFor::Cycles(_) => Err(anyhow::anyhow!("end_time not available for RunFor::Cycles")),
    };
    // Capture the concrete bound before the validator consumes the Result
    // (NanoTime is Copy); validation guarantees it is present (bounded, non-MAX)
    // by the time we unwrap it.
    let end_time_bound = end_time_result.as_ref().ok().copied();
    let slices =
        compute_validated_time_slices("postgres_read", start_time, end_time_result, period)?;
    let end_time =
        end_time_bound.expect("compute_validated_time_slices accepted a bounded end_time");

    // Defer the connect + slice queries to the run via `produce_async`: nothing
    // touches the network at wiring, so a connection or query failure aborts the
    // *run* (not graph construction) — matching legacy's lazy `produce_async`
    // reader. The connect happens in the closure (before the stream); the
    // per-slice queries run lazily inside `read_chunk_stream` as the graph drains,
    // so a `buffer_size` bound keeps the replay bounded in memory and pipelines
    // query I/O with graph compute (legacy's model). The run's window is fixed at
    // wiring (run with the `params` you passed), so the producer's own `RunParams`
    // are unused.
    produce_async(
        g,
        move |_params: RunParams| async move {
            let (client, conn) = tokio_postgres::connect(&connection.conn_str, NoTls)
                .await
                .with_context(|| {
                    format!(
                        "postgres_read: failed to connect: {}",
                        connection.redacted()
                    )
                })?;
            // Drive the connection's protocol handling in the background; it ends when
            // `client` is dropped after the slices are drained (inside the stream).
            let conn_task = tokio::spawn(async move {
                if let Err(e) = conn.await {
                    log::error!("postgres connection error: {e}");
                }
            });
            Ok(read_chunk_stream::<T>(
                client, conn_task, slices, start_time, end_time, query_fn,
            ))
        },
        buffer_size,
    )
}

/// Lazily query each slice and yield its in-window rows as a `(NanoTime, T)`
/// stream — the streaming counterpart of what was an eager `Vec` collector.
///
/// An `async_stream` generator owning the `client` (+ its background connection
/// task): it runs the next query only when polled past the previous slice, so —
/// driven by `produce_async`'s back-pressure — a slice is fetched only
/// once the graph has room. A query, decode, or non-monotonic-time failure yields
/// a trailing `Err` and stops (legacy's per-row abort). Rows outside the slice's
/// window are dropped via [`WindowFilter`]. Timestamps are full (not time-of-day),
/// so ordering is enforced across the whole read, not reset per slice.
fn read_chunk_stream<T>(
    client: tokio_postgres::Client,
    conn_task: tokio::task::JoinHandle<()>,
    slices: Vec<((NanoTime, NanoTime), i32, usize)>,
    start_time: NanoTime,
    end_time: NanoTime,
    mut query_fn: impl FnMut((NanoTime, NanoTime), i32, usize) -> String + Send + 'static,
) -> impl futures::Stream<Item = Result<(NanoTime, T)>> + Send + 'static
where
    T: PostgresDeserialize + Send + 'static,
{
    async_stream::stream! {
        let mut prev_time: Option<NanoTime> = None;

        'outer: for ((t0, t1), date, iteration) in slices {
            let query = query_fn((t0, t1), date, iteration);
            info!("postgres query: {query}");
            let rows = match client.query(&query, &[]).await {
                Ok(r) => r,
                Err(e) => {
                    yield Err(anyhow::Error::new(e).context("postgres_read: query failed"));
                    break;
                }
            };
            info!("postgres query: {} rows", rows.len());

            // The query uses the period-aligned (t0, t1) for clean boundaries, but
            // the first slice's t0 can precede start_time (and the last slice's t1
            // exceed end_time), so a `time >= t0` filter may return rows outside the
            // run window. Drop those via the shared WindowFilter: emitting a row
            // before the graph clock aborts the run, and a row past end_time would
            // drive the monotonic check to reject a later slice.
            let mut filter = WindowFilter::new(
                "postgres_read",
                TimeWindow::clamp(t0, t1, start_time, end_time),
            );

            for row in &rows {
                let (time, record) = match T::from_row(row) {
                    Ok(r) => r,
                    Err(e) => { yield Err(e); break 'outer; }
                };

                if !filter.keep(time) {
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

            filter.finish();
        }

        drop(client);
        let _ = conn_task.await;
    }
}

// ---------------------------------------------------------------------------
// Source: real-time LISTEN/NOTIFY live tail
// ---------------------------------------------------------------------------

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

/// Live-tail a PostgreSQL table in real time using `LISTEN`/`NOTIFY`, as a
/// `Burst<T>` source.
///
/// Complements [`postgres_read`] (bounded historical replay) with an unbounded
/// real-time source: rows inserted while the graph runs are streamed on-graph as
/// they commit. Notifications are a **wake-up signal only** — the adapter opens a
/// connection, issues `LISTEN <channel>` **before** the first query, runs
/// `query_fn(cursor)` (starting at `start_from`) to catch up, then sleeps until a
/// notification arrives, coalesces any others, and re-queries past the cursor.
/// Rows always come from a query decoded by the same [`PostgresDeserialize`]
/// impl, so it is robust to `NOTIFY`'s lossiness. Wire the table up with the SQL
/// from [`postgres_notify_trigger_sql`].
///
/// `query_fn` receives the cursor and must return SQL selecting rows with
/// `time > cursor`, ordered by time. The time column must be **strictly
/// increasing** across inserts (the cursor query is `time > cursor`, so a row at
/// or before the cursor is silently dropped).
///
/// # Errors
///
/// Returns an error at **wiring time** if `run_mode` is
/// [`RunMode::HistoricalFrom`]: the live tail is a live, unbounded,
/// wall-clock-stamped stream with no historical timeline to replay, and the
/// historical channel path would block-collect it up front and deadlock at
/// `start`. Use [`postgres_read`] for historical replay and run `postgres_sub`
/// under [`RunMode::RealTime`]. A connection, `LISTEN`, or query failure aborts
/// the run with context (the connection string is redacted).
pub fn postgres_sub<T, F>(
    g: &GraphBuilder,
    run_mode: RunMode,
    connection: impl Into<PostgresConnection>,
    channel: impl Into<String>,
    start_from: NanoTime,
    query_fn: F,
) -> Result<Stream<Burst<T>>>
where
    T: PostgresDeserialize + Clone + Default + Send + 'static,
    F: FnMut(NanoTime) -> String + Send + 'static,
{
    if let RunMode::HistoricalFrom(_) = run_mode {
        anyhow::bail!(
            "postgres_sub: RunMode::HistoricalFrom is unsupported — the LISTEN/NOTIFY \
             live tail is a live, unbounded, wall-clock-stamped stream with no \
             historical timeline to replay; use postgres_read for historical replay \
             and run postgres_sub under RunMode::RealTime"
        );
    }
    let connection = connection.into();
    let channel = channel.into();
    produce_async(
        g,
        move |_p: RunParams| async move {
            let mut query_fn = query_fn;
            let (client, mut conn) = tokio_postgres::connect(&connection.conn_str, NoTls)
                .await
                .with_context(|| {
                    format!("postgres_sub: failed to connect: {}", connection.redacted())
                })?;

            // Drive the connection and forward notification wake-ups. Polling
            // `poll_message` (rather than awaiting the plain connection future) is
            // what surfaces `AsyncMessage::Notification`s.
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

                    // Wait for the next wake-up; coalesce any that queued while we
                    // were querying (they all mean the same thing: re-query).
                    match rx.next().await {
                        Some(()) => {
                            // Coalesce queued wake-ups; each means "re-query".
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
        },
        None,
    )
}

// ---------------------------------------------------------------------------
// Source: unified mode-agnostic source (dispatches read vs sub on run_mode)
// ---------------------------------------------------------------------------

/// The historical and live query specs for [`postgres_source`], each optional.
///
/// [`RunMode`] is a *run-time* choice, but [`postgres_read`] / [`postgres_sub`]
/// are mode-locked — forcing that choice into *wiring*. This config carries both
/// mechanisms so a single [`postgres_source`] call can dispatch on the run's mode
/// at wiring: supply **both** halves for a fully mode-agnostic graph (flip the
/// mode at `run()`, downstream wiring unchanged); supply **one** and the other
/// mode errors at wiring naming the missing half. See the deviation register's
/// "B2 — agreed plan: unified `<adapter>_source`".
#[derive(Default)]
pub struct PostgresSourceConfig {
    historical: Option<HistoricalSpec>,
    live: Option<LiveSpec>,
}

/// The historical half — see [`PostgresSourceConfig::historical`].
struct HistoricalSpec {
    period: Duration,
    query_fn: Box<dyn FnMut((NanoTime, NanoTime), i32, usize) -> String + Send>,
    buffer_size: Option<usize>,
}

/// The live half — see [`PostgresSourceConfig::live`].
struct LiveSpec {
    channel: String,
    start_from: NanoTime,
    query_fn: Box<dyn FnMut(NanoTime) -> String + Send>,
}

impl PostgresSourceConfig {
    /// An empty config — add halves with [`historical`](Self::historical) and/or
    /// [`live`](Self::live).
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure the **historical** (bounded, time-sliced replay) half — the
    /// [`postgres_read`] mechanism. `period` is the slice length; `query_fn` is
    /// called once per slice with `((t0, t1), date, iteration)` and returns SQL
    /// filtering `time >= t0 AND time < t1`, ordered by time. `buffer_size` bounds
    /// the replay's look-ahead as back-pressure (`None` = unbounded), exactly as
    /// [`postgres_read`]'s `buffer_size`.
    pub fn historical(
        mut self,
        period: Duration,
        query_fn: impl FnMut((NanoTime, NanoTime), i32, usize) -> String + Send + 'static,
        buffer_size: Option<usize>,
    ) -> Self {
        self.historical = Some(HistoricalSpec {
            period,
            query_fn: Box::new(query_fn),
            buffer_size,
        });
        self
    }

    /// Configure the **live** (`LISTEN`/`NOTIFY` tail) half — the [`postgres_sub`]
    /// mechanism. `channel` is the notify channel, `start_from` the initial cursor,
    /// and `query_fn(cursor)` returns SQL selecting rows with `time > cursor`,
    /// ordered by time.
    pub fn live(
        mut self,
        channel: impl Into<String>,
        start_from: NanoTime,
        query_fn: impl FnMut(NanoTime) -> String + Send + 'static,
    ) -> Self {
        self.live = Some(LiveSpec {
            channel: channel.into(),
            start_from,
            query_fn: Box::new(query_fn),
        });
        self
    }
}

/// A single **mode-agnostic** PostgreSQL source: dispatches on the run's
/// [`RunMode`] at wiring time to the bounded historical read ([`postgres_read`])
/// or the live `LISTEN`/`NOTIFY` tail ([`postgres_sub`]), per the
/// [`PostgresSourceConfig`] halves.
///
/// - [`RunMode::HistoricalFrom`] → the historical half (requires
///   [`PostgresSourceConfig::historical`]).
/// - [`RunMode::RealTime`] → the live half (requires
///   [`PostgresSourceConfig::live`]).
///
/// With both halves supplied the graph is fully mode-agnostic — pick real-time vs
/// historical at `run()` with the wiring unchanged, exactly as `ticker` /
/// `channel` already allow. [`postgres_read`] / [`postgres_sub`] remain the
/// low-level primitives; this wires through them. See the deviation register's
/// "B2 — agreed plan: unified `<adapter>_source`".
///
/// # Errors
///
/// Returns an error at **wiring time** if the config lacks the half the run's mode
/// needs (the message names the missing half), or for any error the dispatched
/// primitive surfaces at wiring (`postgres_read`'s bounded-window validation, or a
/// connection failure with the password redacted).
pub fn postgres_source<T>(
    g: &GraphBuilder,
    params: RunParams,
    connection: impl Into<PostgresConnection>,
    cfg: PostgresSourceConfig,
) -> Result<Stream<Burst<T>>>
where
    T: PostgresDeserialize + Clone + Default + Send + 'static,
{
    let connection = connection.into();
    match params.run_mode {
        RunMode::HistoricalFrom(_) => {
            let hist = cfg.historical.ok_or_else(|| {
                anyhow::anyhow!(
                    "postgres_source: run mode is RunMode::HistoricalFrom but no historical \
                     config was supplied — add .historical(period, query_fn, buffer_size), or run under \
                     RunMode::RealTime with a .live(..) config"
                )
            })?;
            postgres_read::<T>(
                g,
                params,
                connection,
                hist.period,
                hist.query_fn,
                hist.buffer_size,
            )
        }
        RunMode::RealTime => {
            let live = cfg.live.ok_or_else(|| {
                anyhow::anyhow!(
                    "postgres_source: run mode is RunMode::RealTime but no live config was \
                     supplied — add .live(channel, start_from, query_fn), or run under \
                     RunMode::HistoricalFrom with a .historical(..) config"
                )
            })?;
            postgres_sub::<T, _>(
                g,
                params.run_mode,
                connection,
                live.channel,
                live.start_from,
                live.query_fn,
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Sink: streaming insert
// ---------------------------------------------------------------------------

/// Trait for serializing a Rust record into PostgreSQL column values.
///
/// Return the **business** column values only, in the same order they appear in
/// the target table *after* the time column. The graph timestamp is prepended
/// automatically by [`PostgresSinkOps::postgres_write`] as the first inserted
/// column — do not include it.
///
/// Each value is returned as an owned `Box<dyn ToSql>`, so this allocates once
/// per column per row — a deliberate trade for an open type set and an
/// object-safe method, a non-issue at the low-frequency write rates this adapter
/// targets.
pub trait PostgresSerialize {
    /// Owned, boxed column values (excluding time) in table column order.
    fn to_params(&self) -> Vec<Box<dyn ToSql + Sync + Send>>;
}

/// One burst's worth of records sharing the burst's graph timestamp — the unit
/// [`consume_async`] drains per write, so an N-record burst is inserted as one
/// pipelined batch (matching legacy's per-burst insert).
#[derive(Clone, Default)]
struct WriteBatch<T> {
    time: NanoTime,
    records: Vec<T>,
}

/// Fluent extension for writing `Burst<T>` streams to a PostgreSQL table.
///
/// Enabled with `use wingfoil::adapters::postgres::PostgresSinkOps;`. Like
/// legacy, the sink is **burst-only** (write single records via
/// `constant(burst![record])` / a `Stream<Burst<T>>`), because time is prepended
/// per burst.
pub trait PostgresSinkOps<T> {
    /// Insert each record as one row `(time, <to_params()…>)`, the graph
    /// timestamp prepended as the first column. The target table's columns must
    /// line up positionally: `(timestamp, <business columns in to_params()
    /// order>)`. Returns the sink `Stream<()>`.
    ///
    /// - `buffer_size`: back-pressure bound handed to
    ///   [`consume_async`](crate::async_source::consume_async) — `Some(n)` blocks
    ///   the graph once ~`n` bursts are unwritten; `None` is unbounded.
    ///
    /// The graph owns the tokio runtime (see the module docs).
    ///
    /// # Errors
    ///
    /// The connection is opened lazily on the first write (on the consumer
    /// task), so a connection failure aborts the *run* — not wiring — with
    /// context (the connection string is redacted). A per-burst insert failure
    /// likewise aborts the run with context on a subsequent cycle.
    fn postgres_write(
        &self,
        conn: impl Into<PostgresConnection>,
        table: &str,
        buffer_size: Option<usize>,
    ) -> Result<Stream<()>>;
}

impl<T> PostgresSinkOps<T> for Stream<Burst<T>>
where
    T: PostgresSerialize + Clone + Default + Send + 'static,
{
    fn postgres_write(
        &self,
        conn: impl Into<PostgresConnection>,
        table: &str,
        buffer_size: Option<usize>,
    ) -> Result<Stream<()>> {
        let connection = conn.into();
        // Identifier-quoted (per dot-separated segment) so mixed-case and
        // reserved-word table names work; also closes the interpolation hole.
        let table_sql = quote_table(table);

        // The graph owns the runtime; the sink connects/drives/writes on it.
        let g = self.graph();
        // The connection is established lazily on the consumer task on the first
        // write (see below), not here — so wiring opens no socket and a connect
        // failure surfaces during the run (via `consume_async`'s error channel),
        // matching legacy's connect-at-run behaviour rather than failing at
        // graph construction.
        let conn_str = connection.conn_str.clone();
        let redacted = connection.redacted();
        let client_cell: Arc<tokio::sync::Mutex<Option<Arc<tokio_postgres::Client>>>> =
            Arc::new(tokio::sync::Mutex::new(None));
        // Prepared once the column count is known (from the first burst), then
        // reused. `consume_async` runs each write to completion before the next
        // (single consumer), so this cache is never contended; the async Mutex
        // only bridges the `Fn` sink closure to the shared statement, and it lives
        // on the background task — off the graph execution path.
        let stmt_cache: Arc<tokio::sync::Mutex<Option<Statement>>> =
            Arc::new(tokio::sync::Mutex::new(None));

        let (sink, flush) = consume_async(&g, buffer_size, move |batch: WriteBatch<T>| {
            let client_cell = Arc::clone(&client_cell);
            let stmt_cache = Arc::clone(&stmt_cache);
            let table_sql = table_sql.clone();
            let conn_str = conn_str.clone();
            let redacted = redacted.clone();
            async move {
                if batch.records.is_empty() {
                    return Ok(());
                }
                // Connect on first use (deferred off the wiring path). The socket
                // opens on the consumer task at run time; a connect failure here
                // aborts the run with context. Cached for subsequent bursts.
                let client = {
                    let mut guard = client_cell.lock().await;
                    match guard.as_ref() {
                        Some(c) => Arc::clone(c),
                        None => {
                            let (client, driver) = tokio_postgres::connect(&conn_str, NoTls)
                                .await
                                .with_context(|| {
                                    format!("postgres_write: failed to connect: {redacted}")
                                })?;
                            tokio::spawn(async move {
                                if let Err(e) = driver.await {
                                    log::error!("postgres connection error: {e}");
                                }
                            });
                            let client = Arc::new(client);
                            *guard = Some(Arc::clone(&client));
                            client
                        }
                    }
                };
                // All records in a burst share the graph timestamp.
                let ts: NaiveDateTime = batch.time.into();
                // Serialize the whole batch up front so the inserts below can borrow.
                let rows: Vec<Vec<Box<dyn ToSql + Sync + Send>>> = batch
                    .records
                    .iter()
                    .map(|record| record.to_params())
                    .collect();
                let n = rows[0].len() + 1; // + time column

                // Prepare (once) or reuse the INSERT statement.
                let stmt = {
                    let mut guard = stmt_cache.lock().await;
                    match guard.as_ref() {
                        Some(s) => s.clone(),
                        None => {
                            let placeholders = (1..=n)
                                .map(|i| format!("${i}"))
                                .collect::<Vec<_>>()
                                .join(", ");
                            let sql = format!("INSERT INTO {table_sql} VALUES ({placeholders})");
                            let s = client.prepare(&sql).await.with_context(|| {
                                format!("postgres_write: failed to prepare `{sql}`")
                            })?;
                            *guard = Some(s.clone());
                            s
                        }
                    }
                };

                // Launch every insert in the burst concurrently: tokio-postgres
                // pipelines in-flight extended-protocol requests over the single
                // connection, so an N-record burst costs ~1 round trip.
                let inserts = rows.iter().map(|values| {
                    let mut params: Vec<&(dyn ToSql + Sync)> = Vec::with_capacity(n);
                    params.push(&ts);
                    for value in values {
                        params.push(value.as_ref());
                    }
                    let client = &client;
                    let stmt = &stmt;
                    async move { client.execute(stmt, &params).await }
                });
                futures::future::try_join_all(inserts)
                    .await
                    .with_context(|| format!("postgres_write: insert into {table_sql} failed"))?;
                Ok(())
            }
        })?;

        Ok(self
            .with_time()
            .map(
                |(time, burst): &(NanoTime, Burst<T>)| -> Burst<WriteBatch<T>> {
                    burst![WriteBatch {
                        time: *time,
                        records: burst.iter().cloned().collect(),
                    }]
                },
            )
            .for_each(sink)
            .finally(flush))
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
    fn test_redacted_masks_password() {
        let conn = PostgresConnection::new(
            "host=localhost port=5432 user=postgres password=s3cr3t dbname=postgres",
        );
        let out = conn.redacted();
        assert!(!out.contains("s3cr3t"), "password leaked: {out}");
        assert_eq!(
            out,
            "host=localhost port=5432 user=postgres password=*** dbname=postgres"
        );
    }

    #[test]
    fn test_redacted_is_case_insensitive() {
        let conn = PostgresConnection::new("host=x PassWord=hunter2");
        assert_eq!(conn.redacted(), "host=x password=***");
    }

    #[test]
    fn test_redacted_noop_without_password() {
        let conn = PostgresConnection::new("host=localhost dbname=db");
        assert_eq!(conn.redacted(), "host=localhost dbname=db");
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

    #[test]
    fn test_postgres_timestamp_format() {
        // 2000-01-01 00:00:00 UTC (KDB epoch). `%.6f` always renders 6 digits.
        let t = NanoTime::from_kdb_timestamp(0);
        assert_eq!(postgres_timestamp(t), "2000-01-01 00:00:00.000000");

        // One second + 500 microseconds later.
        let t = NanoTime::from_kdb_timestamp(1_000_000_500_000);
        assert_eq!(postgres_timestamp(t), "2000-01-01 00:16:40.000500");
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
