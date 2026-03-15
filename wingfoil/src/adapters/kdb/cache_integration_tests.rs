//! Integration tests for `kdb_read_cached`.
//!
//! Requires a running KDB+ instance:
//! ```sh
//! q -p 5000
//! ```
//! Run with:
//! ```sh
//! RUST_LOG=INFO cargo test kdb::cache_integration_tests --features kdb-integration-test -p wingfoil -- --test-threads=1 --no-capture
//! ```

use super::integration_tests::{TABLE_NAME, slice_query, with_test_data};
use super::*;
use crate::{RunFor, RunMode, nodes::*, types::*};
use anyhow::Result;

const PERIOD: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// A trade type with serde support for use with `kdb_read_cached`.
/// `Sym` is supported — it serializes as a plain string.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct TestTradeCached {
    sym: Sym,
    price: f64,
    qty: i64,
}

impl KdbDeserialize for TestTradeCached {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        interner: &mut SymbolInterner,
    ) -> Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(1)?; // col 0: date, col 1: time
        Ok((
            time,
            TestTradeCached {
                sym: row.get_sym(2, interner)?,
                price: row.get(3)?.get_float()?,
                qty: row.get(4)?.get_long()?,
            },
        ))
    }
}

/// Run `kdb_read_cached` over one day of `TABLE_NAME` and return the row count.
fn run_cached(conn: KdbConnection, cache_dir: &std::path::Path) -> Result<usize> {
    let stream = kdb_read_cached::<TestTradeCached, _>(
        conn,
        PERIOD,
        cache_dir.to_path_buf(),
        |within, date, _| slice_query(date, within.0, within.1),
    );
    let collected = stream.collapse().collect();
    collected.clone().run(
        RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
        RunFor::Duration(std::time::Duration::from_secs(86400)),
    )?;
    Ok(collected.peek_value().len())
}

fn count_cache_files(cache_dir: &std::path::Path) -> usize {
    std::fs::read_dir(cache_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "cache").unwrap_or(false))
        .count()
}

/// First run fetches from KDB and populates the cache. Second run uses a
/// deliberately closed port — if the cache is hit for every slice, no TCP
/// connection is attempted and the run still succeeds with identical data.
///
/// This test exercises both "cache populates on first run" and "lazy connection
/// skipped on full cache hit" in a single pass.
#[test]
fn test_kdb_read_cached_populates_and_hits() -> Result<()> {
    let _ = env_logger::try_init();
    let cache_dir =
        std::env::temp_dir().join(format!("wingfoil_cache_integ_{}", std::process::id()));

    // First run: all slices are cache misses → fetches from KDB, writes cache.
    with_test_data(3, 1, true, |_n, conn| {
        let n = run_cached(conn, &cache_dir)?;
        assert_eq!(n, 3, "First run should read 3 rows from KDB");
        assert!(
            count_cache_files(&cache_dir) > 0,
            "Cache directory should contain .cache files after first run"
        );
        Ok(())
    })?;
    // Table is now dropped from KDB.

    // Second run: all slices are cache hits → closed port is never dialled.
    let closed = KdbConnection::new("localhost", 59999);
    let n = run_cached(closed, &cache_dir)?;
    assert_eq!(n, 3, "Second run should return same 3 rows from cache");

    std::fs::remove_dir_all(&cache_dir).ok();
    Ok(())
}

/// After cache population, corrupt one cache file with invalid bincode. The
/// next run should log a warning, fall back to KDB, overwrite the bad file,
/// and return the correct rows. A subsequent run with a closed port confirms
/// the overwritten file is now valid.
#[test]
fn test_kdb_read_cached_corrupt_fallback() -> Result<()> {
    let _ = env_logger::try_init();
    let cache_dir =
        std::env::temp_dir().join(format!("wingfoil_cache_corrupt_{}", std::process::id()));

    with_test_data(3, 1, true, |_n, conn| {
        // Populate cache.
        let n = run_cached(conn.clone(), &cache_dir)?;
        assert_eq!(n, 3);

        // Corrupt the first .cache file with garbage bincode after the header.
        let corrupt_path = std::fs::read_dir(&cache_dir)?
            .filter_map(|e| e.ok())
            .find(|e| e.path().extension().map(|x| x == "cache").unwrap_or(false))
            .expect("should have a cache file")
            .path();
        std::fs::write(
            &corrupt_path,
            format!("select from {}\ngarbage not valid bincode", TABLE_NAME),
        )?;

        // Re-run with real connection: corrupt file triggers fallback, then
        // overwrites the file with valid data.
        let n = run_cached(conn, &cache_dir)?;
        assert_eq!(n, 3, "Fallback run should still return 3 rows");

        Ok(())
    })?;
    // Table is now dropped from KDB.

    // All cache files are now valid — closed port should succeed.
    let closed = KdbConnection::new("localhost", 59999);
    let n = run_cached(closed, &cache_dir)?;
    assert_eq!(n, 3, "After corrupt-file overwrite, cache hit should work");

    std::fs::remove_dir_all(&cache_dir).ok();
    Ok(())
}

/// Partial cache: some slices are cached, one is missing. The run should fetch
/// only the missing slice from KDB (opening a connection exactly once) and
/// serve the rest from cache. A subsequent run against a closed port confirms
/// all slices are now cached.
#[test]
fn test_kdb_read_cached_partial_cache() -> Result<()> {
    let _ = env_logger::try_init();
    let cache_dir =
        std::env::temp_dir().join(format!("wingfoil_cache_partial_{}", std::process::id()));

    // 12-hour slices over 2 days → 4 slices total, so we can delete one.
    let half_day = std::time::Duration::from_secs(12 * 3600);

    let run = |conn: KdbConnection| -> Result<usize> {
        let stream = kdb_read_cached::<TestTradeCached, _>(
            conn,
            half_day,
            cache_dir.to_path_buf(),
            |within, date, _| slice_query(date, within.0, within.1),
        );
        let collected = stream.collapse().collect();
        collected.clone().run(
            RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
            RunFor::Duration(std::time::Duration::from_secs(2 * 86400)),
        )?;
        Ok(collected.peek_value().len())
    };

    // Populate all 4 slices.
    with_test_data(4, 2, true, |n, conn| {
        let count = run(conn.clone())?;
        assert_eq!(count, n, "First run should read all rows from KDB");

        // Delete one cache file to simulate a partial cache.
        let victim = std::fs::read_dir(&cache_dir)?
            .filter_map(|e| e.ok())
            .find(|e| e.path().extension().map(|x| x == "cache").unwrap_or(false))
            .expect("should have cache files")
            .path();
        std::fs::remove_file(&victim)?;

        // Second run: 3 cache hits + 1 miss fetched from KDB.
        let count2 = run(conn)?;
        assert_eq!(count2, n, "Partial-cache run should still return all rows");

        Ok(())
    })?;

    // All 4 slices are now cached — closed port succeeds.
    let closed = KdbConnection::new("localhost", 59999);
    let n = run(closed)?;
    assert!(
        n > 0,
        "All slices cached: closed port should not be dialled"
    );

    std::fs::remove_dir_all(&cache_dir).ok();
    Ok(())
}
