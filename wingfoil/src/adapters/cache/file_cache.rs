use super::CacheKey;
use crate::time::NanoTime;
use anyhow::Result;
use log::info;
use std::path::PathBuf;

/// `PhantomData<fn() -> T>` is always `Send + Sync` regardless of `T`, which is
/// correct here because `FileCache<T>` never actually stores a `T` value — the
/// type parameter only appears in the bounds of the async `get`/`put` methods.
pub struct FileCache<T> {
    cache_dir: PathBuf,
    _phantom: std::marker::PhantomData<fn() -> T>,
}

impl<T> FileCache<T> {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            _phantom: std::marker::PhantomData,
        }
    }

    fn path(&self, key: &CacheKey) -> PathBuf {
        self.cache_dir.join(format!("{}.cache", key.0))
    }

    /// Look up a cached result. Returns `None` on a cache miss, `Err` if the
    /// file exists but is corrupt or unreadable.
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
        info!("KDB cache hit: {}", path.display());
        Ok(Some(
            raw.into_iter()
                .map(|(ns, t)| (NanoTime::new(ns), t))
                .collect(),
        ))
    }

    /// Write a result to cache atomically (write to `.tmp`, then rename).
    ///
    /// The file begins with the query string followed by `\n`, then the
    /// bincode-encoded payload. This makes cache files self-documenting:
    /// `head -1 <hex>.cache` shows the exact query that produced the file.
    pub async fn put(&self, key: &CacheKey, query: &str, data: &[(NanoTime, T)]) -> Result<()>
    where
        T: serde::Serialize,
    {
        let path = self.path(key);
        let tmp = path.with_extension("tmp");

        let serializable: Vec<(u64, &T)> = data.iter().map(|(t, v)| (u64::from(*t), v)).collect();
        let mut buf = format!("{}\n", query).into_bytes();
        buf.extend(bincode::serialize(&serializable)?);
        tokio::fs::write(&tmp, buf).await?;
        tokio::fs::rename(&tmp, &path).await?;

        info!("KDB cache write: {}", path.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::cache::CacheKey;

    fn test_key(s: &str) -> CacheKey {
        CacheKey::from_parts(&["localhost", "5000", s])
    }

    #[tokio::test]
    async fn test_round_trip() {
        let dir = std::env::temp_dir().join(format!("wingfoil_cache_test_{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let cache = FileCache::<f64>::new(&dir);
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
        let cache = FileCache::<f64>::new(&dir);
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
        let cache = FileCache::<f64>::new(&dir);
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
        let cache = FileCache::<f64>::new(&dir);
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
}
