use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::commands::scaffold_agents;
use crate::env::ResolvedRoots;
use crate::model::{BaselineTarget, Context, Scope};
use crate::paths::normalize_path;

const AGENTS_FILE_NAME: &str = "AGENTS.md";
const DEVELOPMENT_FILE_NAME: &str = "DEVELOPMENT.md";
const CLI_TOOLS_FILE_NAME: &str = "CLI_TOOLS.md";
const DEVELOPMENT_TEMPLATE: &str = include_str!("../templates/development_default.md");
const CLI_TOOLS_TEMPLATE: &str = include_str!("../templates/cli_tools_default.md");
const SETUP_PLACEHOLDER: &str = "{{SETUP_COMMANDS}}";
const BUILD_PLACEHOLDER: &str = "{{BUILD_COMMANDS}}";
const TEST_PLACEHOLDER: &str = "{{TEST_COMMANDS}}";
const CHECKS_SCRIPT_PATH: &str =
    ".agents/skills/nils-cli-verify-required-checks/scripts/nils-cli-verify-required-checks.sh";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldBaselineRequest {
    pub target: BaselineTarget,
    pub missing_only: bool,
    pub force: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ScaffoldBaselineAction {
    Created,
    Overwritten,
    Skipped,
    PlannedCreate,
    PlannedOverwrite,
    PlannedSkip,
}

impl ScaffoldBaselineAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Overwritten => "overwritten",
            Self::Skipped => "skipped",
            Self::PlannedCreate => "planned-create",
            Self::PlannedOverwrite => "planned-overwrite",
            Self::PlannedSkip => "planned-skip",
        }
    }
}

impl fmt::Display for ScaffoldBaselineAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScaffoldBaselineItemReport {
    pub scope: Scope,
    pub context: Context,
    pub label: String,
    pub path: PathBuf,
    pub action: ScaffoldBaselineAction,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScaffoldBaselineReport {
    pub target: BaselineTarget,
    pub missing_only: bool,
    pub force: bool,
    pub dry_run: bool,
    pub docs_home: PathBuf,
    pub project_path: PathBuf,
    pub items: Vec<ScaffoldBaselineItemReport>,
    pub created: usize,
    pub overwritten: usize,
    pub skipped: usize,
    pub planned_create: usize,
    pub planned_overwrite: usize,
    pub planned_skip: usize,
}

impl ScaffoldBaselineReport {
    fn from_items(
        request: &ScaffoldBaselineRequest,
        roots: &ResolvedRoots,
        items: Vec<ScaffoldBaselineItemReport>,
    ) -> Self {
        let created = items
            .iter()
            .filter(|item| matches!(item.action, ScaffoldBaselineAction::Created))
            .count();
        let overwritten = items
            .iter()
            .filter(|item| matches!(item.action, ScaffoldBaselineAction::Overwritten))
            .count();
        let skipped = items
            .iter()
            .filter(|item| matches!(item.action, ScaffoldBaselineAction::Skipped))
            .count();
        let planned_create = items
            .iter()
            .filter(|item| matches!(item.action, ScaffoldBaselineAction::PlannedCreate))
            .count();
        let planned_overwrite = items
            .iter()
            .filter(|item| matches!(item.action, ScaffoldBaselineAction::PlannedOverwrite))
            .count();
        let planned_skip = items
            .iter()
            .filter(|item| matches!(item.action, ScaffoldBaselineAction::PlannedSkip))
            .count();

        Self {
            target: request.target,
            missing_only: request.missing_only,
            force: request.force,
            dry_run: request.dry_run,
            docs_home: roots.docs_home.clone(),
            project_path: roots.project_path.clone(),
            items,
            created,
            overwritten,
            skipped,
            planned_create,
            planned_overwrite,
            planned_skip,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldBaselineErrorKind {
    Io,
}

impl ScaffoldBaselineErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
        }
    }
}

impl fmt::Display for ScaffoldBaselineErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldBaselineError {
    pub kind: ScaffoldBaselineErrorKind,
    pub path: PathBuf,
    pub message: String,
}

impl ScaffoldBaselineError {
    fn io(path: PathBuf, message: impl Into<String>) -> Self {
        Self {
            kind: ScaffoldBaselineErrorKind::Io,
            path,
            message: message.into(),
        }
    }
}

impl fmt::Display for ScaffoldBaselineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} [{}]: {}",
            self.path.display(),
            self.kind,
            self.message
        )
    }
}

impl std::error::Error for ScaffoldBaselineError {}

pub fn scaffold_baseline(
    request: &ScaffoldBaselineRequest,
    roots: &ResolvedRoots,
) -> Result<ScaffoldBaselineReport, ScaffoldBaselineError> {
    let mut items = Vec::new();
    for candidate in collect_candidates(request.target, roots) {
        items.push(scaffold_candidate(request, &candidate)?);
    }

    Ok(ScaffoldBaselineReport::from_items(request, roots, items))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaselineTemplate {
    Agents,
    Development,
    CliTools,
}

#[derive(Debug, Clone)]
struct BaselineCandidate {
    scope: Scope,
    context: Context,
    label: &'static str,
    path: PathBuf,
    root: PathBuf,
    template: BaselineTemplate,
}

fn collect_candidates(target: BaselineTarget, roots: &ResolvedRoots) -> Vec<BaselineCandidate> {
    let mut candidates = Vec::new();
    match target {
        BaselineTarget::Home => candidates.extend(home_candidates(roots)),
        BaselineTarget::Project => candidates.extend(project_candidates(roots)),
        BaselineTarget::All => {
            candidates.extend(home_candidates(roots));
            candidates.extend(project_candidates(roots));
        }
    }
    candidates
}

fn home_candidates(roots: &ResolvedRoots) -> Vec<BaselineCandidate> {
    vec![
        candidate(
            Scope::Home,
            Context::Startup,
            "startup policy",
            &roots.docs_home,
            AGENTS_FILE_NAME,
            BaselineTemplate::Agents,
        ),
        candidate(
            Scope::Home,
            Context::SkillDev,
            "skill-dev",
            &roots.docs_home,
            DEVELOPMENT_FILE_NAME,
            BaselineTemplate::Development,
        ),
        candidate(
            Scope::Home,
            Context::TaskTools,
            "task-tools",
            &roots.docs_home,
            CLI_TOOLS_FILE_NAME,
            BaselineTemplate::CliTools,
        ),
    ]
}

fn project_candidates(roots: &ResolvedRoots) -> Vec<BaselineCandidate> {
    vec![
        candidate(
            Scope::Project,
            Context::Startup,
            "startup policy",
            &roots.project_path,
            AGENTS_FILE_NAME,
            BaselineTemplate::Agents,
        ),
        candidate(
            Scope::Project,
            Context::ProjectDev,
            "project-dev",
            &roots.project_path,
            DEVELOPMENT_FILE_NAME,
            BaselineTemplate::Development,
        ),
    ]
}

fn candidate(
    scope: Scope,
    context: Context,
    label: &'static str,
    root: &Path,
    file_name: &str,
    template: BaselineTemplate,
) -> BaselineCandidate {
    BaselineCandidate {
        scope,
        context,
        label,
        path: normalize_path(&root.join(file_name)),
        root: root.to_path_buf(),
        template,
    }
}

fn scaffold_candidate(
    request: &ScaffoldBaselineRequest,
    candidate: &BaselineCandidate,
) -> Result<ScaffoldBaselineItemReport, ScaffoldBaselineError> {
    let existed_before = candidate.path.exists();
    if existed_before && request.missing_only {
        return Ok(report_item(
            candidate,
            if request.dry_run {
                ScaffoldBaselineAction::PlannedSkip
            } else {
                ScaffoldBaselineAction::Skipped
            },
            if request.dry_run {
                "dry-run: would skip existing file because --missing-only is set".to_string()
            } else {
                "skipped existing file because --missing-only is set".to_string()
            },
        ));
    }

    if existed_before && !request.force {
        return Ok(report_item(
            candidate,
            if request.dry_run {
                ScaffoldBaselineAction::PlannedSkip
            } else {
                ScaffoldBaselineAction::Skipped
            },
            if request.dry_run {
                "dry-run: would skip existing file; pass --force to overwrite".to_string()
            } else {
                "skipped existing file; pass --force to overwrite".to_string()
            },
        ));
    }

    if request.dry_run {
        let action = if existed_before {
            ScaffoldBaselineAction::PlannedOverwrite
        } else {
            ScaffoldBaselineAction::PlannedCreate
        };
        let reason = if existed_before {
            format!(
                "dry-run: would overwrite {} from default template",
                candidate.label
            )
        } else {
            format!(
                "dry-run: would create {} from default template",
                candidate.label
            )
        };
        return Ok(report_item(candidate, action, reason));
    }

    let body = render_template(candidate);
    ensure_parent_dir(&candidate.path)?;
    fs::write(&candidate.path, body).map_err(|err| {
        ScaffoldBaselineError::io(
            candidate.path.clone(),
            format!("failed to write baseline document: {err}"),
        )
    })?;

    let action = if existed_before {
        ScaffoldBaselineAction::Overwritten
    } else {
        ScaffoldBaselineAction::Created
    };
    let reason = if existed_before {
        format!("overwrote {} from default template", candidate.label)
    } else {
        format!("created {} from default template", candidate.label)
    };

    Ok(report_item(candidate, action, reason))
}

fn report_item(
    candidate: &BaselineCandidate,
    action: ScaffoldBaselineAction,
    reason: String,
) -> ScaffoldBaselineItemReport {
    ScaffoldBaselineItemReport {
        scope: candidate.scope,
        context: candidate.context,
        label: candidate.label.to_string(),
        path: candidate.path.clone(),
        action,
        reason,
    }
}

fn render_template(candidate: &BaselineCandidate) -> String {
    match candidate.template {
        BaselineTemplate::Agents => scaffold_agents::default_template().to_string(),
        BaselineTemplate::Development => render_with_commands(
            DEVELOPMENT_TEMPLATE,
            &detect_workflow_commands(&candidate.root),
        ),
        BaselineTemplate::CliTools => render_with_commands(
            CLI_TOOLS_TEMPLATE,
            &detect_workflow_commands(&candidate.root),
        ),
    }
}

fn render_with_commands(template: &str, commands: &WorkflowCommands) -> String {
    template
        .replace(SETUP_PLACEHOLDER, &commands.setup.join("\n"))
        .replace(BUILD_PLACEHOLDER, &commands.build.join("\n"))
        .replace(TEST_PLACEHOLDER, &commands.test.join("\n"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkflowCommands {
    setup: Vec<String>,
    build: Vec<String>,
    test: Vec<String>,
}

fn detect_workflow_commands(root: &Path) -> WorkflowCommands {
    if root.join("Cargo.toml").exists() {
        let mut test = Vec::new();
        if root.join(CHECKS_SCRIPT_PATH).exists() {
            test.push(format!("./{CHECKS_SCRIPT_PATH}"));
        }
        test.push("cargo fmt --all -- --check".to_string());
        test.push("cargo clippy --all-targets --all-features -- -D warnings".to_string());
        test.push("cargo test --workspace".to_string());

        return WorkflowCommands {
            setup: vec!["cargo fetch".to_string()],
            build: vec!["cargo build --workspace".to_string()],
            test,
        };
    }

    WorkflowCommands {
        setup: vec!["echo \"Define setup command for this repository\"".to_string()],
        build: vec!["echo \"Define build command for this repository\"".to_string()],
        test: vec!["echo \"Define test command for this repository\"".to_string()],
    }
}

fn ensure_parent_dir(path: &Path) -> Result<(), ScaffoldBaselineError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }

    fs::create_dir_all(parent).map_err(|err| {
        ScaffoldBaselineError::io(
            path.to_path_buf(),
            format!(
                "failed to create parent directory {}: {err}",
                parent.display()
            ),
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn roots(home: &TempDir, project: &TempDir) -> ResolvedRoots {
        ResolvedRoots {
            docs_home: home.path().to_path_buf(),
            project_path: project.path().to_path_buf(),
            is_linked_worktree: false,
            git_common_dir: None,
            primary_worktree_path: None,
        }
    }

    fn item_for<'a>(
        report: &'a ScaffoldBaselineReport,
        scope: Scope,
        file_name: &str,
    ) -> &'a ScaffoldBaselineItemReport {
        report
            .items
            .iter()
            .find(|item| {
                item.scope == scope
                    && item.path.file_name().and_then(|value| value.to_str()) == Some(file_name)
            })
            .expect("expected report item")
    }

    #[test]
    fn scaffold_baseline_missing_only_creates_only_missing_project_documents() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        fs::write(
            project.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .expect("seed cargo file");
        fs::write(project.path().join("AGENTS.md"), "# custom\n").expect("seed agents");

        let request = ScaffoldBaselineRequest {
            target: BaselineTarget::Project,
            missing_only: true,
            force: true,
            dry_run: false,
        };

        let report = scaffold_baseline(&request, &roots(&home, &project)).expect("scaffold");
        assert_eq!(report.items.len(), 2);
        assert_eq!(report.created, 1);
        assert_eq!(report.overwritten, 0);
        assert_eq!(report.skipped, 1);

        let agents = item_for(&report, Scope::Project, AGENTS_FILE_NAME);
        assert_eq!(agents.action, ScaffoldBaselineAction::Skipped);
        assert!(agents.reason.contains("--missing-only"));
        let development = item_for(&report, Scope::Project, DEVELOPMENT_FILE_NAME);
        assert_eq!(development.action, ScaffoldBaselineAction::Created);
        assert_eq!(
            fs::read_to_string(project.path().join("AGENTS.md")).expect("read agents"),
            "# custom\n"
        );
        let written =
            fs::read_to_string(project.path().join("DEVELOPMENT.md")).expect("read development");
        assert!(written.contains("cargo fetch"));
        assert!(written.contains("cargo build --workspace"));
        assert!(written.contains("cargo test --workspace"));
    }

    #[test]
    fn scaffold_baseline_skips_existing_without_force() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        fs::write(project.path().join("AGENTS.md"), "# existing agents\n").expect("seed agents");
        fs::write(project.path().join("DEVELOPMENT.md"), "# existing dev\n")
            .expect("seed development");

        let request = ScaffoldBaselineRequest {
            target: BaselineTarget::Project,
            missing_only: false,
            force: false,
            dry_run: false,
        };

        let report = scaffold_baseline(&request, &roots(&home, &project)).expect("scaffold");
        assert_eq!(report.created, 0);
        assert_eq!(report.overwritten, 0);
        assert_eq!(report.skipped, 2);
        assert_eq!(
            item_for(&report, Scope::Project, AGENTS_FILE_NAME).action,
            ScaffoldBaselineAction::Skipped
        );
        assert_eq!(
            item_for(&report, Scope::Project, DEVELOPMENT_FILE_NAME).action,
            ScaffoldBaselineAction::Skipped
        );
        assert_eq!(
            fs::read_to_string(project.path().join("AGENTS.md")).expect("read agents"),
            "# existing agents\n"
        );
        assert_eq!(
            fs::read_to_string(project.path().join("DEVELOPMENT.md")).expect("read development"),
            "# existing dev\n"
        );
    }

    #[test]
    fn scaffold_baseline_force_overwrites_existing_documents() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        fs::write(
            project.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .expect("seed cargo file");
        fs::write(project.path().join("AGENTS.md"), "# stale agents\n").expect("seed agents");
        fs::write(project.path().join("DEVELOPMENT.md"), "# stale dev\n")
            .expect("seed development");

        let request = ScaffoldBaselineRequest {
            target: BaselineTarget::Project,
            missing_only: false,
            force: true,
            dry_run: false,
        };

        let report = scaffold_baseline(&request, &roots(&home, &project)).expect("scaffold");
        assert_eq!(report.created, 0);
        assert_eq!(report.overwritten, 2);
        assert_eq!(report.skipped, 0);
        assert_eq!(
            item_for(&report, Scope::Project, AGENTS_FILE_NAME).action,
            ScaffoldBaselineAction::Overwritten
        );
        assert_eq!(
            item_for(&report, Scope::Project, DEVELOPMENT_FILE_NAME).action,
            ScaffoldBaselineAction::Overwritten
        );

        let agents_written =
            fs::read_to_string(project.path().join("AGENTS.md")).expect("read agents");
        assert_eq!(agents_written, scaffold_agents::default_template());
        let development_written =
            fs::read_to_string(project.path().join("DEVELOPMENT.md")).expect("read development");
        assert!(development_written.contains("cargo fmt --all -- --check"));
        assert!(
            development_written
                .contains("cargo clippy --all-targets --all-features -- -D warnings")
        );
        assert!(development_written.contains("cargo test --workspace"));
    }

    #[test]
    fn scaffold_baseline_dry_run_reports_plan_without_writing() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        fs::write(home.path().join("AGENTS.md"), "# existing home agents\n")
            .expect("seed home agents");
        fs::write(
            project.path().join("DEVELOPMENT.md"),
            "# existing project dev\n",
        )
        .expect("seed project development");

        let request = ScaffoldBaselineRequest {
            target: BaselineTarget::All,
            missing_only: false,
            force: true,
            dry_run: true,
        };

        let report = scaffold_baseline(&request, &roots(&home, &project)).expect("scaffold");
        assert_eq!(report.items.len(), 5);
        assert_eq!(report.created, 0);
        assert_eq!(report.overwritten, 0);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.planned_create, 3);
        assert_eq!(report.planned_overwrite, 2);
        assert_eq!(report.planned_skip, 0);

        assert_eq!(
            fs::read_to_string(home.path().join("AGENTS.md")).expect("read home agents"),
            "# existing home agents\n"
        );
        assert_eq!(
            fs::read_to_string(project.path().join("DEVELOPMENT.md"))
                .expect("read project development"),
            "# existing project dev\n"
        );
        assert!(!home.path().join("DEVELOPMENT.md").exists());
        assert!(!home.path().join("CLI_TOOLS.md").exists());
        assert!(!project.path().join("AGENTS.md").exists());
    }

    #[test]
    fn scaffold_baseline_uses_checks_script_when_present_for_cargo_projects() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        fs::write(
            project.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .expect("seed cargo file");
        let checks_script = project.path().join(CHECKS_SCRIPT_PATH);
        fs::create_dir_all(
            checks_script
                .parent()
                .expect("checks script should have parent"),
        )
        .expect("create checks script parent");
        fs::write(&checks_script, "#!/usr/bin/env bash\nexit 0\n").expect("seed checks script");

        let request = ScaffoldBaselineRequest {
            target: BaselineTarget::Project,
            missing_only: false,
            force: false,
            dry_run: false,
        };

        scaffold_baseline(&request, &roots(&home, &project)).expect("scaffold");
        let development_written =
            fs::read_to_string(project.path().join("DEVELOPMENT.md")).expect("read development");
        assert!(
            development_written
                .contains("./.agents/skills/nils-cli-verify-required-checks/scripts/nils-cli-verify-required-checks.sh")
        );
        assert!(development_written.contains("cargo test --workspace"));
    }
}
