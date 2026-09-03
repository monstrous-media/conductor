// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Profile cache for fast profile switching (Issue #355 — R770: <50ms target).
//!
//! Pre-parses and validates profile configs so that profile switching avoids
//! filesystem I/O and re-validation on the hot path. Configs are cached after
//! first load and invalidated externally when the config watcher fires.
//!
//! # Design Decisions
//!
//! - **No filesystem I/O in `get()`**: The hot path is a pure HashMap lookup.
//!   Staleness is handled by watcher-driven invalidation, not mtime checks.
//! - **Keys are normalized paths** (not canonicalized): Avoids syscalls and
//!   handles deleted files correctly.
//! - **Eviction is "least-hit"** (not true LRU): Acceptable for ~16 entries.

use conductor_core::config::Config;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// A cached profile entry: parsed config with freshness metadata.
#[derive(Debug, Clone)]
struct CachedProfile {
    /// Pre-parsed and validated config
    config: Config,
    /// File modification time at cache time (for freshness check on inactive profiles)
    mtime: Option<std::time::SystemTime>,
    /// Number of cache hits (for eviction priority)
    hits: u64,
}

/// Profile cache with watcher-driven invalidation.
///
/// # Thread Safety
///
/// This struct is **NOT thread-safe**. It is designed to be owned exclusively by
/// `EngineManager` and accessed only from the single-threaded `tokio::select!`
/// command loop. Do not share across tasks or wrap in `Arc` without adding
/// synchronization.
///
/// # Eviction
///
/// Uses a "least-hit" eviction strategy (not true LRU). When the cache is full,
/// the entry with the fewest hits is evicted. This is acceptable for the expected
/// cache size (~16 entries) but is not optimal for larger caches.
#[derive(Debug)]
pub struct ProfileCache {
    entries: HashMap<PathBuf, CachedProfile>,
    /// Maximum number of cached profiles.
    max_entries: usize,
    /// Lifetime cache hits (survives eviction).
    total_hits: u64,
    /// Lifetime cache misses (for hit-ratio reporting).
    total_misses: u64,
}

/// Result of a cache lookup.
#[derive(Debug)]
#[must_use]
pub enum CacheLookup {
    /// Cache hit — config is pre-validated and ready to use.
    /// Boxed to reduce enum size difference between variants.
    Hit(Box<Config>),
    /// Cache miss — caller must load from disk
    Miss,
}

/// Metrics about the profile cache state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct ProfileCacheMetrics {
    pub cached_profiles: usize,
    pub max_entries: usize,
    pub total_hits: u64,
    pub total_misses: u64,
}

/// Normalize a path for use as cache key. Resolves to absolute (using cwd)
/// and cleans `.` and `..` components without hitting the filesystem for
/// symlink resolution. This ensures consistent keys regardless of whether
/// callers pass relative or absolute paths.
///
/// Note: This does NOT resolve symlinks (that requires `canonicalize()`).
fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;

    // Make absolute first if relative, so keys are always consistent
    let absolute = if path.is_relative() {
        if let Ok(cwd) = std::env::current_dir() {
            cwd.join(path)
        } else {
            path.to_path_buf()
        }
    } else {
        path.to_path_buf()
    };

    // Track depth after root/prefix so we don't pop past the filesystem root.
    let mut normalized = PathBuf::new();
    let mut depth_after_root: usize = 0;

    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if depth_after_root > 0 {
                    normalized.pop();
                    depth_after_root -= 1;
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                normalized.push(component);
                depth_after_root = 0;
            }
            Component::Normal(_) => {
                normalized.push(component);
                depth_after_root += 1;
            }
        }
    }
    normalized
}

impl ProfileCache {
    /// Create a new profile cache with the given capacity.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            max_entries: max_entries.max(1),
            total_hits: 0,
            total_misses: 0,
        }
    }

    /// Look up a profile config by path.
    ///
    /// Performs a single `stat()` call to verify the cached mtime still matches,
    /// which catches edits to inactive profiles not covered by the config watcher.
    /// Returns `Miss` if the file was modified since caching.
    pub fn get(&mut self, path: &Path) -> CacheLookup {
        let key = normalize_path(path);

        if let Some(entry) = self.entries.get_mut(&key) {
            // Freshness check: verify file mtime hasn't changed since caching.
            // This catches edits to non-active profiles that the watcher doesn't cover.
            if let Some(cached_mtime) = entry.mtime
                && let Ok(current_mtime) = std::fs::metadata(&key)
                    .or_else(|_| std::fs::metadata(path))
                    .and_then(|m| m.modified())
                && current_mtime != cached_mtime
            {
                debug!(path = %key.display(), "Profile cache stale (mtime changed)");
                self.entries.remove(&key);
                self.total_misses += 1;
                return CacheLookup::Miss;
            }

            entry.hits += 1;
            self.total_hits += 1;
            debug!(path = %key.display(), hits = entry.hits, "Profile cache hit");
            CacheLookup::Hit(Box::new(entry.config.clone()))
        } else {
            self.total_misses += 1;
            CacheLookup::Miss
        }
    }

    /// Insert a validated config into the cache.
    pub fn insert(&mut self, path: &Path, config: Config) {
        let key = normalize_path(path);

        // Evict least-hit entry if at capacity
        if self.entries.len() >= self.max_entries
            && !self.entries.contains_key(&key)
            && let Some(evict_key) = self
                .entries
                .iter()
                .min_by_key(|(_, v)| v.hits)
                .map(|(k, _)| k.clone())
        {
            debug!(path = %evict_key.display(), "Evicting least-hit profile from cache");
            self.entries.remove(&evict_key);
        }

        // Capture file mtime for freshness checks on inactive profiles
        let mtime = std::fs::metadata(&key)
            .or_else(|_| std::fs::metadata(path))
            .and_then(|m| m.modified())
            .ok();

        self.entries.insert(
            key,
            CachedProfile {
                config,
                mtime,
                hits: 0,
            },
        );
    }

    /// Invalidate a specific cache entry (e.g., after file change or deletion).
    ///
    /// Tries both the normalized path and the raw path to handle cases where
    /// the file has been deleted (canonicalize would fail differently now).
    pub fn invalidate(&mut self, path: &Path) {
        let key = normalize_path(path);
        let mut removed = self.entries.remove(&key).is_some();

        // Also try raw path in case normalize_path resolved differently at insert time
        if !removed {
            removed = self.entries.remove(path).is_some();
        }

        // Fallback: scan by filename + parent directory match.
        // This handles edge cases where normalization resolved differently
        // at insert vs invalidation time, without overbroad matching
        // (e.g. two dirs both containing `default.toml`).
        if !removed && let (Some(filename), Some(parent)) = (path.file_name(), path.parent()) {
            let matching_keys: Vec<PathBuf> = self
                .entries
                .keys()
                .filter(|k| k.file_name() == Some(filename) && k.parent() == Some(parent))
                .cloned()
                .collect();
            for k in matching_keys {
                debug!(path = %k.display(), "Invalidating by filename+parent match");
                self.entries.remove(&k);
            }
        }
    }

    /// Clear the entire cache.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Pre-load all `.toml` profiles from a directory.
    ///
    /// Profiles that fail to parse are logged and skipped (don't block startup).
    /// This should be called from startup context where brief blocking is acceptable.
    pub fn preload_directory(&mut self, dir: &Path) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                debug!(dir = %dir.display(), error = %e, "Cannot read profiles directory for preload");
                return;
            }
        };

        let mut loaded = 0u32;
        let mut failed = 0u32;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                match self.load_and_cache(&path) {
                    Ok(_) => loaded += 1,
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "Failed to preload profile");
                        failed += 1;
                    }
                }
            }
        }

        if loaded > 0 || failed > 0 {
            info!(loaded, failed, dir = %dir.display(), "Profile cache preloaded");
        }
    }

    /// Load a single profile from disk, validate, and insert into cache.
    fn load_and_cache(&mut self, path: &Path) -> Result<(), String> {
        let path_str = path.to_str().ok_or_else(|| "Non-UTF8 path".to_string())?;
        let config =
            Config::load(path_str).map_err(|e| format!("Failed to load {}: {}", path_str, e))?;

        // Run schema validation.
        //
        // #891: validation warnings here flood startup logs. The bootstrap
        // re-validates every cached profile (one INFO line per profile may
        // already be `loaded=22`); raising each profile's per-mapping warning
        // to `warn!` produces 22 × N lines of duplicate noise. The active
        // profile's warnings are already emitted via the main-config
        // validation path, so emit profile-cache validation warnings at
        // `debug!`. Errors stay at `warn!` since a failed-to-validate profile
        // is a hard problem worth surfacing.
        let report = conductor_core::config::validator::validate_config(&config);
        for finding in &report.warnings {
            debug!(
                path = %path_str,
                "Profile validation warning: {}: {}",
                finding.path, finding.message
            );
        }
        if !report.is_valid() {
            for finding in &report.errors {
                warn!(
                    path = %path_str,
                    "Profile validation error: {}: {}",
                    finding.path, finding.message
                );
            }
            return Err(format!(
                "Validation failed with {} error(s)",
                report.errors.len()
            ));
        }

        self.insert(path, config);
        Ok(())
    }

    /// Get cache metrics for status reporting.
    pub fn metrics(&self) -> ProfileCacheMetrics {
        ProfileCacheMetrics {
            cached_profiles: self.entries.len(),
            max_entries: self.max_entries,
            total_hits: self.total_hits,
            total_misses: self.total_misses,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_valid_toml(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        // Minimal valid conductor config
        writeln!(f, "[[modes]]").unwrap();
        writeln!(f, "name = \"Default\"").unwrap();
        writeln!(f, "mappings = []").unwrap();
        path
    }

    #[test]
    fn test_cache_miss_on_empty() {
        let mut cache = ProfileCache::new(10);
        assert!(matches!(
            cache.get(Path::new("/nonexistent.toml")),
            CacheLookup::Miss
        ));
        assert_eq!(cache.metrics().total_misses, 1);
    }

    #[test]
    fn test_cache_hit_after_insert() {
        let tmp = TempDir::new().unwrap();
        let path = write_valid_toml(tmp.path(), "test.toml");
        let config = Config::load(path.to_str().unwrap()).unwrap();

        let mut cache = ProfileCache::new(10);
        cache.insert(&path, config.clone());

        match cache.get(&path) {
            CacheLookup::Hit(cached) => {
                assert_eq!(cached.modes.len(), config.modes.len());
            }
            CacheLookup::Miss => panic!("Expected cache hit"),
        }
    }

    #[test]
    fn test_invalidate_removes_entry() {
        let tmp = TempDir::new().unwrap();
        let path = write_valid_toml(tmp.path(), "test.toml");
        let config = Config::load(path.to_str().unwrap()).unwrap();

        let mut cache = ProfileCache::new(10);
        cache.insert(&path, config);
        assert!(matches!(cache.get(&path), CacheLookup::Hit(_)));

        cache.invalidate(&path);
        assert!(matches!(cache.get(&path), CacheLookup::Miss));
    }

    #[test]
    fn test_invalidate_after_deletion() {
        let tmp = TempDir::new().unwrap();
        let path = write_valid_toml(tmp.path(), "test.toml");
        let config = Config::load(path.to_str().unwrap()).unwrap();

        let mut cache = ProfileCache::new(10);
        cache.insert(&path, config);

        // Delete the file — canonicalize will fail in invalidate
        std::fs::remove_file(&path).unwrap();
        cache.invalidate(&path);

        // Re-create the file for get() normalization to work
        write_valid_toml(tmp.path(), "test.toml");
        assert!(matches!(cache.get(&path), CacheLookup::Miss));
    }

    #[test]
    fn test_cache_eviction_at_capacity() {
        let tmp = TempDir::new().unwrap();
        let mut cache = ProfileCache::new(2);

        let p1 = write_valid_toml(tmp.path(), "a.toml");
        let p2 = write_valid_toml(tmp.path(), "b.toml");
        let p3 = write_valid_toml(tmp.path(), "c.toml");

        let c1 = Config::load(p1.to_str().unwrap()).unwrap();
        let c2 = Config::load(p2.to_str().unwrap()).unwrap();
        let c3 = Config::load(p3.to_str().unwrap()).unwrap();

        cache.insert(&p1, c1);
        cache.insert(&p2, c2);

        // Hit p1 to give it higher hit count
        let _ = cache.get(&p1);

        // Insert p3 — should evict p2 (least hits)
        cache.insert(&p3, c3);

        assert!(matches!(cache.get(&p1), CacheLookup::Hit(_)));
        assert!(matches!(cache.get(&p2), CacheLookup::Miss));
        assert!(matches!(cache.get(&p3), CacheLookup::Hit(_)));
    }

    #[test]
    fn test_preload_directory() {
        let tmp = TempDir::new().unwrap();
        write_valid_toml(tmp.path(), "one.toml");
        write_valid_toml(tmp.path(), "two.toml");
        // Non-toml file should be ignored
        std::fs::write(tmp.path().join("readme.md"), "# Hi").unwrap();

        let mut cache = ProfileCache::new(10);
        cache.preload_directory(tmp.path());

        assert_eq!(cache.metrics().cached_profiles, 2);
    }

    #[test]
    fn test_metrics() {
        let tmp = TempDir::new().unwrap();
        let path = write_valid_toml(tmp.path(), "test.toml");
        let config = Config::load(path.to_str().unwrap()).unwrap();

        let mut cache = ProfileCache::new(5);
        cache.insert(&path, config);
        let _ = cache.get(&path); // hit
        let _ = cache.get(&path); // hit
        let _ = cache.get(Path::new("/nope.toml")); // miss

        let m = cache.metrics();
        assert_eq!(m.cached_profiles, 1);
        assert_eq!(m.max_entries, 5);
        assert_eq!(m.total_hits, 2);
        assert_eq!(m.total_misses, 1);
    }

    #[test]
    fn test_max_entries_at_least_one() {
        let cache = ProfileCache::new(0);
        assert_eq!(cache.max_entries, 1);
    }

    #[test]
    fn test_get_is_pure_lookup_no_fs() {
        // Verify get() works even when the file doesn't exist on disk,
        // as long as the entry was previously inserted. normalize_path
        // uses component-based normalization (no canonicalize), so the
        // key is deterministic regardless of file existence.
        let tmp = TempDir::new().unwrap();
        let path = write_valid_toml(tmp.path(), "test.toml");
        let config = Config::load(path.to_str().unwrap()).unwrap();

        let mut cache = ProfileCache::new(10);
        cache.insert(&path, config);

        let initial_hits = cache.total_hits;
        let initial_misses = cache.total_misses;

        // Delete the file — get() should still return Hit from cache
        std::fs::remove_file(&path).unwrap();

        let result = cache.get(&path);
        match result {
            CacheLookup::Hit(_) => {
                assert_eq!(cache.total_hits, initial_hits + 1);
                assert_eq!(cache.total_misses, initial_misses);
            }
            CacheLookup::Miss => {
                panic!("expected cache hit after deletion; get() returned Miss");
            }
        }
    }

    #[test]
    fn test_stale_mtime_returns_miss() {
        let tmp = TempDir::new().unwrap();
        let path = write_valid_toml(tmp.path(), "test.toml");
        let config = Config::load(path.to_str().unwrap()).unwrap();

        let mut cache = ProfileCache::new(10);
        cache.insert(&path, config);

        // First get should be a hit
        assert!(matches!(cache.get(&path), CacheLookup::Hit(_)));

        // Modify the file (changes mtime)
        std::thread::sleep(std::time::Duration::from_millis(50));
        write_valid_toml(tmp.path(), "test.toml");

        // Second get should detect stale mtime and return miss
        assert!(matches!(cache.get(&path), CacheLookup::Miss));
        assert_eq!(cache.metrics().cached_profiles, 0); // entry was evicted
    }
}
