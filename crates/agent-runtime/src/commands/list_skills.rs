//! `agent-runtime list-skills` — enumerate skills that an `install`
//! would activate for a given product, source-root, and (optional)
//! live-home triple.
//!
//! Reuses the existing `install::link_map::LinkMap` parser and the
//! `install::plan::InstallPlan::build` projection so the enumeration
//! matches what `install --dry-run` would activate. Adds deterministic
//! `text` / `json` formatters keyed by sorted skill `id`.

use crate::doctor::skill_surface;
use crate::install::link_map::LinkMap;
use crate::install::plan::{InstallPlan, PlanAction, SymlinkLinkMode};
use crate::render::manifest::{SkillExposure, SkillInvocation, SourceRoot, load_optional_skills};
use clap::{Args, ValueEnum};
use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

const SCHEMA_VERSION: &str = "cli.agent-runtime.list-skills.v1";

#[derive(Args, Debug)]
pub struct ListSkillsArgs {
    /// Source root containing `manifests/`, `core/`, `targets/`, `build/`.
    /// Defaults to the current working directory.
    #[arg(long)]
    pub source_root: Option<PathBuf>,
    /// Product to enumerate (`codex` or `claude`).
    #[arg(long)]
    pub product: String,
    /// Absolute path of the runtime home. Accepted for parity with
    /// `install` / `doctor`. Not required for enumeration in v1.
    #[arg(long)]
    pub live_home: Option<PathBuf>,
    /// Output format.
    #[arg(long, value_enum, default_value = "text")]
    pub format: OutputFormat,
    /// Surface `skill_surface` warnings inline in text output. Warnings
    /// are always present in `json` output.
    #[arg(long, default_value_t = false)]
    pub include_warnings: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillsReport {
    pub schema: &'static str,
    pub product: String,
    pub source_root: PathBuf,
    pub live_home: Option<PathBuf>,
    pub skills: Vec<SkillRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillRecord {
    pub id: String,
    pub source: String,
    pub destination: String,
    pub link_mode: SkillLinkMode,
    /// Codex discoverability. `Some(bool)` for `--product codex`,
    /// `None` for other products.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discoverable: Option<bool>,
    pub invocation: Option<SkillInvocation>,
    pub exposure: Option<SkillExposure>,
    pub pending_disposition: bool,
    pub warnings: Vec<SkillWarning>,
}

#[derive(Debug, Clone)]
struct SkillMetadata {
    invocation: Option<SkillInvocation>,
    exposure: Option<SkillExposure>,
    pending_disposition: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkillLinkMode {
    File,
    Directory,
    RecursiveFile,
}

impl From<SymlinkLinkMode> for SkillLinkMode {
    fn from(value: SymlinkLinkMode) -> Self {
        match value {
            SymlinkLinkMode::File => SkillLinkMode::File,
            SymlinkLinkMode::Directory => SkillLinkMode::Directory,
            SymlinkLinkMode::RecursiveFile => SkillLinkMode::RecursiveFile,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillWarning {
    pub code: String,
    pub message: String,
}

pub fn run(args: ListSkillsArgs) -> anyhow::Result<u8> {
    if args.product != "claude" && args.product != "codex" && args.product != "hermes" {
        anyhow::bail!(
            "agent-runtime list-skills: unknown --product `{}` (expected one of: claude, codex, hermes)",
            args.product
        );
    }
    if let Some(path) = args.live_home.as_deref()
        && !path.is_absolute()
    {
        anyhow::bail!(
            "agent-runtime list-skills: --live-home must be absolute (got: {})",
            path.display()
        );
    }

    let root = SourceRoot::from_arg_or_cwd(args.source_root.as_deref())?;
    let skills_manifest = load_optional_skills(&root)?;
    let metadata_by_id = skills_manifest
        .as_ref()
        .map(|manifest| {
            manifest
                .skills
                .iter()
                .map(|skill| {
                    (
                        skill.id.clone(),
                        SkillMetadata {
                            invocation: skill.invocation.clone(),
                            exposure: skill.exposure.clone(),
                            pending_disposition: manifest.is_pending_disposition(&skill.id),
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let link_map = LinkMap::load(root.path(), &args.product)?;

    // The install plan expansion is filesystem-aware but does not mutate
    // any state at build time. Use the caller-provided live-home when
    // present so `dest` paths in the plan match the rehearsal probe,
    // and fall back to a synthetic absolute path otherwise.
    let synthetic_home = args
        .live_home
        .clone()
        .unwrap_or_else(|| PathBuf::from("/var/empty/agent-runtime-list-skills-home"));
    let synthetic_state_home = PathBuf::from("/var/empty/agent-runtime-list-skills-state");
    let plan = InstallPlan::build(
        &args.product,
        root.path(),
        &synthetic_home,
        &synthetic_state_home,
        &link_map,
    )?;

    let warnings_by_entry_id = if args.product == "codex" {
        let report = skill_surface::check(&args.product, root.path(), &link_map);
        report
            .items
            .into_iter()
            .filter(|item| !item.warnings.is_empty())
            .map(|item| {
                let mut warnings: Vec<SkillWarning> = item
                    .warnings
                    .into_iter()
                    .map(|w| SkillWarning {
                        code: w.code.to_string(),
                        message: w.message,
                    })
                    .collect();
                warnings
                    .sort_by(|a, b| a.code.cmp(&b.code).then_with(|| a.message.cmp(&b.message)));
                (item.id, warnings)
            })
            .collect::<BTreeMap<String, Vec<SkillWarning>>>()
    } else {
        BTreeMap::new()
    };

    let mut by_id: BTreeMap<String, SkillRecord> = BTreeMap::new();
    for action in &plan.actions {
        let PlanAction::Symlink {
            entry_id,
            source,
            dest,
            link_mode,
            ..
        } = action
        else {
            continue;
        };
        let Ok(dest_rel) = dest.strip_prefix(&synthetic_home) else {
            continue;
        };
        let Some(id) = identify_skill(&args.product, dest_rel) else {
            continue;
        };
        if by_id.contains_key(&id) {
            // Multiple plan actions can map to the same skill (e.g. a
            // claude recursive expansion includes SKILL.md plus sibling
            // files). The first SKILL.md hit becomes the canonical
            // record; subsequent siblings are ignored.
            continue;
        }
        let source_rel = source
            .strip_prefix(root.path())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| source.to_string_lossy().to_string());
        let dest_str = dest_rel.to_string_lossy().to_string();
        let discoverable = match args.product.as_str() {
            "codex" => Some(
                matches!(link_mode, SymlinkLinkMode::Directory) && is_skills_prefixed(dest_rel),
            ),
            _ => None,
        };
        let warnings = warnings_by_entry_id
            .get(entry_id)
            .cloned()
            .unwrap_or_default();
        let metadata = metadata_by_id.get(&id);
        by_id.insert(
            id.clone(),
            SkillRecord {
                id,
                source: source_rel,
                destination: dest_str,
                link_mode: (*link_mode).into(),
                discoverable,
                invocation: metadata.and_then(|item| item.invocation.clone()),
                exposure: metadata.and_then(|item| item.exposure.clone()),
                pending_disposition: metadata.is_some_and(|item| item.pending_disposition),
                warnings,
            },
        );
    }

    let report = SkillsReport {
        schema: SCHEMA_VERSION,
        product: args.product.clone(),
        source_root: root.path().to_path_buf(),
        live_home: args.live_home.clone(),
        skills: by_id.into_values().collect(),
    };

    match args.format {
        OutputFormat::Text => print_text(&report, args.include_warnings),
        OutputFormat::Json => print_json(&report)?,
    }
    Ok(0)
}

fn identify_skill(product: &str, dest_rel: &Path) -> Option<String> {
    let components: Vec<&str> = dest_rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    match product {
        // Hermes installs skills under `~/.hermes/skills/<domain>/<skill>/`
        // via a recursive copy, so its destination layout matches codex's
        // `skills/<domain>/<skill>` surface exactly.
        "codex" | "hermes" => {
            // `skills/<domain>/<skill>` is the active discoverable skill
            // surface; the rehearsal pins these.
            if components.len() == 3 && components[0] == "skills" {
                return Some(format!("{}.{}", components[1], components[2]));
            }
            // `skills/<domain>/<skill>/SKILL.md` is the warning leaf.
            if components.len() == 4 && components[0] == "skills" && components[3] == "SKILL.md" {
                return Some(format!("{}.{}", components[1], components[2]));
            }
            None
        }
        "claude" => {
            // `plugins/<domain>/skills/<skill>/SKILL.md` is the canonical
            // per-skill leaf produced by the claude recursive expansion.
            if components.len() == 5
                && components[0] == "plugins"
                && components[2] == "skills"
                && components[4] == "SKILL.md"
            {
                return Some(format!("{}.{}", components[1], components[3]));
            }
            None
        }
        _ => None,
    }
}

fn is_skills_prefixed(path: &Path) -> bool {
    matches!(path.components().next(), Some(Component::Normal(first)) if first == OsStr::new("skills"))
}

fn print_text(report: &SkillsReport, include_warnings: bool) {
    for skill in &report.skills {
        let link_mode = match skill.link_mode {
            SkillLinkMode::File => "file",
            SkillLinkMode::Directory => "directory",
            SkillLinkMode::RecursiveFile => "recursive-file",
        };
        println!("{}\t{}\t{}", skill.id, link_mode, skill.destination);
        if include_warnings {
            for warning in &skill.warnings {
                println!("  warning {}: {}", warning.code, warning.message);
            }
        }
    }
}

fn print_json(report: &SkillsReport) -> anyhow::Result<()> {
    let serialised = serde_json::to_string_pretty(report)?;
    println!("{serialised}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn write_min_runtime_roots(root: &Path) {
        write(
            &root.join("manifests/runtime-roots.yaml"),
            r#"schema_version: 1
products: {}
"#,
        );
    }

    fn make_codex_source_root() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_min_runtime_roots(root);
        write(
            &root.join("targets/codex/link-map.yaml"),
            r#"schema_version: 1
entries:
  - id: reporting.plugin-manifest
    kind: plugin-manifest-copy
    source: targets/codex/plugins/reporting/.codex-plugin/plugin.json
    destination: plugins/reporting/.codex-plugin/plugin.json
  - id: reporting.daily-brief.codex-skill-dir
    kind: symlinked-file
    source: build/codex/plugins/reporting/skills/daily-brief
    destination: skills/reporting/daily-brief
    recursive: false
  - id: reporting.topic-radar.codex-skill-dir
    kind: symlinked-file
    source: build/codex/plugins/reporting/skills/topic-radar
    destination: skills/reporting/topic-radar
    recursive: false
"#,
        );
        write(
            &root.join("targets/codex/plugins/reporting/.codex-plugin/plugin.json"),
            "{}\n",
        );
        write(
            &root.join("build/codex/plugins/reporting/skills/daily-brief/SKILL.md"),
            "# daily-brief\n",
        );
        write(
            &root.join("build/codex/plugins/reporting/skills/topic-radar/SKILL.md"),
            "# topic-radar\n",
        );
        tmp
    }

    fn make_claude_source_root() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_min_runtime_roots(root);
        write(
            &root.join("targets/claude/link-map.yaml"),
            r#"schema_version: 1
entries:
  - id: reporting.plugin-manifest
    kind: plugin-manifest-copy
    source: targets/claude/plugins/reporting/.claude-plugin/plugin.json
    destination: plugins/reporting/.claude-plugin/plugin.json
  - id: reporting.skills-tree
    kind: symlinked-file
    source: build/claude/plugins/reporting/skills
    destination: plugins/reporting/skills
    recursive: true
"#,
        );
        write(
            &root.join("targets/claude/plugins/reporting/.claude-plugin/plugin.json"),
            "{}\n",
        );
        write(
            &root.join("build/claude/plugins/reporting/skills/daily-brief/SKILL.md"),
            "# daily-brief\n",
        );
        write(
            &root.join("build/claude/plugins/reporting/skills/topic-radar/SKILL.md"),
            "# topic-radar\n",
        );
        tmp
    }

    fn make_codex_file_symlink_root() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_min_runtime_roots(root);
        write(
            &root.join("targets/codex/link-map.yaml"),
            r#"schema_version: 1
entries:
  - id: reporting.daily-brief.codex-skill-md
    kind: symlinked-file
    source: build/codex/plugins/reporting/skills/daily-brief/SKILL.md
    destination: skills/reporting/daily-brief/SKILL.md
    recursive: false
"#,
        );
        write(
            &root.join("build/codex/plugins/reporting/skills/daily-brief/SKILL.md"),
            "# daily-brief\n",
        );
        tmp
    }

    #[test]
    fn enumerates_codex_skills_sorted_by_id() {
        let tmp = make_codex_source_root();
        let args = ListSkillsArgs {
            source_root: Some(tmp.path().to_path_buf()),
            product: "codex".to_string(),
            live_home: None,
            format: OutputFormat::Json,
            include_warnings: false,
        };
        let root = SourceRoot::from_arg_or_cwd(args.source_root.as_deref()).unwrap();
        let link_map = LinkMap::load(root.path(), &args.product).unwrap();
        let synthetic_home = PathBuf::from("/var/empty/agent-runtime-list-skills-home");
        let synthetic_state = PathBuf::from("/var/empty/agent-runtime-list-skills-state");
        let plan = InstallPlan::build(
            &args.product,
            root.path(),
            &synthetic_home,
            &synthetic_state,
            &link_map,
        )
        .unwrap();
        let mut by_id: BTreeMap<String, SkillRecord> = BTreeMap::new();
        for action in &plan.actions {
            let PlanAction::Symlink { dest, .. } = action else {
                continue;
            };
            let dest_rel = dest.strip_prefix(&synthetic_home).unwrap();
            if let Some(id) = identify_skill(&args.product, dest_rel)
                && !by_id.contains_key(&id)
            {
                by_id.insert(
                    id.clone(),
                    SkillRecord {
                        id,
                        source: String::new(),
                        destination: dest_rel.to_string_lossy().to_string(),
                        link_mode: SkillLinkMode::Directory,
                        discoverable: Some(true),
                        invocation: None,
                        exposure: None,
                        pending_disposition: false,
                        warnings: Vec::new(),
                    },
                );
            }
        }
        let ids: Vec<_> = by_id.keys().cloned().collect();
        assert_eq!(
            ids,
            vec![
                "reporting.daily-brief".to_string(),
                "reporting.topic-radar".to_string(),
            ]
        );
    }

    #[test]
    fn identifies_codex_active_skill_dir() {
        let id = identify_skill("codex", Path::new("skills/reporting/daily-brief")).unwrap();
        assert_eq!(id, "reporting.daily-brief");
    }

    #[test]
    fn identifies_codex_skill_md_leaf() {
        let id =
            identify_skill("codex", Path::new("skills/reporting/daily-brief/SKILL.md")).unwrap();
        assert_eq!(id, "reporting.daily-brief");
    }

    #[test]
    fn identifies_claude_skill_md_leaf() {
        let id = identify_skill(
            "claude",
            Path::new("plugins/reporting/skills/daily-brief/SKILL.md"),
        )
        .unwrap();
        assert_eq!(id, "reporting.daily-brief");
    }

    #[test]
    fn rejects_non_skill_destinations() {
        assert!(
            identify_skill(
                "codex",
                Path::new("plugins/reporting/.codex-plugin/plugin.json")
            )
            .is_none()
        );
        assert!(identify_skill("claude", Path::new("commands/foo.md")).is_none());
    }

    #[test]
    fn rejects_unknown_product() {
        let args = ListSkillsArgs {
            source_root: Some(PathBuf::from(".")),
            product: "vscode".to_string(),
            live_home: None,
            format: OutputFormat::Text,
            include_warnings: false,
        };
        let err = run(args).unwrap_err();
        assert!(err.to_string().contains("unknown --product"));
    }

    #[test]
    fn rejects_relative_live_home() {
        let args = ListSkillsArgs {
            source_root: Some(PathBuf::from(".")),
            product: "codex".to_string(),
            live_home: Some(PathBuf::from("relative/path")),
            format: OutputFormat::Text,
            include_warnings: false,
        };
        let err = run(args).unwrap_err();
        assert!(
            err.to_string().contains("--live-home must be absolute"),
            "{err}"
        );
    }

    #[test]
    fn claude_recursive_expansion_yields_one_record_per_skill() {
        let tmp = make_claude_source_root();
        let root = SourceRoot::from_arg_or_cwd(Some(tmp.path())).unwrap();
        let link_map = LinkMap::load(root.path(), "claude").unwrap();
        let synthetic_home = PathBuf::from("/var/empty/agent-runtime-list-skills-home");
        let synthetic_state = PathBuf::from("/var/empty/agent-runtime-list-skills-state");
        let plan = InstallPlan::build(
            "claude",
            root.path(),
            &synthetic_home,
            &synthetic_state,
            &link_map,
        )
        .unwrap();
        let mut ids = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for action in &plan.actions {
            let PlanAction::Symlink { dest, .. } = action else {
                continue;
            };
            let dest_rel = dest.strip_prefix(&synthetic_home).unwrap();
            if let Some(id) = identify_skill("claude", dest_rel)
                && seen.insert(id.clone())
            {
                ids.push(id);
            }
        }
        ids.sort();
        assert_eq!(
            ids,
            vec![
                "reporting.daily-brief".to_string(),
                "reporting.topic-radar".to_string(),
            ]
        );
    }

    fn make_hermes_source_root() -> TempDir {
        // Hermes installs skills under `~/.hermes/skills/<domain>/<skill>/`
        // via a recursive skills-tree, so destinations are relative to the
        // `~/.hermes` live-home and start with a bare `skills/` (no `.hermes/`
        // prefix) exactly like codex's `skills/` surface.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_min_runtime_roots(root);
        write(
            &root.join("targets/hermes/link-map.yaml"),
            r#"schema_version: 1
entries:
  - id: reporting.plugin-manifest
    kind: plugin-manifest-copy
    source: targets/hermes/plugins/reporting/.hermes-plugin/plugin.json
    destination: plugins/reporting/.hermes-plugin/plugin.json
  - id: reporting.skills-tree
    kind: symlinked-file
    source: build/hermes/plugins/reporting/skills
    destination: skills/reporting
    recursive: true
"#,
        );
        write(
            &root.join("targets/hermes/plugins/reporting/.hermes-plugin/plugin.json"),
            "{}\n",
        );
        write(
            &root.join("build/hermes/plugins/reporting/skills/daily-brief/SKILL.md"),
            "# daily-brief\n",
        );
        write(
            &root.join("build/hermes/plugins/reporting/skills/topic-radar/SKILL.md"),
            "# topic-radar\n",
        );
        tmp
    }

    #[test]
    fn accepts_hermes_product() {
        // Before hermes was added to the product guard, `run` bailed with
        // "unknown --product `hermes`"; it must now enumerate cleanly.
        let tmp = make_hermes_source_root();
        let args = ListSkillsArgs {
            source_root: Some(tmp.path().to_path_buf()),
            product: "hermes".to_string(),
            live_home: None,
            format: OutputFormat::Json,
            include_warnings: false,
        };
        assert_eq!(run(args).unwrap(), 0);
    }

    #[test]
    fn hermes_recursive_expansion_yields_one_record_per_skill() {
        let tmp = make_hermes_source_root();
        let root = SourceRoot::from_arg_or_cwd(Some(tmp.path())).unwrap();
        let link_map = LinkMap::load(root.path(), "hermes").unwrap();
        let synthetic_home = PathBuf::from("/var/empty/agent-runtime-list-skills-home");
        let synthetic_state = PathBuf::from("/var/empty/agent-runtime-list-skills-state");
        let plan = InstallPlan::build(
            "hermes",
            root.path(),
            &synthetic_home,
            &synthetic_state,
            &link_map,
        )
        .unwrap();
        let mut ids = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for action in &plan.actions {
            let PlanAction::Symlink { dest, .. } = action else {
                continue;
            };
            let dest_rel = dest.strip_prefix(&synthetic_home).unwrap();
            if let Some(id) = identify_skill("hermes", dest_rel)
                && seen.insert(id.clone())
            {
                ids.push(id);
            }
        }
        ids.sort();
        assert_eq!(
            ids,
            vec![
                "reporting.daily-brief".to_string(),
                "reporting.topic-radar".to_string(),
            ]
        );
    }

    #[test]
    fn codex_file_symlink_leaf_surfaces_warning() {
        let tmp = make_codex_file_symlink_root();
        let root = SourceRoot::from_arg_or_cwd(Some(tmp.path())).unwrap();
        let link_map = LinkMap::load(root.path(), "codex").unwrap();
        let report = skill_surface::check("codex", root.path(), &link_map);
        let warnings: Vec<_> = report
            .items
            .iter()
            .flat_map(|i| i.warnings.iter())
            .collect();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, skill_surface::FILE_SYMLINK_WARNING);
    }
}
