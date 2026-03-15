pub mod file_cache;
pub use file_cache::FileCache;

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
}
