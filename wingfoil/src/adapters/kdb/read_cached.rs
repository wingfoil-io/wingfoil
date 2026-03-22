//! Cached variant of `kdb_read` — checks a file cache before each time-slice query.

use super::read::compute_time_slices;
use super::{KdbConnection, KdbDeserialize, KdbExt, SymbolInterner};
use crate::adapters::cache::{CacheConfig, CacheKey, FileCache};
use crate::nodes::produce_async;
use crate::types::*;
use anyhow::bail;
use kdb_plus_fixed::ipc::{ConnectionMethod, K, QStream};
use log::info;
use std::rc::Rc;

/// Cached version of [`kdb_read`].
///
/// Checks a file cache before executing each time-slice query. On a cache miss
/// the query is executed against KDB+ and the result is written to disk. On
/// subsequent runs the cached result is returned without opening a TCP
/// connection to KDB+.
///
/// Cache files live in `cache_config.folder` and are evicted using an LRU
/// policy once the total on-disk size exceeds `cache_config.max_size_bytes`.
/// Use [`CacheConfig::clear`] to remove all cached files, or set
/// `max_size_bytes` to `u64::MAX` for an unbounded cache.
///
/// # Serde requirement
///
/// `T` must additionally implement `serde::Serialize + serde::Deserialize`. If
/// your type contains [`Sym`], this is supported — `Sym` serializes as a plain
/// string. Note that interning is **not** restored on deserialization (each `Sym`
/// gets its own `Arc<str>`), but this is irrelevant for cached data fed into the
/// graph.
///
/// # Schema evolution
///
/// `bincode` is not self-describing. If `T` changes (added/renamed/reordered
/// fields), old cache files will deserialise as garbage or return an error. The
/// fix is to call [`CacheConfig::clear`] (or delete the cache directory) and re-run.
///
/// # Example
/// ```ignore
/// let config = CacheConfig::new("/tmp/my-backtest-cache", 512 * 1024 * 1024);
/// let stream = kdb_read_cached::<Trade, _>(
///     KdbConnection::new("localhost", 5000),
///     Duration::from_secs(3600),
///     config,
///     |(t0, t1), date, _| format!(
///         "select from trades where date=2000.01.01+{}, \
///          time >= (`timestamp$){}j, time < (`timestamp$){}j",
///         date, t0.to_kdb_timestamp(), t1.to_kdb_timestamp()
///     ),
/// );
/// ```
#[must_use]
pub fn kdb_read_cached<T, F>(
    connection: KdbConnection,
    period: std::time::Duration,
    cache_config: CacheConfig,
    query_fn: F,
) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element
        + Send
        + Sync
        + KdbDeserialize
        + serde::Serialize
        + for<'de> serde::Deserialize<'de>
        + 'static,
    F: FnMut((NanoTime, NanoTime), i32, usize) -> String + Send + 'static,
{
    produce_async(move |ctx| {
        let start_time = ctx.start_time;
        let end_time_result = ctx.end_time();

        async move {
            if start_time == NanoTime::ZERO {
                bail!(
                    "kdb_read_cached: start_time is NanoTime::ZERO; \
                    use RunMode::HistoricalFrom with an explicit start time"
                );
            }

            let end_time = match end_time_result {
                Ok(t) if t == NanoTime::MAX => bail!(
                    "kdb_read_cached requires RunFor::Duration; \
                    RunFor::Forever would generate an unbounded number of slices"
                ),
                Ok(t) => t,
                Err(_) => bail!(
                    "kdb_read_cached requires RunFor::Duration; \
                    RunFor::Cycles does not provide an end time"
                ),
            };

            tokio::fs::create_dir_all(&cache_config.folder).await?;
            let cache = FileCache::<T>::new(cache_config);
            let slices = compute_time_slices(start_time, end_time, period);
            let host = connection.host.clone();
            let port_str = connection.port.to_string();

            Ok(async_stream::stream! {
                let mut socket: Option<QStream> = None;
                let mut interner = SymbolInterner::default();
                let mut query_fn = query_fn;

                'slices: for (within, date, iteration) in slices {
                    let query = query_fn(within, date, iteration);
                    let key = CacheKey::from_parts(&[&host, &port_str, &query]);

                    let cached = match cache.get(&key).await {
                        Ok(Some(rows)) => Some(rows),
                        Ok(None) => None,
                        Err(e) => {
                            log::warn!("KDB cache read error (falling back to KDB): {e}");
                            None
                        }
                    };

                    if let Some(rows) = cached {
                        for (time, record) in rows {
                            yield Ok((time, record));
                        }
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
                                yield Err(e.into());
                                break 'slices;
                            }
                        }
                    }
                    let sock = socket.as_mut().unwrap();

                    info!("KDB query: {}", query);
                    let fetch_start = std::time::Instant::now();
                    let result: K = match sock.send_sync_message(&query.as_str()).await {
                        Ok(r) => r,
                        Err(e) => {
                            yield Err(e.into());
                            break 'slices;
                        }
                    };

                    let (columns, rows) = match (result.column_names(), result.rows()) {
                        (Ok(cols), Ok(rows)) => (cols, rows),
                        (Err(e), _) | (_, Err(e)) => {
                            yield Err(e);
                            break 'slices;
                        }
                    };

                    let row_count = rows.len();
                    info!("KDB query: {} rows in {:?}", row_count, fetch_start.elapsed());

                    // Parse rows, validate ascending time order, and collect.
                    // Only cached after a full successful parse — no partial writes.
                    let mut parsed: Vec<(NanoTime, T)> = Vec::with_capacity(row_count);
                    let mut prev_time: Option<NanoTime> = None;
                    for row in &rows {
                        let (time, record) = match T::from_kdb_row(row, &columns, &mut interner) {
                            Ok(r) => r,
                            Err(e) => {
                                yield Err(e.into());
                                break 'slices;
                            }
                        };
                        if let Some(prev) = prev_time
                            && time < prev
                        {
                            yield Err(anyhow::anyhow!(
                                "KDB data is not sorted by time: got {:?} after {:?}. \
                                Add `xasc` to your query to sort the data.",
                                time,
                                prev
                            ));
                            break 'slices;
                        }
                        prev_time = Some(time);
                        parsed.push((time, record));
                    }

                    // Write to cache; log on failure but don't abort the stream.
                    if let Err(e) = cache.put(&key, &query, &parsed).await {
                        log::warn!("KDB cache write error: {e}");
                    }

                    for (time, record) in parsed {
                        yield Ok((time, record));
                    }
                }
            })
        }
    })
}
