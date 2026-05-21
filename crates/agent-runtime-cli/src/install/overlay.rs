//! `.private/link-map.overrides.yaml` overlay parser. Plan 04 Sprint 1
//! Task 1.3.
//!
//! Merge rule per
//! `agent-runtime-kit/docs/source/inventory-target-architecture.md`
//! `### Overlay Merge Semantics`:
//!
//! > Per-entry override. Entry id is the key; the `.private/` entry
//! > **replaces** the tracked entry as a whole (no deep merge — avoids
//! > subtle drift in nested install metadata). `enabled: false` in an
//! > overlay entry drops that entry from the install plan.
//!
//! Overlay file is **optional**: when absent, [`LinkMapOverlay::load_optional`]
//! returns `Ok(None)` and the tracked link-map flows through unchanged. When
//! present, it MUST round-trip through the same schema gates as the tracked
//! link-map (schema_version match, `deny_unknown_fields`, per-kind required
//! / forbidden fields).

use super::link_map::{CommentStyle, EntryKind, LinkEntry, LinkMap, LinkMapError, SCHEMA_VERSION};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Conventional location of the overlay file, relative to the agent-runtime-kit
/// source root.
pub const OVERLAY_REL_PATH: &str = ".private/link-map.overrides.yaml";

fn default_enabled() -> bool {
    true
}

/// Single overlay entry. Mirrors [`LinkEntry`] plus an optional `enabled`
/// flag: when `enabled: false`, the matching tracked entry is dropped from
/// the merged plan and the other fields are ignored.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct OverlayEntry {
    pub id: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub kind: Option<EntryKind>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub surface: Option<String>,
    #[serde(default)]
    pub comment_style: Option<CommentStyle>,
    #[serde(default)]
    pub body_template: Option<String>,
}

/// Parsed overlay file.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LinkMapOverlay {
    pub schema_version: u32,
    #[serde(default)]
    pub entries: Vec<OverlayEntry>,
}

impl LinkMapOverlay {
    /// Read `<source_root>/.private/link-map.overrides.yaml` if it exists.
    /// Missing file is `Ok(None)`; any other failure (schema mismatch, parse
    /// error, duplicate id) bubbles as a [`LinkMapError`] so the install
    /// pipeline refuses to start rather than silently falling back to the
    /// tracked map.
    pub fn load_optional(source_root: &Path) -> Result<Option<Self>, LinkMapError> {
        Self::load_from(&source_root.join(OVERLAY_REL_PATH))
    }

    /// Read the overlay from an explicit path. `Ok(None)` when the file is
    /// absent; otherwise mirrors [`load_optional`] semantics. Used by
    /// `commands::install` when a caller passes `--overlay-path`.
    pub fn load_from(file: &Path) -> Result<Option<Self>, LinkMapError> {
        if !file.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(file).map_err(|source| LinkMapError::Io {
            file: file.to_path_buf(),
            source,
        })?;
        let parsed: LinkMapOverlay =
            serde_yaml_ng::from_str(&raw).map_err(|source| LinkMapError::Parse {
                file: file.to_path_buf(),
                source,
            })?;
        if parsed.schema_version != SCHEMA_VERSION {
            return Err(LinkMapError::SchemaVersion {
                file: file.to_path_buf(),
                expected: SCHEMA_VERSION,
                found: parsed.schema_version,
            });
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &parsed.entries {
            if !seen.insert(entry.id.clone()) {
                return Err(LinkMapError::DuplicateId {
                    file: file.to_path_buf(),
                    id: entry.id.clone(),
                });
            }
        }
        Ok(Some(parsed))
    }
}

/// Apply `overlay` to `base` per the per-entry-replace rule. Drops, replaces,
/// and additions all happen in-place against `base.entries` so the caller's
/// downstream plan builder consumes the merged result without knowing the
/// overlay existed.
pub fn apply(base: &mut LinkMap, overlay: &LinkMapOverlay) -> Result<(), LinkMapError> {
    for ov in &overlay.entries {
        if !ov.enabled {
            // Drop matching tracked entry. Silently no-op when missing: the
            // overlay author may have already removed the entry upstream.
            base.entries.retain(|e| e.id != ov.id);
            continue;
        }
        let merged = build_full_entry(ov)?;
        merged
            .validate()
            .map_err(|reason| LinkMapError::InvalidEntry {
                file: PathBuf::from(OVERLAY_REL_PATH),
                id: merged.id.clone(),
                reason,
            })?;
        match base.entries.iter().position(|e| e.id == ov.id) {
            Some(idx) => base.entries[idx] = merged,
            None => base.entries.push(merged),
        }
    }
    Ok(())
}

fn build_full_entry(ov: &OverlayEntry) -> Result<LinkEntry, LinkMapError> {
    // `enabled: true` overlay entries MUST declare a full entry shape; the
    // overlay rule is replacement, not deep-merge, so missing required fields
    // are a usage error.
    let kind = ov.kind.ok_or_else(|| LinkMapError::InvalidEntry {
        file: PathBuf::from(OVERLAY_REL_PATH),
        id: ov.id.clone(),
        reason: "overlay entry with enabled=true must declare `kind`".to_string(),
    })?;
    let destination = ov
        .destination
        .clone()
        .ok_or_else(|| LinkMapError::InvalidEntry {
            file: PathBuf::from(OVERLAY_REL_PATH),
            id: ov.id.clone(),
            reason: "overlay entry with enabled=true must declare `destination`".to_string(),
        })?;
    Ok(LinkEntry {
        id: ov.id.clone(),
        kind,
        source: ov.source.clone(),
        destination,
        recursive: ov.recursive,
        surface: ov.surface.clone(),
        comment_style: ov.comment_style,
        body_template: ov.body_template.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_overlay(root: &Path, body: &str) -> PathBuf {
        let dir = root.join(".private");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("link-map.overrides.yaml");
        fs::write(&file, body).unwrap();
        file
    }

    #[test]
    fn load_optional_returns_none_when_missing() {
        let tmp = TempDir::new().unwrap();
        let result = LinkMapOverlay::load_optional(tmp.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_optional_returns_some_when_present() {
        let tmp = TempDir::new().unwrap();
        write_overlay(
            tmp.path(),
            "\
schema_version: 1
entries:
  - id: reporting.skills-tree
    enabled: false
",
        );
        let overlay = LinkMapOverlay::load_optional(tmp.path()).unwrap().unwrap();
        assert_eq!(overlay.entries.len(), 1);
        assert!(!overlay.entries[0].enabled);
    }

    #[test]
    fn load_optional_rejects_unknown_field() {
        let tmp = TempDir::new().unwrap();
        write_overlay(
            tmp.path(),
            "\
schema_version: 1
entries:
  - id: x
    bogus: 1
",
        );
        let err = LinkMapOverlay::load_optional(tmp.path()).unwrap_err();
        assert!(matches!(err, LinkMapError::Parse { .. }));
    }

    #[test]
    fn load_optional_rejects_schema_mismatch() {
        let tmp = TempDir::new().unwrap();
        write_overlay(
            tmp.path(),
            "\
schema_version: 99
entries: []
",
        );
        let err = LinkMapOverlay::load_optional(tmp.path()).unwrap_err();
        assert!(matches!(err, LinkMapError::SchemaVersion { found: 99, .. }));
    }

    #[test]
    fn load_optional_rejects_duplicate_id() {
        let tmp = TempDir::new().unwrap();
        write_overlay(
            tmp.path(),
            "\
schema_version: 1
entries:
  - id: same
    enabled: false
  - id: same
    enabled: false
",
        );
        let err = LinkMapOverlay::load_optional(tmp.path()).unwrap_err();
        assert!(matches!(err, LinkMapError::DuplicateId { id, .. } if id == "same"));
    }

    fn tracked_entry(id: &str) -> LinkEntry {
        LinkEntry {
            id: id.to_string(),
            kind: EntryKind::SymlinkedFile,
            source: Some("build/a".to_string()),
            destination: "plugins/a".to_string(),
            recursive: false,
            surface: None,
            comment_style: None,
            body_template: None,
        }
    }

    #[test]
    fn apply_drops_disabled_entry() {
        let mut base = LinkMap {
            schema_version: 1,
            entries: vec![tracked_entry("keep"), tracked_entry("drop")],
        };
        let overlay = LinkMapOverlay {
            schema_version: 1,
            entries: vec![OverlayEntry {
                id: "drop".to_string(),
                enabled: false,
                kind: None,
                source: None,
                destination: None,
                recursive: false,
                surface: None,
                comment_style: None,
                body_template: None,
            }],
        };
        apply(&mut base, &overlay).unwrap();
        assert_eq!(base.entries.len(), 1);
        assert_eq!(base.entries[0].id, "keep");
    }

    #[test]
    fn apply_replaces_existing_entry() {
        let mut base = LinkMap {
            schema_version: 1,
            entries: vec![tracked_entry("x")],
        };
        let overlay = LinkMapOverlay {
            schema_version: 1,
            entries: vec![OverlayEntry {
                id: "x".to_string(),
                enabled: true,
                kind: Some(EntryKind::SymlinkedFile),
                source: Some("build/replaced".to_string()),
                destination: Some("plugins/replaced".to_string()),
                recursive: false,
                surface: None,
                comment_style: None,
                body_template: None,
            }],
        };
        apply(&mut base, &overlay).unwrap();
        assert_eq!(base.entries.len(), 1);
        assert_eq!(base.entries[0].source.as_deref(), Some("build/replaced"));
        assert_eq!(base.entries[0].destination, "plugins/replaced");
    }

    #[test]
    fn apply_adds_new_entry() {
        let mut base = LinkMap {
            schema_version: 1,
            entries: vec![tracked_entry("a")],
        };
        let overlay = LinkMapOverlay {
            schema_version: 1,
            entries: vec![OverlayEntry {
                id: "b".to_string(),
                enabled: true,
                kind: Some(EntryKind::SymlinkedFile),
                source: Some("build/b".to_string()),
                destination: Some("plugins/b".to_string()),
                recursive: false,
                surface: None,
                comment_style: None,
                body_template: None,
            }],
        };
        apply(&mut base, &overlay).unwrap();
        assert_eq!(base.entries.len(), 2);
        assert_eq!(base.entries[1].id, "b");
    }

    #[test]
    fn apply_rejects_enabled_entry_missing_kind() {
        let mut base = LinkMap {
            schema_version: 1,
            entries: vec![],
        };
        let overlay = LinkMapOverlay {
            schema_version: 1,
            entries: vec![OverlayEntry {
                id: "x".to_string(),
                enabled: true,
                kind: None,
                source: None,
                destination: Some("plugins/x".to_string()),
                recursive: false,
                surface: None,
                comment_style: None,
                body_template: None,
            }],
        };
        let err = apply(&mut base, &overlay).unwrap_err();
        match err {
            LinkMapError::InvalidEntry { reason, .. } => {
                assert!(reason.contains("`kind`"), "got: {reason}");
            }
            other => panic!("expected InvalidEntry, got {other:?}"),
        }
    }

    #[test]
    fn apply_rejects_enabled_entry_failing_kind_contract() {
        // managed-block requires surface/comment_style/body_template; an
        // overlay entry that names kind=managed-block but omits them must
        // fail the same contract the tracked link-map enforces.
        let mut base = LinkMap {
            schema_version: 1,
            entries: vec![],
        };
        let overlay = LinkMapOverlay {
            schema_version: 1,
            entries: vec![OverlayEntry {
                id: "bad".to_string(),
                enabled: true,
                kind: Some(EntryKind::ManagedBlock),
                source: None,
                destination: Some("config.toml".to_string()),
                recursive: false,
                surface: None,
                comment_style: None,
                body_template: None,
            }],
        };
        let err = apply(&mut base, &overlay).unwrap_err();
        match err {
            LinkMapError::InvalidEntry { reason, .. } => {
                assert!(reason.contains("surface"), "got: {reason}");
            }
            other => panic!("expected InvalidEntry, got {other:?}"),
        }
    }
}
