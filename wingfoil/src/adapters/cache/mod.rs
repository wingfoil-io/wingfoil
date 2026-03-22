pub mod file_cache;
pub use file_cache::FileCache;

use anyhow::Result;
use std::path::PathBuf;

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
/// Pass this to [`kdb_read_cached`] in place of a bare path. `max_size_bytes`
/// controls the total on-disk size of `.cache` files in `folder`; when a new
/// file would push the total over the limit, the least-recently-used files are
/// deleted first (LRU eviction).
///
/// Set `max_size_bytes` to `u64::MAX` for an unbounded cache.
///
/// # Example
/// ```ignore
/// let config = CacheConfig::new("/tmp/my-backtest-cache", 512 * 1024 * 1024); // 512 MiB cap
/// let stream = kdb_read_cached::<Trade, _>(conn, period, config, query_fn);
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(key.0, "5899c93491e25e68");
    }

    #[test]
    fn test_cache_config_clear() {
        let dir = std::env::temp_dir().join(format!(
            "wingfoil_cache_config_clear_{}",
            std::process::id()
        ));
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
}
