//! Per-product render cache persisted as `.render-cache.json` at the
//! root of `build/<product>/`. The cache keeps the cache-hit path
//! byte-identical to the cache-miss path: when the recorded hash for a
//! skill matches and the output file is still on disk, render leaves
//! the file alone; otherwise the skill is re-rendered and overwritten.
//!
//! Serialization uses [`BTreeMap`] so the on-disk file is byte-stable
//! across runs (the cache-hit equality test depends on that).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub const CACHE_FILE: &str = ".render-cache.json";
pub const CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderCache {
    pub schema_version: u32,
    pub skills: BTreeMap<String, CacheEntry>,
}

impl RenderCache {
    pub fn empty() -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            skills: BTreeMap::new(),
        }
    }

    /// Load the cache from `path`, returning an empty cache when the
    /// file is missing or unreadable. An unparsable cache file is
    /// silently treated as empty so a corrupted cache forces a full
    /// re-render instead of failing the run.
    pub fn load_or_empty(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(body) => serde_json::from_str(&body).unwrap_or_else(|_| Self::empty()),
            Err(_) => Self::empty(),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let body = serde_json::to_string_pretty(self).expect("RenderCache serializes");
        fs::write(path, body.as_bytes())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheEntry {
    pub hash: String,
    pub output: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn round_trips_through_disk() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".render-cache.json");
        let mut cache = RenderCache::empty();
        cache.skills.insert(
            "market.favorites".to_string(),
            CacheEntry {
                hash: "sha256:deadbeef".to_string(),
                output: "skills/sample/SKILL.md".to_string(),
            },
        );
        cache.save(&path).unwrap();
        let loaded = RenderCache::load_or_empty(&path);
        assert_eq!(loaded, cache);
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("does-not-exist.json");
        let loaded = RenderCache::load_or_empty(&path);
        assert!(loaded.skills.is_empty());
        assert_eq!(loaded.schema_version, CACHE_SCHEMA_VERSION);
    }

    #[test]
    fn unparsable_file_loads_as_empty() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".render-cache.json");
        fs::write(&path, "this is not json").unwrap();
        let loaded = RenderCache::load_or_empty(&path);
        assert!(loaded.skills.is_empty());
    }

    #[test]
    fn save_emits_sorted_keys_for_byte_stability() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".render-cache.json");
        let mut cache = RenderCache::empty();
        cache.skills.insert(
            "zeta.last".to_string(),
            CacheEntry {
                hash: "sha256:1".to_string(),
                output: "z".to_string(),
            },
        );
        cache.skills.insert(
            "alpha.first".to_string(),
            CacheEntry {
                hash: "sha256:2".to_string(),
                output: "a".to_string(),
            },
        );
        cache.save(&path).unwrap();
        let body = fs::read_to_string(&path).unwrap();
        // BTreeMap serialization → alpha appears before zeta.
        let alpha_idx = body.find("alpha.first").unwrap();
        let zeta_idx = body.find("zeta.last").unwrap();
        assert!(alpha_idx < zeta_idx);
    }
}
