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

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::TempDir;

    fn write_script(root: &Path, script: &str, executable: bool) -> PathBuf {
        let path = root.join(".agents").join("scripts").join(script);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        if executable {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let _ = executable;
        path
    }

    #[test]
    fn status_labels_are_the_published_wire_values() {
        assert_eq!(ProjectOverlayStatus::Wired.as_str(), "wired");
        assert_eq!(ProjectOverlayStatus::Missing.as_str(), "missing");
    }

    #[test]
    fn every_declared_overlay_script_is_probed_exactly_once() {
        let tmp = TempDir::new().unwrap();

        let findings = probe_project(tmp.path());

        assert_eq!(findings.len(), PROJECT_OVERLAY_SCRIPTS.len());
        assert_eq!(
            findings
                .iter()
                .map(|f| f.script.as_str())
                .collect::<Vec<_>>(),
            PROJECT_OVERLAY_SCRIPTS.to_vec(),
            "probe order must follow the declared script list"
        );
        assert!(
            findings
                .iter()
                .all(|f| f.status == ProjectOverlayStatus::Missing
                    && f.severity == DoctorSeverity::Warn),
            "an un-adopted project warns rather than blocks"
        );
        assert_eq!(findings[0].message, "project-local script is missing");
        assert!(findings[0].path.ends_with(".agents/scripts/bench.sh"));
    }

    #[cfg(unix)]
    #[test]
    fn an_executable_script_is_wired_and_a_non_executable_one_is_not() {
        let tmp = TempDir::new().unwrap();
        write_script(tmp.path(), "deploy.sh", true);
        write_script(tmp.path(), "release.sh", false);

        let findings = probe_project(tmp.path());
        let by_script = |name: &str| {
            findings
                .iter()
                .find(|f| f.script == name)
                .unwrap_or_else(|| panic!("finding for {name}"))
        };

        let deploy = by_script("deploy");
        assert_eq!(deploy.status, ProjectOverlayStatus::Wired);
        assert_eq!(deploy.severity, DoctorSeverity::Ok);
        assert_eq!(
            deploy.message,
            "project-local script exists and is executable"
        );

        // Present but not executable is a distinct, actionable diagnosis.
        let release = by_script("release");
        assert_eq!(release.status, ProjectOverlayStatus::Missing);
        assert_eq!(release.severity, DoctorSeverity::Warn);
        assert_eq!(
            release.message,
            "project-local script exists but is not executable"
        );
    }

    #[test]
    fn a_directory_in_the_script_slot_is_never_wired() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".agents").join("scripts").join("demo.sh")).unwrap();

        let findings = probe_project(tmp.path());
        let demo = findings.iter().find(|f| f.script == "demo").expect("demo");

        assert_eq!(demo.status, ProjectOverlayStatus::Missing);
        assert_eq!(
            demo.message,
            "project-local script exists but is not executable"
        );
    }

    #[test]
    fn findings_render_into_the_shared_doctor_shape() {
        let finding = ProjectOverlayFinding {
            script: "deploy".to_string(),
            status: ProjectOverlayStatus::Wired,
            severity: DoctorSeverity::Ok,
            path: PathBuf::from("/repo/.agents/scripts/deploy.sh"),
            message: "project-local script exists and is executable".to_string(),
        };

        let doctor = finding.to_doctor_finding("codex");
        assert_eq!(doctor.product, "codex");
        assert_eq!(doctor.check, "project-overlay");
        assert_eq!(doctor.severity, DoctorSeverity::Ok);
        assert_eq!(doctor.entry_id.as_deref(), Some("deploy"));
        assert_eq!(
            doctor.message,
            "status=wired: project-local script exists and is executable"
        );

        let warn = ProjectOverlayFinding {
            status: ProjectOverlayStatus::Missing,
            severity: DoctorSeverity::Warn,
            message: "project-local script is missing".to_string(),
            ..finding.clone()
        }
        .to_doctor_finding("claude");
        assert_eq!(warn.severity, DoctorSeverity::Warn);
        assert_eq!(
            warn.message,
            "status=missing: project-local script is missing"
        );

        let block = ProjectOverlayFinding {
            severity: DoctorSeverity::Block,
            ..finding
        }
        .to_doctor_finding("claude");
        assert_eq!(block.severity, DoctorSeverity::Block);
        assert_eq!(block.check, "project-overlay");
    }
}
