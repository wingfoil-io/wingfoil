//! Cached variant of [`kdb_read`](super::kdb_read) — checks a file cache before
//! each time-slice query.

use super::read::{KdbDeserialize, KdbExt};
use super::{KdbConnection, SymbolInterner};
use crate::Burst;
use crate::adapters::cache::{CacheConfig, CacheKey, FileCache};
use crate::adapters::common::{TimeWindow, WindowFilter, compute_validated_time_slices};
use crate::async_source::{RunParams, produce_async};
use crate::fluent::{GraphBuilder, Stream};
use anyhow::{Context, Result};
use kdb_plus_fixed::ipc::{ConnectionMethod, K, QStream};
use log::info;
use wingfoil::{NanoTime, RunFor, RunMode};

/// Cached version of [`kdb_read`](super::kdb_read).
///
/// Checks a file cache before executing each time-slice query. On a cache miss
/// the query is executed against KDB+ and the result is written to disk; on
/// subsequent runs the cached result is returned without opening a TCP
/// connection. If all slices are cache hits, no KDB connection is opened.
///
/// Cache files live in `cache_config.folder` and are evicted with an LRU policy
/// once the total on-disk size exceeds `cache_config.max_size_bytes`. Use
/// [`CacheConfig::clear`] to remove all cached files, or set `max_size_bytes` to
/// `u64::MAX` for an unbounded cache.
///
/// # Serde requirement
///
/// `T` must additionally implement `serde::Serialize + Deserialize + Sync`. If
/// your type contains [`Sym`](super::Sym), this is supported — it serializes as a
/// plain string (interning is not restored on deserialization, which is
/// irrelevant for cached data fed into the graph).
///
/// # Schema evolution
///
/// `bincode` is not self-describing. If `T` changes (added/renamed/reordered
/// fields), old cache files deserialise as garbage or error. Call
/// [`CacheConfig::clear`] (or delete the cache directory) and re-run.
///
/// # Out-of-window rows
///
/// Like [`kdb_read`](super::kdb_read), rows a query returns outside the run's
/// `[start_time, end_time)` window are dropped at emit time (with a per-slice
/// warning) rather than aborting the run. The **cache stores the full slice
/// result** (`[t0, t1)`), because the cache key is the query string and does not
/// encode `start_time`/`end_time`; the clamp is applied on the way out, on both
/// cache hits and misses.
///
/// # Errors
///
/// Returns an error at **wiring time** only if `params` does not describe a
/// bounded historical run (as [`kdb_read`](super::kdb_read)) — a pure check, no
/// I/O. A connection failure (host:port only — never the password), a query
/// failure, a decode failure, or a non-monotonic timestamp **aborts the run**.
pub fn kdb_read_cached<T>(
    g: &GraphBuilder,
    params: RunParams,
    connection: KdbConnection,
    period: std::time::Duration,
    cache_config: CacheConfig,
    query_fn: impl FnMut((NanoTime, NanoTime), i32, usize) -> String + Send + 'static,
) -> Result<Stream<Burst<T>>>
where
    T: KdbDeserialize
        + Clone
        + Default
        + Send
        + Sync
        + serde::Serialize
        + for<'de> serde::Deserialize<'de>
        + 'static,
{
    // Validate the run window and slice it at wiring — a pure, fail-fast check
    // with no I/O (see kdb_read).
    let start_time = match params.run_mode {
        RunMode::HistoricalFrom(t) => t,
        RunMode::RealTime => NanoTime::ZERO,
    };
    let end_time_result: Result<NanoTime> = match params.run_for {
        RunFor::Duration(d) => Ok(start_time + d),
        RunFor::Forever => Ok(NanoTime::MAX),
        RunFor::Cycles(_) => Err(anyhow::anyhow!("end_time not available for RunFor::Cycles")),
    };
    let end_time_bound = end_time_result.as_ref().ok().copied();
    let slices =
        compute_validated_time_slices("kdb_read_cached", start_time, end_time_result, period)?;
    let end_time =
        end_time_bound.expect("compute_validated_time_slices accepted a bounded end_time");

    produce_async(g, move |_p: RunParams| async move {
        let rows = read_cached_slices::<T>(
            &connection,
            &slices,
            start_time,
            end_time,
            cache_config,
            query_fn,
        )
        .await?;
        Ok(futures::stream::iter(
            rows.into_iter()
                .map(|r| r.map(|(record, time)| (time, record))),
        ))
    })
}

/// Query each slice through the file cache, decode + window-clamp the rows, and
/// return the finite `Result<(record, time)>` sequence for `produce_async`.
///
/// Connects lazily on the first cache miss (no connection at all if every slice
/// is a hit). A connection failure returns `Err`; a query / decode /
/// non-monotonic-time failure is pushed as a trailing `Err` so it aborts the run
/// through the producer stream.
async fn read_cached_slices<T>(
    connection: &KdbConnection,
    slices: &[((NanoTime, NanoTime), i32, usize)],
    start_time: NanoTime,
    end_time: NanoTime,
    cache_config: CacheConfig,
    mut query_fn: impl FnMut((NanoTime, NanoTime), i32, usize) -> String,
) -> Result<Vec<Result<(T, NanoTime)>>>
where
    T: KdbDeserialize + Clone + serde::Serialize + for<'de> serde::Deserialize<'de>,
{
    tokio::fs::create_dir_all(&cache_config.folder)
        .await
        .with_context(|| {
            format!(
                "kdb_read_cached: creating cache dir {}",
                cache_config.folder.display()
            )
        })?;
    let cache = FileCache::<T>::new(cache_config);

    let mut socket: Option<QStream> = None;
    let mut interner = SymbolInterner::default();
    let mut out: Vec<Result<(T, NanoTime)>> = Vec::new();

    'slices: for &(within, date, iteration) in slices {
        // Effective window for this slice: clamp the period-aligned (t0, t1) to
        // the run's [start_time, end_time). The cache still stores the full
        // [t0, t1) result, since the cache key is the query string.
        let (t0, t1) = within;
        let window = TimeWindow::clamp(t0, t1, start_time, end_time);
        let query = query_fn(within, date, iteration);
        let key = CacheKey::from_parts(&[&query]);

        let cached = match cache.get(&key).await {
            Ok(Some(rows)) => Some(rows),
            Ok(None) => None,
            Err(e) => {
                log::warn!("KDB cache read error (falling back to KDB): {e}");
                None
            }
        };

        if let Some(rows) = cached {
            let mut filter = WindowFilter::new("kdb_read_cached", window);
            for (time, record) in rows {
                if !filter.keep(time) {
                    continue;
                }
                out.push(Ok((record, time)));
            }
            filter.finish();
            continue;
        }

        // Cache miss or corrupt — connect lazily and query KDB.
        if socket.is_none() {
            let creds = connection.credentials_string();
            match QStream::connect(
                ConnectionMethod::TCP,
                &connection.host,
                connection.port,
                &creds,
            )
            .await
            {
                Ok(s) => socket = Some(s),
                Err(e) => {
                    out.push(Err(anyhow::Error::new(e).context(format!(
                        "kdb_read_cached: failed to connect to {}",
                        connection.redacted()
                    ))));
                    break 'slices;
                }
            }
        }
        let sock = socket
            .as_mut()
            .expect("socket initialised on cache miss above");

        info!("KDB query: {query}");
        let fetch_start = std::time::Instant::now();
        let result: K = match sock.send_sync_message(&query.as_str()).await {
            Ok(r) => r,
            Err(e) => {
                out.push(Err(e.into()));
                break 'slices;
            }
        };

        let (columns, rows) = match (result.column_names(), result.rows()) {
            (Ok(cols), Ok(rows)) => (cols, rows),
            (Err(e), _) | (_, Err(e)) => {
                out.push(Err(e));
                break 'slices;
            }
        };

        let row_count = rows.len();
        info!(
            "KDB query: {} rows in {:?}",
            row_count,
            fetch_start.elapsed()
        );

        // Parse rows, validate ascending time order, and collect. Only cached
        // after a full successful parse — no partial writes.
        let mut parsed: Vec<(NanoTime, T)> = Vec::with_capacity(row_count);
        let mut prev_time: Option<NanoTime> = None;
        for row in &rows {
            let (time, record) = match T::from_kdb_row(row, &columns, &mut interner) {
                Ok(r) => r,
                Err(e) => {
                    out.push(Err(e.into()));
                    break 'slices;
                }
            };
            if let Some(prev) = prev_time
                && time < prev
            {
                out.push(Err(anyhow::anyhow!(
                    "KDB data is not sorted by time: got {time:?} after {prev:?}. \
                     Add `xasc` to your query to sort the data."
                )));
                break 'slices;
            }
            prev_time = Some(time);
            parsed.push((time, record));
        }

        // Write to cache; log on failure but don't abort the stream.
        if let Err(e) = cache.put(&key, &query, &parsed).await {
            log::warn!("KDB cache write error: {e}");
        }

        let mut filter = WindowFilter::new("kdb_read_cached", window);
        for (time, record) in parsed {
            if !filter.keep(time) {
                continue;
            }
            out.push(Ok((record, time)));
        }
        filter.finish();
    }

    Ok(out)
}
