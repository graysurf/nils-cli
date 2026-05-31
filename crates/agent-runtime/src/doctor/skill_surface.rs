//! Codex active skill-surface classifier for `agent-runtime doctor`.
//!
//! This diagnostic is shape-only. It reads the rendered link map and
//! source tree to classify install intent; it deliberately does not stat
//! `$CODEX_HOME` or attempt to reproduce Codex Desktop discovery.

use super::{DoctorFinding, DoctorSeverity};
use crate::install::link_map::{EntryKind, LinkEntry, LinkMap};
use serde::Serialize;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

pub const CLASS: &str = "skill-surface";
pub const FILE_SYMLINK_WARNING: &str = "codex.active-skill.file-symlink";
pub const CODEX_ACCEPTANCE_BOUNDARY: &str = "shape validation only; live Codex Desktop discovery still requires `codex debug prompt-input` in a fresh session";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillSurfaceReport {
    pub product: String,
    pub items: Vec<SkillSurfaceItem>,
    pub findings: Vec<DoctorFinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acceptance_boundary: Option<String>,
}

impl SkillSurfaceReport {
    pub fn empty(product: &str) -> Self {
        Self {
            product: product.to_string(),
            items: Vec::new(),
            findings: Vec::new(),
            acceptance_boundary: acceptance_boundary(product).map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillSurfaceItem {
    pub id: String,
    pub source: String,
    pub destination: String,
    pub link_mode: SkillSurfaceLinkMode,
    pub expected_codex_discoverable: CodexDiscoverability,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<SkillSurfaceWarning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkillSurfaceLinkMode {
    File,
    Directory,
    RecursiveFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexDiscoverability {
    Yes,
    No,
    NotApplicable,
}

impl Serialize for CodexDiscoverability {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Yes => serializer.serialize_bool(true),
            Self::No => serializer.serialize_bool(false),
            Self::NotApplicable => serializer.serialize_str("not-applicable"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillSurfaceWarning {
    pub code: &'static str,
    pub message: String,
    pub remediation: &'static str,
}

pub fn acceptance_boundary(product: &str) -> Option<&'static str> {
    (product == "codex").then_some(CODEX_ACCEPTANCE_BOUNDARY)
}

pub fn check(product: &str, source_root: &Path, link_map: &LinkMap) -> SkillSurfaceReport {
    let mut items = Vec::new();
    let mut findings = Vec::new();
    for entry in &link_map.entries {
        let Some(item) = classify_entry(product, source_root, entry) else {
            continue;
        };
        for warning in &item.warnings {
            findings.push(DoctorFinding {
                product: product.to_string(),
                check: CLASS,
                severity: DoctorSeverity::Warn,
                entry_id: Some(item.id.clone()),
                path: Some(PathBuf::from(&item.destination)),
                message: format!(
                    "{}: {}; {}",
                    warning.code, warning.message, warning.remediation
                ),
            });
        }
        items.push(item);
    }
    SkillSurfaceReport {
        product: product.to_string(),
        items,
        findings,
        acceptance_boundary: acceptance_boundary(product).map(str::to_string),
    }
}

fn classify_entry(
    product: &str,
    source_root: &Path,
    entry: &LinkEntry,
) -> Option<SkillSurfaceItem> {
    let source = entry.source.as_deref()?;
    let source_abs = source_root.join(source);
    let link_mode = link_mode(&source_abs, entry);
    let destination = clean_rel_path(&entry.destination);
    let expected_codex_discoverable =
        expected_codex_discoverable(product, destination.as_deref(), link_mode, entry);
    let warnings = warnings(product, destination.as_deref(), &entry.id);
    Some(SkillSurfaceItem {
        id: entry.id.clone(),
        source: source.to_string(),
        destination: entry.destination.clone(),
        link_mode,
        expected_codex_discoverable,
        warnings,
    })
}

fn link_mode(source_abs: &Path, entry: &LinkEntry) -> SkillSurfaceLinkMode {
    if entry.kind == EntryKind::SymlinkedFile && entry.recursive {
        return SkillSurfaceLinkMode::RecursiveFile;
    }
    match std::fs::symlink_metadata(source_abs) {
        Ok(meta) if meta.is_dir() => SkillSurfaceLinkMode::Directory,
        _ => SkillSurfaceLinkMode::File,
    }
}

fn expected_codex_discoverable(
    product: &str,
    destination: Option<&Path>,
    link_mode: SkillSurfaceLinkMode,
    entry: &LinkEntry,
) -> CodexDiscoverability {
    if product != "codex" {
        return CodexDiscoverability::NotApplicable;
    }
    let Some(destination) = destination else {
        return CodexDiscoverability::NotApplicable;
    };
    if !is_skills_prefixed(destination) {
        return CodexDiscoverability::NotApplicable;
    }
    if entry.kind == EntryKind::SymlinkedFile
        && !entry.recursive
        && link_mode == SkillSurfaceLinkMode::Directory
        && is_domain_nested_skill_leaf(destination)
    {
        CodexDiscoverability::Yes
    } else {
        CodexDiscoverability::No
    }
}

fn warnings(
    product: &str,
    destination: Option<&Path>,
    _entry_id: &str,
) -> Vec<SkillSurfaceWarning> {
    let Some(destination) = destination else {
        return Vec::new();
    };
    if product == "codex" && is_skill_md_leaf(destination) {
        vec![SkillSurfaceWarning {
            code: FILE_SYMLINK_WARNING,
            message: format!(
                "Codex active skill destination `{}` is a SKILL.md file symlink",
                destination.display()
            ),
            remediation: "use a directory-symlink leaf at `skills/<domain>/<skill>`",
        }]
    } else {
        Vec::new()
    }
}

fn clean_rel_path(raw: &str) -> Option<PathBuf> {
    let path = Path::new(raw);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return None;
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(path.to_path_buf())
}

fn is_skills_prefixed(path: &Path) -> bool {
    matches!(path.components().next(), Some(Component::Normal(first)) if first == OsStr::new("skills"))
}

fn is_domain_nested_skill_leaf(path: &Path) -> bool {
    let components: Vec<_> = path.components().collect();
    components.len() >= 3
        && matches!(components[0], Component::Normal(first) if first == OsStr::new("skills"))
        && !matches!(components.last(), Some(Component::Normal(last)) if *last == OsStr::new("SKILL.md"))
}

fn is_skill_md_leaf(path: &Path) -> bool {
    is_skills_prefixed(path)
        && matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("SKILL.md")
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::link_map::LinkEntry;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::TempDir;

    fn entry(id: &str, source: &str, destination: &str, recursive: bool) -> LinkEntry {
        LinkEntry {
            id: id.to_string(),
            kind: EntryKind::SymlinkedFile,
            source: Some(source.to_string()),
            destination: destination.to_string(),
            recursive,
            surface: None,
            comment_style: None,
            body_template: None,
        }
    }

    #[test]
    fn classifies_domain_nested_directory_skill_as_codex_discoverable() {
        let tmp = TempDir::new().unwrap();
        let source = tmp
            .path()
            .join("build/codex/plugins/reporting/skills/daily-brief");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "# daily brief\n").unwrap();

        let item = classify_entry(
            "codex",
            tmp.path(),
            &entry(
                "reporting.daily-brief",
                "build/codex/plugins/reporting/skills/daily-brief",
                "skills/reporting/daily-brief",
                false,
            ),
        )
        .unwrap();

        assert_eq!(item.link_mode, SkillSurfaceLinkMode::Directory);
        assert_eq!(item.expected_codex_discoverable, CodexDiscoverability::Yes);
        assert_eq!(item.warnings, Vec::new());
    }

    #[test]
    fn classifies_skill_md_file_leaf_as_not_discoverable_with_warning() {
        let tmp = TempDir::new().unwrap();
        let source = tmp
            .path()
            .join("build/codex/plugins/reporting/skills/daily-brief/SKILL.md");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, "# daily brief\n").unwrap();

        let item = classify_entry(
            "codex",
            tmp.path(),
            &entry(
                "reporting.daily-brief",
                "build/codex/plugins/reporting/skills/daily-brief/SKILL.md",
                "skills/reporting/daily-brief/SKILL.md",
                false,
            ),
        )
        .unwrap();

        assert_eq!(item.link_mode, SkillSurfaceLinkMode::File);
        assert_eq!(item.expected_codex_discoverable, CodexDiscoverability::No);
        assert_eq!(item.warnings.len(), 1);
        assert_eq!(item.warnings[0].code, FILE_SYMLINK_WARNING);
    }

    #[test]
    fn classifies_recursive_skill_entry_as_not_discoverable() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("build/codex/plugins/reporting/skills");
        fs::create_dir_all(source.join("daily-brief")).unwrap();
        fs::write(source.join("daily-brief/SKILL.md"), "# daily brief\n").unwrap();

        let item = classify_entry(
            "codex",
            tmp.path(),
            &entry(
                "reporting.skills-tree",
                "build/codex/plugins/reporting/skills",
                "skills/reporting",
                true,
            ),
        )
        .unwrap();

        assert_eq!(item.link_mode, SkillSurfaceLinkMode::RecursiveFile);
        assert_eq!(item.expected_codex_discoverable, CodexDiscoverability::No);
    }

    #[test]
    fn classifies_non_skills_destination_as_not_applicable() {
        let tmp = TempDir::new().unwrap();
        let source = tmp
            .path()
            .join("targets/codex/plugins/reporting/.codex-plugin/plugin.json");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, "{}\n").unwrap();

        let item = classify_entry(
            "codex",
            tmp.path(),
            &entry(
                "reporting.plugin-manifest",
                "targets/codex/plugins/reporting/.codex-plugin/plugin.json",
                "plugins/reporting/.codex-plugin/plugin.json",
                false,
            ),
        )
        .unwrap();

        assert_eq!(item.link_mode, SkillSurfaceLinkMode::File);
        assert_eq!(
            item.expected_codex_discoverable,
            CodexDiscoverability::NotApplicable
        );
    }
}
