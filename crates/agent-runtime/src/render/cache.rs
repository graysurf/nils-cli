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
/// Separate cache file for the optional agents render surface. Kept
/// distinct from `CACHE_FILE` so the agents loop and the skills loop
/// never reconcile against each other's entries (a shared cache would
/// make each surface delete the other's outputs on save).
pub const AGENTS_CACHE_FILE: &str = ".render-cache-agents.json";
/// Bumped to 2 alongside the multi-file render landing (v0.14): cache
/// entries now record every file the skill wrote (`outputs: Vec<String>`)
/// so the renderer can surgically remove sibling files that disappear
/// from source on a subsequent run. Caches written by older binaries
/// silently load as empty and force a full re-render.
pub const CACHE_SCHEMA_VERSION: u32 = 2;

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
    /// file is missing or unreadable. An unparsable cache file or one
    /// with a `schema_version` that we don't know how to interpret is
    /// silently treated as empty so a corrupted or future-format cache
    /// forces a full re-render instead of failing the run or silently
    /// trusting cache entries from a different version.
    pub fn load_or_empty(path: &Path) -> Self {
        let Ok(body) = fs::read_to_string(path) else {
            return Self::empty();
        };
        let Ok(parsed) = serde_json::from_str::<Self>(&body) else {
            return Self::empty();
        };
        if parsed.schema_version != CACHE_SCHEMA_VERSION {
            return Self::empty();
        }
        parsed
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let body = serde_json::to_string_pretty(self).expect("RenderCache serializes");
        fs::write(path, body.as_bytes())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheEntry {
    pub hash: String,
    /// Every file path (relative to `build/<product>/`) that this skill
    /// wrote during the recorded render. Sorted lexicographically for
    /// byte-stable serialization; the SKILL leaf appears in the list
    /// alongside every sibling (`bin/...`, `scripts/...`,
    /// `references/...`). The renderer uses this set to remove stale
    /// files on cache miss without disturbing files owned by sibling
    /// skills that share the same `dirname(render_to)`.
    pub outputs: Vec<String>,
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
                outputs: vec!["skills/sample/SKILL.md".to_string()],
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

    /// A cache file written by a future agent-runtime with a newer
    /// `schema_version` should be ignored rather than half-trusted —
    /// the entries' shape might have changed.
    #[test]
    fn schema_version_mismatch_loads_as_empty() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".render-cache.json");
        fs::write(
            &path,
            r#"{"schema_version": 99, "skills": {"old.skill": {"hash": "sha256:0", "outputs": ["x"]}}}"#,
        )
        .unwrap();
        let loaded = RenderCache::load_or_empty(&path);
        assert!(loaded.skills.is_empty());
        assert_eq!(loaded.schema_version, CACHE_SCHEMA_VERSION);
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
                outputs: vec!["z".to_string()],
            },
        );
        cache.skills.insert(
            "alpha.first".to_string(),
            CacheEntry {
                hash: "sha256:2".to_string(),
                outputs: vec!["a".to_string()],
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
