//! Aggregate result cache.
//!
//! Caches aggregate results keyed by a hash of the spec + drive snapshot
//! version. This avoids re-scanning millions of records when the same
//! preset is requested multiple times before the index changes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::finalize::AggregateResponse;

/// A time-limited aggregate cache.
///
/// Entries are keyed by a spec hash and automatically expire after a
/// configurable TTL. The cache is invalidated when the drive index
/// version changes.
#[derive(Debug)]
pub struct AggregateCache {
    /// Cache entries keyed by spec hash.
    entries: Mutex<HashMap<u64, CacheEntry>>,
    /// Time-to-live for cache entries.
    ttl: Duration,
    /// Drive index version at cache time (for invalidation).
    index_version: Mutex<u64>,
}

/// A single cache entry.
#[derive(Debug, Clone)]
struct CacheEntry {
    /// The cached response.
    response: AggregateResponse,
    /// When this entry was created.
    created: Instant,
    /// Drive index version when this was computed.
    index_version: u64,
}

impl AggregateCache {
    /// Create a new cache with the specified TTL.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
            index_version: Mutex::new(0),
        }
    }

    /// Create a cache with the default 60-second TTL.
    #[must_use]
    pub fn default_ttl() -> Self {
        Self::new(Duration::from_secs(60))
    }

    /// Set the current drive index version.
    ///
    /// When this changes, all existing cache entries are invalidated.
    pub fn set_index_version(&self, version: u64) {
        let mut current = self.index_version.lock().unwrap_or_else(|e| e.into_inner());
        if *current != version {
            *current = version;
            // Invalidate all entries.
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            entries.clear();
        }
    }

    /// Look up a cached result.
    ///
    /// Returns `None` if the entry is missing, expired, or belongs
    /// to a different index version.
    #[must_use]
    pub fn get(&self, spec_hash: u64) -> Option<AggregateResponse> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get(&spec_hash)?;

        // Check TTL.
        if entry.created.elapsed() > self.ttl {
            return None;
        }

        // Check index version.
        let current_version = *self.index_version.lock().unwrap_or_else(|e| e.into_inner());
        if entry.index_version != current_version {
            return None;
        }

        Some(entry.response.clone())
    }

    /// Insert a result into the cache.
    pub fn put(&self, spec_hash: u64, response: AggregateResponse) {
        let current_version = *self.index_version.lock().unwrap_or_else(|e| e.into_inner());
        let entry = CacheEntry {
            response,
            created: Instant::now(),
            index_version: current_version,
        };
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());

        // Evict expired entries first.
        entries.retain(|_, e| e.created.elapsed() <= self.ttl);

        entries.insert(spec_hash, entry);
    }

    /// Clear all cached entries.
    pub fn clear(&self) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.clear();
    }

    /// Number of entries currently in cache.
    #[must_use]
    pub fn len(&self) -> usize {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Compute a hash for a set of aggregate spec labels + kinds.
///
/// This is a simple hash function for cache keying — not cryptographic.
#[must_use]
pub fn hash_specs(specs_key: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    specs_key.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_put_and_get() {
        let cache = AggregateCache::default_ttl();
        let response = AggregateResponse { results: vec![] };
        let hash = hash_specs("test_key");

        cache.put(hash, response.clone());
        let cached = cache.get(hash);
        assert!(cached.is_some());
    }

    #[test]
    fn cache_miss_after_version_change() {
        let cache = AggregateCache::default_ttl();
        let response = AggregateResponse { results: vec![] };
        let hash = hash_specs("test_key");

        cache.put(hash, response);
        cache.set_index_version(1);

        let cached = cache.get(hash);
        assert!(cached.is_none());
    }

    #[test]
    fn cache_clear() {
        let cache = AggregateCache::default_ttl();
        let response = AggregateResponse { results: vec![] };
        cache.put(hash_specs("a"), response.clone());
        cache.put(hash_specs("b"), response);
        assert_eq!(cache.len(), 2);

        cache.clear();
        assert!(cache.is_empty());
    }
}
