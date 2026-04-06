use super::{CacheConfig, CacheKey};
use crate::time::NanoTime;
use anyhow::Result;
use log::info;
use std::path::PathBuf;
use std::time::SystemTime;

/// `PhantomData<fn() -> T>` is always `Send + Sync` regardless of `T`, which is
/// correct here because `FileCache<T>` never actually stores a `T` value — the
/// type parameter only appears in the bounds of the async `get`/`put` methods.
pub struct FileCache<T> {
    config: CacheConfig,
    _phantom: std::marker::PhantomData<fn() -> T>,
}

impl<T> FileCache<T> {
    pub fn new(config: CacheConfig) -> Self {
        Self {
            config,
            _phantom: std::marker::PhantomData,
        }
    }

    fn path(&self, key: &CacheKey) -> PathBuf {
        self.config.folder.join(format!("{}.cache", key.0))
    }

    /// Look up a cached result. Returns `None` on a cache miss, `Err` if the
    /// file exists but is corrupt or unreadable.
    ///
    /// On a cache hit the file's modification time is updated so that LRU
    /// eviction in [`put`] treats it as recently used.
    pub async fn get(&self, key: &CacheKey) -> Result<Option<Vec<(NanoTime, T)>>>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        let path = self.path(key);
        let bytes = match tokio::fs::read(&path).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
            Ok(b) => b,
        };
        // Skip the newline-terminated query header written by `put`.
        let payload = bytes
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| &bytes[i + 1..])
            .ok_or_else(|| anyhow::anyhow!("cache file missing header newline"))?;
        let raw: Vec<(u64, T)> = bincode::deserialize(payload)?;

        // Touch mtime so LRU eviction treats this file as recently used.
        // We rewrite the unchanged bytes; any IO error is silently ignored
        // since the data was already read successfully.
        let _ = tokio::fs::write(&path, &bytes).await;

        info!("KDB cache hit: {}", path.display());
        Ok(Some(
            raw.into_iter()
                .map(|(ns, t)| (NanoTime::new(ns), t))
                .collect(),
        ))
    }

    /// Write a result to cache atomically (write to `.tmp`, then rename),
    /// evicting least-recently-used `.cache` files when the total on-disk size
    /// would exceed [`CacheConfig::max_size_bytes`].
    ///
    /// The file begins with the query string followed by `\n`, then the
    /// bincode-encoded payload. This makes cache files self-documenting:
    /// `head -1 <hex>.cache` shows the exact query that produced the file.
    ///
    /// **Eviction:** before the atomic rename, the folder is scanned for
    /// `*.cache` files. Files are sorted by modification time (oldest first)
    /// and deleted until the combined size of existing files plus the new file
    /// fits within `max_size_bytes`. Errors from individual deletions are
    /// logged but do not abort the write.
    ///
    /// **Concurrent writes:** the `.tmp` path is `<hex>.tmp`, shared by all
    /// writers for the same key. If two processes race on a cache miss for the
    /// same slice, their in-flight `.tmp` writes may clobber each other. The
    /// final `rename` is atomic so the `.cache` file is never left in a torn
    /// state, but one writer's serialization work is silently discarded. For
    /// the intended use-case (single backtesting process) this is harmless.
    pub async fn put(&self, key: &CacheKey, query: &str, data: &[(NanoTime, T)]) -> Result<()>
    where
        T: serde::Serialize,
    {
        let path = self.path(key);
        let tmp = path.with_extension("tmp");

        let serializable: Vec<(u64, &T)> = data.iter().map(|(t, v)| (u64::from(*t), v)).collect();
        let mut buf = format!("{}\n", query).into_bytes();
        buf.extend(bincode::serialize(&serializable)?);
        tokio::fs::write(&tmp, &buf).await?;

        // Evict LRU files if the cache would exceed the size limit.
        if self.config.max_size_bytes < u64::MAX {
            self.evict_lru(buf.len() as u64, &path).await;
        }

        tokio::fs::rename(&tmp, &path).await?;

        info!("KDB cache write: {}", path.display());
        Ok(())
    }

    /// Scan `folder` for `.cache` files and delete the oldest (by mtime) until
    /// `existing_total + new_size <= max_size_bytes`. `exclude` is the path
    /// about to be written — it must not be deleted even if it already exists.
    async fn evict_lru(&self, new_size: u64, exclude: &std::path::Path) {
        let mut entries = match tokio::fs::read_dir(&self.config.folder).await {
            Ok(e) => e,
            Err(_) => return,
        };

        let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "cache")
                && p != exclude
                && let Ok(meta) = entry.metadata().await
            {
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                files.push((p, meta.len(), mtime));
            }
        }

        // Oldest mtime first → evict least-recently-used.
        files.sort_by_key(|(_, _, mtime)| *mtime);

        let total_existing: u64 = files.iter().map(|(_, sz, _)| sz).sum();
        let needed = (total_existing + new_size).saturating_sub(self.config.max_size_bytes);
        if needed == 0 {
            return;
        }

        let mut freed: u64 = 0;
        for (p, sz, _) in &files {
            if freed >= needed {
                break;
            }
            match tokio::fs::remove_file(p).await {
                Ok(()) => {
                    info!("KDB cache evict (LRU): {}", p.display());
                    freed += sz;
                }
                Err(e) => log::warn!("KDB cache evict error {}: {}", p.display(), e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::cache::CacheKey;
    use std::time::Duration;
    use std::time::SystemTime;

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
        let dir = std::env::temp_dir().join(format!("wingfoil_cache_test_{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("wingfoil_cache_miss_{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let cache = unbounded_cache(&dir);
        let key = test_key("nonexistent");

        let result = cache.get(&key).await.unwrap();
        assert!(result.is_none());

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn test_atomic_write_no_tmp_leftover() {
        let dir =
            std::env::temp_dir().join(format!("wingfoil_cache_atomic_{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let cache = unbounded_cache(&dir);
        let key = test_key("atomic");

        let data = vec![(NanoTime::new(1_000), 42.0_f64)];
        cache.put(&key, "q", &data).await.unwrap();

        let tmp = dir.join(format!("{}.tmp", key.0));
        assert!(
            !tmp.exists(),
            ".tmp file should not exist after successful put"
        );

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn test_corrupt_file_returns_err() {
        let dir =
            std::env::temp_dir().join(format!("wingfoil_cache_corrupt_{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let cache = unbounded_cache(&dir);
        let key = test_key("corrupt");

        // Write a file that has a newline but garbage bincode after it
        let path = dir.join(format!("{}.cache", key.0));
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
        let dir = std::env::temp_dir().join(format!("wingfoil_cache_lru_{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let data_a = vec![(NanoTime::new(1_000), 1.0_f64)];
        let data_b = vec![(NanoTime::new(2_000), 2.0_f64)];
        let data_c = vec![(NanoTime::new(3_000), 3.0_f64)];

        // Measure how large one serialized entry is by writing with an unbounded cache.
        let probe = unbounded_cache(&dir);
        let key_a = test_key("lru_a");
        probe.put(&key_a, "q_a", &data_a).await.unwrap();
        let file_size = tokio::fs::metadata(dir.join(format!("{}.cache", key_a.0)))
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
            dir.join(format!("{}.cache", key_a.0)).exists(),
            "key_a should still exist"
        );
        assert!(
            dir.join(format!("{}.cache", key_b.0)).exists(),
            "key_b should exist"
        );

        // Touch key_a via get() to make it the most recently used. This relies on `get()`
        // updating file mtime; we wait until the filesystem reports key_a is newer than key_b.
        let path_a = dir.join(format!("{}.cache", key_a.0));
        let path_b = dir.join(format!("{}.cache", key_b.0));
        let b_mtime = mtime(&path_b).await;
        ensure_mtime_newer(&path_a, b_mtime, || async {
            let _ = cache.get(&key_a).await.unwrap();
        })
        .await;

        // Write key_c — would exceed limit, so the LRU file (key_b, not key_a) is evicted.
        let key_c = test_key("lru_c");
        cache.put(&key_c, "q_c", &data_c).await.unwrap();

        assert!(
            !dir.join(format!("{}.cache", key_b.0)).exists(),
            "key_b (LRU) should have been evicted"
        );
        assert!(
            dir.join(format!("{}.cache", key_a.0)).exists(),
            "key_a (recently used) should survive"
        );
        assert!(
            dir.join(format!("{}.cache", key_c.0)).exists(),
            "key_c (just written) should exist"
        );

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }
}
