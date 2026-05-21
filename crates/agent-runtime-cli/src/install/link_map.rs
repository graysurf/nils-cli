//! `targets/<product>/link-map.yaml` parser.
//!
//! Schema source-of-truth lives in `agent-runtime-kit`:
//! `core/docs/schemas/link-map.schema.json`. This module mirrors that
//! schema with `#[serde(deny_unknown_fields)]` types so unknown keys fail
//! deserialization loudly instead of being silently dropped. Per-kind
//! required/forbidden field enforcement happens in [`LinkEntry::validate`]
//! after the YAML round-trip, because `serde` alone cannot express the
//! `allOf if/then` shape that the JSON Schema uses.
//!
//! Source: agent-runtime-kit Plan 04 Sprint 1 Task 1.2.
//!   `docs/plans/04-installer-doctor-and-bootstrap/...-plan.md`.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum LinkMapError {
    #[error("missing link-map: {path}")]
    Missing { path: PathBuf },
    #[error("schema_version mismatch in {file}: expected {expected}, got {found}")]
    SchemaVersion {
        file: PathBuf,
        expected: u32,
        found: u32,
    },
    #[error("parse error in {file}: {source}")]
    Parse {
        file: PathBuf,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("io error reading {file}: {source}")]
    Io {
        file: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("link-map entry `{id}` is invalid: {reason} (file: {file})")]
    InvalidEntry {
        file: PathBuf,
        id: String,
        reason: String,
    },
    #[error("link-map entry id `{id}` is duplicated in {file}")]
    DuplicateId { file: PathBuf, id: String },
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    SymlinkedFile,
    PluginManifestCopy,
    ManagedBlock,
    BackedUpOnReplace,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CommentStyle {
    Hash,
    DoubleSlash,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LinkEntry {
    pub id: String,
    pub kind: EntryKind,
    #[serde(default)]
    pub source: Option<String>,
    pub destination: String,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub surface: Option<String>,
    #[serde(default)]
    pub comment_style: Option<CommentStyle>,
    #[serde(default)]
    pub body_template: Option<String>,
}

impl LinkEntry {
    /// Enforce the per-kind required / forbidden field contract that the
    /// JSON Schema expresses via `allOf if/then`. Returns the first
    /// violation it finds so error messages stay actionable.
    pub fn validate(&self) -> Result<(), String> {
        match self.kind {
            EntryKind::SymlinkedFile => {
                if self.source.is_none() {
                    return Err("kind=symlinked-file requires `source`".to_string());
                }
                self.forbid_managed_block_fields()?;
            }
            EntryKind::PluginManifestCopy => {
                if self.source.is_none() {
                    return Err("kind=plugin-manifest-copy requires `source`".to_string());
                }
                if self.recursive {
                    return Err("kind=plugin-manifest-copy forbids `recursive`".to_string());
                }
                self.forbid_managed_block_fields()?;
            }
            EntryKind::BackedUpOnReplace => {
                if self.source.is_none() {
                    return Err("kind=backed-up-on-replace requires `source`".to_string());
                }
                self.forbid_managed_block_fields()?;
            }
            EntryKind::ManagedBlock => {
                if self.surface.is_none() {
                    return Err("kind=managed-block requires `surface`".to_string());
                }
                if self.comment_style.is_none() {
                    return Err("kind=managed-block requires `comment_style`".to_string());
                }
                if self.body_template.is_none() {
                    return Err("kind=managed-block requires `body_template`".to_string());
                }
                if self.source.is_some() {
                    return Err("kind=managed-block forbids `source`".to_string());
                }
                if self.recursive {
                    return Err("kind=managed-block forbids `recursive`".to_string());
                }
            }
        }
        Ok(())
    }

    fn forbid_managed_block_fields(&self) -> Result<(), String> {
        if self.surface.is_some() {
            return Err(format!("kind={:?} forbids `surface`", self.kind));
        }
        if self.comment_style.is_some() {
            return Err(format!("kind={:?} forbids `comment_style`", self.kind));
        }
        if self.body_template.is_some() {
            return Err(format!("kind={:?} forbids `body_template`", self.kind));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LinkMap {
    pub schema_version: u32,
    pub entries: Vec<LinkEntry>,
}

impl LinkMap {
    /// Load and validate the link-map at the conventional location:
    /// `<source_root>/targets/<product>/link-map.yaml`.
    pub fn load(source_root: &Path, product: &str) -> Result<Self, LinkMapError> {
        let file = source_root
            .join("targets")
            .join(product)
            .join("link-map.yaml");
        if !file.exists() {
            return Err(LinkMapError::Missing { path: file });
        }
        let raw = std::fs::read_to_string(&file).map_err(|source| LinkMapError::Io {
            file: file.clone(),
            source,
        })?;
        let parsed: LinkMap =
            serde_yaml_ng::from_str(&raw).map_err(|source| LinkMapError::Parse {
                file: file.clone(),
                source,
            })?;
        if parsed.schema_version != SCHEMA_VERSION {
            return Err(LinkMapError::SchemaVersion {
                file,
                expected: SCHEMA_VERSION,
                found: parsed.schema_version,
            });
        }

        // Per-entry validation + duplicate id detection.
        let mut seen = std::collections::BTreeSet::new();
        for entry in &parsed.entries {
            entry
                .validate()
                .map_err(|reason| LinkMapError::InvalidEntry {
                    file: file.clone(),
                    id: entry.id.clone(),
                    reason,
                })?;
            if !seen.insert(entry.id.clone()) {
                return Err(LinkMapError::DuplicateId {
                    file,
                    id: entry.id.clone(),
                });
            }
        }
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_link_map(root: &Path, product: &str, body: &str) -> PathBuf {
        let dir = root.join("targets").join(product);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("link-map.yaml");
        fs::write(&file, body).unwrap();
        file
    }

    #[test]
    fn load_accepts_initial_codex_shape() {
        let tmp = TempDir::new().unwrap();
        write_link_map(
            tmp.path(),
            "codex",
            "\
schema_version: 1
entries:
  - id: reporting.plugin-manifest
    kind: plugin-manifest-copy
    source: targets/codex/plugins/reporting/.codex-plugin/plugin.json
    destination: plugins/reporting/.codex-plugin/plugin.json
  - id: reporting.skills-tree
    kind: symlinked-file
    source: build/codex/plugins/reporting/skills
    destination: plugins/reporting/skills
    recursive: true
",
        );
        let lm = LinkMap::load(tmp.path(), "codex").unwrap();
        assert_eq!(lm.schema_version, 1);
        assert_eq!(lm.entries.len(), 2);
        assert_eq!(lm.entries[0].kind, EntryKind::PluginManifestCopy);
        assert_eq!(lm.entries[1].kind, EntryKind::SymlinkedFile);
        assert!(lm.entries[1].recursive);
    }

    #[test]
    fn load_rejects_missing_file() {
        let tmp = TempDir::new().unwrap();
        let err = LinkMap::load(tmp.path(), "codex").unwrap_err();
        assert!(matches!(err, LinkMapError::Missing { .. }));
    }

    #[test]
    fn load_rejects_unknown_key() {
        let tmp = TempDir::new().unwrap();
        write_link_map(
            tmp.path(),
            "claude",
            "\
schema_version: 1
entries:
  - id: x
    kind: symlinked-file
    source: a
    destination: b
    bogus_field: 1
",
        );
        let err = LinkMap::load(tmp.path(), "claude").unwrap_err();
        assert!(matches!(err, LinkMapError::Parse { .. }));
    }

    #[test]
    fn load_rejects_schema_version_mismatch() {
        let tmp = TempDir::new().unwrap();
        write_link_map(
            tmp.path(),
            "codex",
            "\
schema_version: 99
entries:
  - id: x
    kind: symlinked-file
    source: a
    destination: b
",
        );
        let err = LinkMap::load(tmp.path(), "codex").unwrap_err();
        assert!(matches!(err, LinkMapError::SchemaVersion { found: 99, .. }));
    }

    #[test]
    fn load_rejects_managed_block_without_required_fields() {
        let tmp = TempDir::new().unwrap();
        write_link_map(
            tmp.path(),
            "codex",
            "\
schema_version: 1
entries:
  - id: bad
    kind: managed-block
    destination: config.toml
",
        );
        let err = LinkMap::load(tmp.path(), "codex").unwrap_err();
        match err {
            LinkMapError::InvalidEntry { id, reason, .. } => {
                assert_eq!(id, "bad");
                assert!(reason.contains("surface"), "got: {reason}");
            }
            other => panic!("expected InvalidEntry, got {other:?}"),
        }
    }

    #[test]
    fn load_rejects_managed_block_with_forbidden_source() {
        let tmp = TempDir::new().unwrap();
        write_link_map(
            tmp.path(),
            "codex",
            "\
schema_version: 1
entries:
  - id: bad
    kind: managed-block
    destination: config.toml
    surface: install
    comment_style: hash
    body_template: 't = 1'
    source: somewhere
",
        );
        let err = LinkMap::load(tmp.path(), "codex").unwrap_err();
        match err {
            LinkMapError::InvalidEntry { reason, .. } => {
                assert!(reason.contains("forbids `source`"), "got: {reason}");
            }
            other => panic!("expected InvalidEntry, got {other:?}"),
        }
    }

    #[test]
    fn load_rejects_duplicate_ids() {
        let tmp = TempDir::new().unwrap();
        write_link_map(
            tmp.path(),
            "codex",
            "\
schema_version: 1
entries:
  - id: same
    kind: symlinked-file
    source: a
    destination: b
  - id: same
    kind: symlinked-file
    source: c
    destination: d
",
        );
        let err = LinkMap::load(tmp.path(), "codex").unwrap_err();
        assert!(matches!(err, LinkMapError::DuplicateId { id, .. } if id == "same"));
    }
}
