//! A file-backed result cache for time-sliced query readers — the
//! wingfoil-next port of the classic `wingfoil::adapters::cache` module onto the
//! Op-pattern crate.
//!
//! This is a **pure utility** adapter (no graph edge): a stable, query-keyed,
//! LRU-evicting on-disk cache of `Vec<(NanoTime, T)>` result batches, used by
//! caller-parameterised historical readers (classic `kdb_read_cached`) to avoid
//! re-fetching a slice that was already downloaded. It is *not* a source or
//! sink op — it exposes plain (async) `get`/`put` methods a reader calls from
//! its own producer path.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`stats`](crate::stats)
//! pattern, the module is *not* in the [`prelude`](crate::prelude) and is gated
//! behind the `cache` feature (it pulls in `sha2`, `bincode`, `serde`, and
//! tokio's `fs`). Bring in what you need explicitly:
//! `use wingfoil_next::adapters::cache::{CacheConfig, CacheKey, FileCache};`.
//!
//! - [`CacheKey`] — opaque, stable SHA-256 key derived from `[host, port,
//!   query]`.
//! - [`CacheConfig`] — cache directory + size cap, with a [`CacheConfig::clear`]
//!   helper.
//! - [`FileCache`] — the async `get`/`put` store with LRU eviction.
//!
//! Every public capability of the classic module is preserved, so the classic
//! unit tests port across verbatim (`tests/cache_adapter.rs`). The one
//! behavioural deviation is cosmetic: next's `FileCache` log messages drop the
//! classic "KDB " prefix (the cache is not kdb-specific in next) — deviation
//! register D7. The only surface change is the crate path (`wingfoil::NanoTime`
//! for the time type).

use anyhow::Result;
use log::info;
use std::path::PathBuf;
use std::time::SystemTime;
use wingfoil::NanoTime;

/// Opaque cache key derived from a query string.
///
/// Built from `[host, port_str, query]` — the query string is the single source
/// of truth for what was fetched (it already embeds time bounds). Using a stable
/// SHA-256 digest avoids the toolchain-version instability of `DefaultHasher`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey(pub(crate) String);

impl CacheKey {
    /// `parts` should be `[host, port_str, query_string]`.
    pub fn from_parts(parts: &[&str]) -> Self {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        for p in parts {
            h.update(p.as_bytes());
            h.update(b"\0"); // separator so ["ab","c"] != ["a","bc"]
        }
        let full = format!("{:x}", h.finalize());
        Self(full[..16].to_string()) // first 16 hex chars (64 bits) of SHA-256
    }
}

/// Configuration for a file-based cache directory.
///
/// Pass this to a cached reader in place of a bare path. `max_size_bytes`
/// controls the total on-disk size of `.cache` files in `folder`; when a new
/// file would push the total over the limit, the least-recently-used files are
/// deleted first (LRU eviction).
///
/// Set `max_size_bytes` to `u64::MAX` for an unbounded cache.
///
/// # Example
/// ```ignore
/// let config = CacheConfig::new("/tmp/my-backtest-cache", 512 * 1024 * 1024); // 512 MiB cap
/// let cache = FileCache::<Trade>::new(config);
/// ```
#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub folder: PathBuf,
    pub max_size_bytes: u64,
}

impl CacheConfig {
    /// Create a new `CacheConfig` with the given folder and maximum total cache
    /// size in bytes. Use `u64::MAX` for an unbounded cache.
    pub fn new(folder: impl Into<PathBuf>, max_size_bytes: u64) -> Self {
        Self {
            folder: folder.into(),
            max_size_bytes,
        }
    }

    /// Delete all `.cache` files inside [`Self::folder`].
    ///
    /// Returns `Ok(())` if the folder does not exist (nothing to clear).
    /// Errors from individual file deletions are collected and returned as a
    /// single combined error; other files in the folder are left untouched.
    pub fn clear(&self) -> Result<()> {
        let entries = match std::fs::read_dir(&self.folder) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let mut errors: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "cache")
                && let Err(e) = std::fs::remove_file(&path)
            {
                errors.push(format!("{}: {}", path.display(), e));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "cache clear errors:\n{}",
                errors.join("\n")
            ))
        }
    }
}

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
    /// eviction in [`put`](Self::put) treats it as recently used.
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

        info!("cache hit: {}", path.display());
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
        let mut buf = format!("{query}\n").into_bytes();
        buf.extend(bincode::serialize(&serializable)?);
        tokio::fs::write(&tmp, &buf).await?;

        // Evict LRU files if the cache would exceed the size limit.
        if self.config.max_size_bytes < u64::MAX {
            self.evict_lru(buf.len() as u64, &path).await;
        }

        tokio::fs::rename(&tmp, &path).await?;

        info!("cache write: {}", path.display());
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
                    info!("cache evict (LRU): {}", p.display());
                    freed += sz;
                }
                Err(e) => log::warn!("cache evict error {}: {}", p.display(), e),
            }
        }
    }
}
