//! Project-local overlay coverage for `agent-runtime doctor --check-project`.

use super::{DoctorFinding, DoctorSeverity};
use std::path::{Path, PathBuf};

const PROJECT_OVERLAY_SCRIPTS: &[&str] =
    &["bench", "bootstrap", "demo", "deploy", "pre-pr", "release"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectOverlayStatus {
    Wired,
    Missing,
}

impl ProjectOverlayStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectOverlayStatus::Wired => "wired",
            ProjectOverlayStatus::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectOverlayFinding {
    pub script: String,
    pub status: ProjectOverlayStatus,
    pub severity: DoctorSeverity,
    pub path: PathBuf,
    pub message: String,
}

impl ProjectOverlayFinding {
    pub fn to_doctor_finding(&self, product: &str) -> DoctorFinding {
        let message = format!("status={}: {}", self.status.as_str(), self.message);
        match self.severity {
            DoctorSeverity::Ok => DoctorFinding {
                product: product.to_string(),
                check: "project-overlay",
                severity: DoctorSeverity::Ok,
                entry_id: Some(self.script.clone()),
                path: Some(self.path.clone()),
                message,
            },
            DoctorSeverity::Warn => DoctorFinding::warn(
                product,
                "project-overlay",
                Some(self.script.clone()),
                Some(self.path.clone()),
                message,
            ),
            DoctorSeverity::Block => DoctorFinding::block(
                product,
                "project-overlay",
                Some(self.script.clone()),
                Some(self.path.clone()),
                message,
            ),
        }
    }
}

pub fn probe_project(project_root: &Path) -> Vec<ProjectOverlayFinding> {
    PROJECT_OVERLAY_SCRIPTS
        .iter()
        .map(|script| probe_script(project_root, script))
        .collect()
}

fn probe_script(project_root: &Path, script: &str) -> ProjectOverlayFinding {
    let path = project_root
        .join(".agents")
        .join("scripts")
        .join(format!("{script}.sh"));
    if is_executable_file(&path) {
        ProjectOverlayFinding {
            script: script.to_string(),
            status: ProjectOverlayStatus::Wired,
            severity: DoctorSeverity::Ok,
            path,
            message: "project-local script exists and is executable".to_string(),
        }
    } else {
        let message = if path.exists() {
            "project-local script exists but is not executable"
        } else {
            "project-local script is missing"
        };
        ProjectOverlayFinding {
            script: script.to_string(),
            status: ProjectOverlayStatus::Missing,
            severity: DoctorSeverity::Warn,
            path,
            message: message.to_string(),
        }
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
