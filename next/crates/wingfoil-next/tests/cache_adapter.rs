//! Parity tests for the `cache` adapter, ported verbatim from classic
//! `wingfoil::adapters::cache` (`mod.rs` + `file_cache.rs` test modules):
//! [`CacheKey`] stability/uniqueness, [`CacheConfig::clear`], and the
//! [`FileCache`] async round-trip / miss / atomic-write / corrupt-file / LRU
//! eviction behaviours.
//!
//! `CacheKey`/`CacheConfig`/`FileCache` are pure utilities (not graph ops), so —
//! like the classic tests — there are no graph runs and thus no tick times to
//! assert; parity is on the returned values (keys, cached batches, on-disk
//! eviction outcome). Temp directories are uniquely named per test (pid + an
//! atomic counter) so parallel tests never collide.

#![cfg(feature = "cache")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use wingfoil_next::NanoTime;
use wingfoil_next::adapters::cache::{CacheConfig, CacheKey, FileCache};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique temp directory path for a test (pid + atomic counter).
fn unique_dir(tag: &str) -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "wingfoil_next_cache_{tag}_{}_{n}",
        std::process::id()
    ))
}

// ---- CacheKey (from classic cache/mod.rs) --------------------------------

#[test]
fn test_key_uniqueness() {
    let k1 = CacheKey::from_parts(&["localhost", "5000", "select from trades where date=0"]);
    let k2 = CacheKey::from_parts(&["localhost", "5000", "select from trades where date=1"]);
    assert_ne!(k1, k2);
}

#[test]
fn test_key_same_input_same_output() {
    let k1 = CacheKey::from_parts(&["localhost", "5000", "select from trades"]);
    let k2 = CacheKey::from_parts(&["localhost", "5000", "select from trades"]);
    assert_eq!(k1, k2);
}

#[test]
fn test_key_separator_prevents_collision() {
    // ["ab", "c"] vs ["a", "bc"] must differ
    let k1 = CacheKey::from_parts(&["ab", "c", "q"]);
    let k2 = CacheKey::from_parts(&["a", "bc", "q"]);
    assert_ne!(k1, k2);
}

#[test]
fn test_key_stability() {
    // SHA-256 is deterministic across toolchain versions. Assert the exact
    // 16-char hex prefix so any accidental algorithm change is caught.
    let key = CacheKey::from_parts(&["localhost", "5000", "select from trades"]);
    assert_eq!(format!("{key:?}"), "CacheKey(\"5899c93491e25e68\")");
}

#[test]
fn test_cache_config_clear() {
    let dir = unique_dir("config_clear");
    std::fs::create_dir_all(&dir).unwrap();

    // Write some .cache files and a non-cache file
    std::fs::write(dir.join("a.cache"), b"data").unwrap();
    std::fs::write(dir.join("b.cache"), b"data").unwrap();
    std::fs::write(dir.join("other.txt"), b"keep").unwrap();

    let config = CacheConfig::new(&dir, u64::MAX);
    config.clear().unwrap();

    // .cache files gone, other file remains
    assert!(!dir.join("a.cache").exists());
    assert!(!dir.join("b.cache").exists());
    assert!(dir.join("other.txt").exists());

    // clear on non-existent folder is Ok
    let absent = CacheConfig::new(dir.join("nonexistent"), u64::MAX);
    absent.clear().unwrap();

    std::fs::remove_dir_all(&dir).unwrap();
}

// ---- FileCache (from classic cache/file_cache.rs) ------------------------

fn test_key(s: &str) -> CacheKey {
    CacheKey::from_parts(&["localhost", "5000", s])
}

fn unbounded_cache(dir: &std::path::Path) -> FileCache<f64> {
    FileCache::new(CacheConfig::new(dir, u64::MAX))
}

async fn mtime(path: &std::path::Path) -> SystemTime {
    tokio::fs::metadata(path)
        .await
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

async fn ensure_mtime_newer<FUT>(
    path: &std::path::Path,
    older_than: SystemTime,
    mut touch: impl FnMut() -> FUT,
) where
    FUT: std::future::Future<Output = ()>,
{
    // Some filesystems have coarse mtime resolution (e.g. 1s). If two writes land in the
    // same tick, the mtime can remain equal. To make the test robust, we:
    // 1) touch the file
    // 2) if it is still not newer, wait for a coarse tick and touch again
    for _ in 0..5 {
        touch().await;
        if mtime(path).await > older_than {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!(
        "expected mtime for {} to advance beyond {:?}",
        path.display(),
        older_than
    );
}

#[tokio::test]
async fn test_round_trip() {
    let dir = unique_dir("round_trip");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let cache = unbounded_cache(&dir);
    let key = test_key("round_trip");

    let data = vec![
        (NanoTime::new(1_000), 1.0_f64),
        (NanoTime::new(2_000), 2.0_f64),
    ];
    cache.put(&key, "select from t", &data).await.unwrap();
    let result = cache.get(&key).await.unwrap().unwrap();

    assert_eq!(result.len(), 2);
    assert_eq!(u64::from(result[0].0), 1_000);
    assert!((result[0].1 - 1.0).abs() < f64::EPSILON);
    assert_eq!(u64::from(result[1].0), 2_000);
    assert!((result[1].1 - 2.0).abs() < f64::EPSILON);

    tokio::fs::remove_dir_all(&dir).await.unwrap();
}

#[tokio::test]
async fn test_cache_miss() {
    let dir = unique_dir("miss");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let cache = unbounded_cache(&dir);
    let key = test_key("nonexistent");

    let result = cache.get(&key).await.unwrap();
    assert!(result.is_none());

    tokio::fs::remove_dir_all(&dir).await.unwrap();
}

#[tokio::test]
async fn test_atomic_write_no_tmp_leftover() {
    let dir = unique_dir("atomic");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let cache = unbounded_cache(&dir);
    let key = test_key("atomic");

    let data = vec![(NanoTime::new(1_000), 42.0_f64)];
    cache.put(&key, "q", &data).await.unwrap();

    let tmp = dir.join(format!("{}.tmp", cache_key_hex(&key)));
    assert!(
        !tmp.exists(),
        ".tmp file should not exist after successful put"
    );

    tokio::fs::remove_dir_all(&dir).await.unwrap();
}

#[tokio::test]
async fn test_corrupt_file_returns_err() {
    let dir = unique_dir("corrupt");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let cache = unbounded_cache(&dir);
    let key = test_key("corrupt");

    // Write a file that has a newline but garbage bincode after it
    let path = dir.join(format!("{}.cache", cache_key_hex(&key)));
    tokio::fs::write(&path, b"select from t\ngarbage bytes not valid bincode")
        .await
        .unwrap();

    let result = cache.get(&key).await;
    assert!(result.is_err(), "corrupt file should return Err");

    tokio::fs::remove_dir_all(&dir).await.unwrap();
}

/// When the cache is full, writing a new file evicts the oldest `.cache` file.
#[tokio::test]
async fn test_lru_eviction() {
    let dir = unique_dir("lru");
    tokio::fs::create_dir_all(&dir).await.unwrap();

    let data_a = vec![(NanoTime::new(1_000), 1.0_f64)];
    let data_b = vec![(NanoTime::new(2_000), 2.0_f64)];
    let data_c = vec![(NanoTime::new(3_000), 3.0_f64)];

    // Measure how large one serialized entry is by writing with an unbounded cache.
    let probe = unbounded_cache(&dir);
    let key_a = test_key("lru_a");
    probe.put(&key_a, "q_a", &data_a).await.unwrap();
    let file_size = tokio::fs::metadata(dir.join(format!("{}.cache", cache_key_hex(&key_a))))
        .await
        .unwrap()
        .len();

    // Allow exactly 2 files worth of space.
    let max = file_size * 2;
    let cache = FileCache::<f64>::new(CacheConfig::new(&dir, max));

    // key_a already exists. Write key_b — total = 2 × file_size, still within limit.
    let key_b = test_key("lru_b");
    cache.put(&key_b, "q_b", &data_b).await.unwrap();
    assert!(
        dir.join(format!("{}.cache", cache_key_hex(&key_a)))
            .exists(),
        "key_a should still exist"
    );
    assert!(
        dir.join(format!("{}.cache", cache_key_hex(&key_b)))
            .exists(),
        "key_b should exist"
    );

    // Touch key_a via get() to make it the most recently used. This relies on `get()`
    // updating file mtime; we wait until the filesystem reports key_a is newer than key_b.
    let path_a = dir.join(format!("{}.cache", cache_key_hex(&key_a)));
    let path_b = dir.join(format!("{}.cache", cache_key_hex(&key_b)));
    let b_mtime = mtime(&path_b).await;
    ensure_mtime_newer(&path_a, b_mtime, || async {
        let _ = cache.get(&key_a).await.unwrap();
    })
    .await;

    // Write key_c — would exceed limit, so the LRU file (key_b, not key_a) is evicted.
    let key_c = test_key("lru_c");
    cache.put(&key_c, "q_c", &data_c).await.unwrap();

    assert!(
        !dir.join(format!("{}.cache", cache_key_hex(&key_b)))
            .exists(),
        "key_b (LRU) should have been evicted"
    );
    assert!(
        dir.join(format!("{}.cache", cache_key_hex(&key_a)))
            .exists(),
        "key_a (recently used) should survive"
    );
    assert!(
        dir.join(format!("{}.cache", cache_key_hex(&key_c)))
            .exists(),
        "key_c (just written) should exist"
    );

    tokio::fs::remove_dir_all(&dir).await.unwrap();
}

/// The classic tests reach the key's inner hex string through the crate-private
/// `CacheKey.0` field; from an external test crate we recover it from the
/// `Debug` output (`CacheKey("<hex>")`).
fn cache_key_hex(key: &CacheKey) -> String {
    let s = format!("{key:?}");
    s.trim_start_matches("CacheKey(\"")
        .trim_end_matches("\")")
        .to_string()
}
