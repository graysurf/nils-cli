use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::env::ResolvedRoots;
use crate::model::Scope;

const DEFAULT_AGENTS_FILE_NAME: &str = "AGENTS.md";
const DEFAULT_TEMPLATE: &str = include_str!("../templates/agents_default.md");

#[derive(Debug, Clone)]
pub struct ScaffoldAgentsRequest {
    pub target: Scope,
    pub output: Option<PathBuf>,
    pub force: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldAgentsWriteMode {
    Created,
    Overwritten,
}

impl ScaffoldAgentsWriteMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Overwritten => "overwritten",
        }
    }
}

impl fmt::Display for ScaffoldAgentsWriteMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldAgentsReport {
    pub target: Scope,
    pub output_path: PathBuf,
    pub write_mode: ScaffoldAgentsWriteMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldAgentsErrorKind {
    AlreadyExists,
    Io,
}

impl ScaffoldAgentsErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyExists => "already-exists",
            Self::Io => "io",
        }
    }
}

impl fmt::Display for ScaffoldAgentsErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldAgentsError {
    pub kind: ScaffoldAgentsErrorKind,
    pub output_path: PathBuf,
    pub message: String,
}

impl ScaffoldAgentsError {
    fn already_exists(output_path: PathBuf) -> Self {
        Self {
            kind: ScaffoldAgentsErrorKind::AlreadyExists,
            output_path,
            message: "output file already exists; pass --force to overwrite".to_string(),
        }
    }

    fn io(output_path: PathBuf, message: impl Into<String>) -> Self {
        Self {
            kind: ScaffoldAgentsErrorKind::Io,
            output_path,
            message: message.into(),
        }
    }
}

impl fmt::Display for ScaffoldAgentsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} [{}]: {}",
            self.output_path.display(),
            self.kind,
            self.message
        )
    }
}

impl std::error::Error for ScaffoldAgentsError {}

pub fn scaffold_agents(
    request: &ScaffoldAgentsRequest,
    roots: &ResolvedRoots,
) -> Result<ScaffoldAgentsReport, ScaffoldAgentsError> {
    let output_path = request
        .output
        .clone()
        .unwrap_or_else(|| default_output_path(request.target, roots));

    let existed_before = output_path.exists();
    if existed_before && !request.force {
        return Err(ScaffoldAgentsError::already_exists(output_path));
    }

    ensure_parent_dir(&output_path)?;
    fs::write(&output_path, default_template()).map_err(|err| {
        ScaffoldAgentsError::io(
            output_path.clone(),
            format!("failed to write AGENTS.md template: {err}"),
        )
    })?;

    Ok(ScaffoldAgentsReport {
        target: request.target,
        output_path,
        write_mode: if existed_before {
            ScaffoldAgentsWriteMode::Overwritten
        } else {
            ScaffoldAgentsWriteMode::Created
        },
    })
}

pub fn default_output_path(target: Scope, roots: &ResolvedRoots) -> PathBuf {
    let base = match target {
        Scope::Home => &roots.docs_home,
        Scope::Project => &roots.project_path,
    };
    base.join(DEFAULT_AGENTS_FILE_NAME)
}

pub const fn default_template() -> &'static str {
    DEFAULT_TEMPLATE
}

fn ensure_parent_dir(output_path: &Path) -> Result<(), ScaffoldAgentsError> {
    let Some(parent) = output_path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(parent).map_err(|err| {
        ScaffoldAgentsError::io(
            output_path.to_path_buf(),
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
    use std::fs;

    use tempfile::TempDir;

    use super::{
        ScaffoldAgentsErrorKind, ScaffoldAgentsRequest, ScaffoldAgentsWriteMode, default_template,
        scaffold_agents,
    };
    use crate::env::ResolvedRoots;
    use crate::model::Scope;

    fn roots(home: &TempDir, project: &TempDir) -> ResolvedRoots {
        ResolvedRoots {
            docs_home: home.path().to_path_buf(),
            project_path: project.path().to_path_buf(),
            is_linked_worktree: false,
            git_common_dir: None,
            primary_worktree_path: None,
        }
    }

    #[test]
    fn scaffold_agents_creates_default_file_when_missing() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        let roots = roots(&home, &project);

        let request = ScaffoldAgentsRequest {
            target: Scope::Project,
            output: None,
            force: false,
        };

        let report = scaffold_agents(&request, &roots).expect("scaffold agents");
        assert_eq!(report.target, Scope::Project);
        assert_eq!(report.output_path, project.path().join("AGENTS.md"));
        assert_eq!(report.write_mode, ScaffoldAgentsWriteMode::Created);

        let written = fs::read_to_string(project.path().join("AGENTS.md")).expect("read output");
        assert_eq!(written, default_template());
    }

    #[test]
    fn scaffold_agents_returns_error_when_target_exists_without_force() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        let roots = roots(&home, &project);
        let output = project.path().join("AGENTS.md");
        fs::write(&output, "# custom\n").expect("seed existing file");

        let request = ScaffoldAgentsRequest {
            target: Scope::Project,
            output: None,
            force: false,
        };

        let err = scaffold_agents(&request, &roots).expect_err("existing target should fail");
        assert_eq!(err.kind, ScaffoldAgentsErrorKind::AlreadyExists);
        assert_eq!(err.output_path, output);
        let persisted = fs::read_to_string(project.path().join("AGENTS.md")).expect("read output");
        assert_eq!(persisted, "# custom\n");
    }

    #[test]
    fn scaffold_agents_overwrites_existing_file_when_forced() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        let roots = roots(&home, &project);
        fs::write(project.path().join("AGENTS.md"), "# stale\n").expect("seed stale file");

        let request = ScaffoldAgentsRequest {
            target: Scope::Project,
            output: None,
            force: true,
        };

        let report = scaffold_agents(&request, &roots).expect("forced overwrite");
        assert_eq!(report.write_mode, ScaffoldAgentsWriteMode::Overwritten);
        let written = fs::read_to_string(project.path().join("AGENTS.md")).expect("read output");
        assert_eq!(written, default_template());
    }

    #[test]
    fn scaffold_agents_supports_explicit_output_path() {
        let home = TempDir::new().expect("create home tempdir");
        let project = TempDir::new().expect("create project tempdir");
        let roots = roots(&home, &project);
        let explicit_output = project.path().join("nested").join("custom-agents.md");

        let request = ScaffoldAgentsRequest {
            target: Scope::Home,
            output: Some(explicit_output.clone()),
            force: false,
        };

        let report = scaffold_agents(&request, &roots).expect("explicit output");
        assert_eq!(report.output_path, explicit_output);
        let written = fs::read_to_string(project.path().join("nested").join("custom-agents.md"))
            .expect("read output");
        assert_eq!(written, default_template());
    }

    #[test]
    fn default_template_contains_required_guidance() {
        let template = default_template();
        assert!(template.contains("agent-docs resolve --context startup"));
        assert!(template.contains("agent-docs resolve --context project-dev"));
        assert!(template.contains("AGENT_DOCS.toml"));
    }
}
