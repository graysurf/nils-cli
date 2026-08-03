use super::{DoctorFinding, ResolvedRuntimeRoots};
use crate::doctor::DoctorSeverity;
use crate::install::link_map::CommentStyle;
use crate::install::plan::{InstallPlan, PlanAction};
use crate::managed_block::{CommentStyle as ManagedBlockStyle, ManagedBlock};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct ProbeReport {
    pub ok: usize,
    pub findings: Vec<DoctorFinding>,
}

impl ProbeReport {
    pub fn extend(&mut self, other: ProbeReport) {
        self.ok += other.ok;
        self.findings.extend(other.findings);
    }

    fn ok(&mut self) {
        self.ok += 1;
    }

    fn push(&mut self, finding: DoctorFinding) {
        self.findings.push(finding);
    }
}

pub fn runtime_roots(roots: &ResolvedRuntimeRoots) -> ProbeReport {
    let mut report = ProbeReport::default();
    check_dir(
        &mut report,
        &roots.product,
        "runtime-root.live_home",
        &roots.live_home,
        DoctorSeverity::Block,
    );
    check_dir(
        &mut report,
        &roots.product,
        "runtime-root.docs_home",
        &roots.docs_home,
        DoctorSeverity::Warn,
    );
    check_dir(
        &mut report,
        &roots.product,
        "runtime-root.state_home",
        &roots.state_home,
        DoctorSeverity::Warn,
    );
    if let Some(plugin_root) = roots.plugin_root.as_ref() {
        check_dir(
            &mut report,
            &roots.product,
            "runtime-root.plugin_root",
            plugin_root,
            DoctorSeverity::Warn,
        );
    }
    report
}

pub fn install_plan(product: &str, plan: &InstallPlan) -> ProbeReport {
    let mut report = ProbeReport::default();
    for action in &plan.actions {
        match action {
            PlanAction::Symlink {
                entry_id,
                source,
                dest,
                link_mode: _,
                requires_backup: _,
            } => check_symlink(&mut report, product, entry_id, source, dest),
            PlanAction::ManagedBlock {
                entry_id,
                config_file,
                surface,
                comment_style,
                body: _,
            } => check_managed_block(
                &mut report,
                product,
                entry_id,
                config_file,
                surface,
                *comment_style,
            ),
        }
    }
    report
}

fn check_dir(
    report: &mut ProbeReport,
    product: &str,
    check: &'static str,
    path: &Path,
    missing_severity: DoctorSeverity,
) {
    if !path.is_absolute() {
        report.push(finding_for_severity(
            missing_severity,
            product,
            check,
            None,
            Some(path.to_path_buf()),
            "runtime-root path is not absolute after environment expansion",
        ));
        return;
    }

    match fs::metadata(path) {
        Ok(meta) if !meta.is_dir() => report.push(finding_for_severity(
            missing_severity,
            product,
            check,
            None,
            Some(path.to_path_buf()),
            "runtime-root path exists but is not a directory",
        )),
        Ok(_) => match fs::read_dir(path) {
            Ok(_) => report.ok(),
            Err(source) => report.push(finding_for_io(
                missing_severity,
                product,
                check,
                None,
                path,
                "runtime-root path is not readable",
                source,
            )),
        },
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            report.push(finding_for_severity(
                missing_severity,
                product,
                check,
                None,
                Some(path.to_path_buf()),
                "runtime-root path does not exist",
            ));
        }
        Err(source) => report.push(finding_for_io(
            missing_severity,
            product,
            check,
            None,
            path,
            "runtime-root path could not be inspected",
            source,
        )),
    }
}

fn check_symlink(
    report: &mut ProbeReport,
    product: &str,
    entry_id: &str,
    source: &Path,
    dest: &Path,
) {
    if !source.exists() {
        report.push(DoctorFinding::block(
            product,
            "link-map.symlink",
            Some(entry_id.to_string()),
            Some(source.to_path_buf()),
            "tracked source file does not exist",
        ));
        return;
    }

    match fs::symlink_metadata(dest) {
        Ok(meta) if !meta.file_type().is_symlink() => report.push(DoctorFinding::block(
            product,
            "link-map.symlink",
            Some(entry_id.to_string()),
            Some(dest.to_path_buf()),
            "destination exists but is not a symlink",
        )),
        Ok(_) => match fs::read_link(dest) {
            Ok(actual) if actual == source => report.ok(),
            Ok(actual) => report.push(DoctorFinding::block(
                product,
                "link-map.symlink",
                Some(entry_id.to_string()),
                Some(dest.to_path_buf()),
                format!(
                    "destination points at {}; expected {}",
                    actual.display(),
                    source.display()
                ),
            )),
            Err(source) => report.push(DoctorFinding::block(
                product,
                "link-map.symlink",
                Some(entry_id.to_string()),
                Some(dest.to_path_buf()),
                format!("could not read symlink target: {source}"),
            )),
        },
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            report.push(DoctorFinding::block(
                product,
                "link-map.symlink",
                Some(entry_id.to_string()),
                Some(dest.to_path_buf()),
                "destination symlink is missing",
            ));
        }
        Err(source) => report.push(DoctorFinding::block(
            product,
            "link-map.symlink",
            Some(entry_id.to_string()),
            Some(dest.to_path_buf()),
            format!("could not inspect destination: {source}"),
        )),
    }
}

fn check_managed_block(
    report: &mut ProbeReport,
    product: &str,
    entry_id: &str,
    config_file: &Path,
    surface: &str,
    comment_style: CommentStyle,
) {
    let style = match comment_style {
        CommentStyle::Hash => ManagedBlockStyle::Hash,
        CommentStyle::DoubleSlash => ManagedBlockStyle::DoubleSlash,
    };
    let block = ManagedBlock::new(surface.to_string(), style);
    let existing = match fs::read_to_string(config_file) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            report.push(DoctorFinding::block(
                product,
                "managed-block",
                Some(entry_id.to_string()),
                Some(config_file.to_path_buf()),
                "config file is missing",
            ));
            return;
        }
        Err(source) => {
            report.push(DoctorFinding::block(
                product,
                "managed-block",
                Some(entry_id.to_string()),
                Some(config_file.to_path_buf()),
                format!("could not read config file: {source}"),
            ));
            return;
        }
    };

    match block.read(&existing) {
        Ok(Some(_)) => report.ok(),
        Ok(None) => report.push(DoctorFinding::block(
            product,
            "managed-block",
            Some(entry_id.to_string()),
            Some(config_file.to_path_buf()),
            format!("managed-block markers for surface `{surface}` are missing"),
        )),
        Err(source) => report.push(DoctorFinding::block(
            product,
            "managed-block",
            Some(entry_id.to_string()),
            Some(config_file.to_path_buf()),
            source.to_string(),
        )),
    }
}

fn finding_for_io(
    severity: DoctorSeverity,
    product: &str,
    check: &'static str,
    entry_id: Option<String>,
    path: &Path,
    prefix: &str,
    source: io::Error,
) -> DoctorFinding {
    finding_for_severity(
        severity,
        product,
        check,
        entry_id,
        Some(path.to_path_buf()),
        format!("{prefix}: {source}"),
    )
}

fn finding_for_severity(
    severity: DoctorSeverity,
    product: &str,
    check: &'static str,
    entry_id: Option<String>,
    path: Option<PathBuf>,
    message: impl Into<String>,
) -> DoctorFinding {
    match severity {
        DoctorSeverity::Ok => DoctorFinding {
            product: product.to_string(),
            check,
            severity,
            entry_id,
            path,
            message: message.into(),
        },
        DoctorSeverity::Warn => DoctorFinding::warn(product, check, entry_id, path, message),
        DoctorSeverity::Block => DoctorFinding::block(product, check, entry_id, path, message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::install::plan::SymlinkLinkMode;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    const PRODUCT: &str = "codex";

    fn roots(base: &Path, plugin_root: Option<PathBuf>) -> ResolvedRuntimeRoots {
        ResolvedRuntimeRoots {
            product: PRODUCT.to_string(),
            live_home: base.join("live"),
            docs_home: base.join("docs"),
            state_home: base.join("state"),
            plugin_root,
        }
    }

    fn symlink_plan(actions: Vec<PlanAction>) -> InstallPlan {
        InstallPlan {
            product: PRODUCT.to_string(),
            source_root: PathBuf::from("/source"),
            home: PathBuf::from("/home"),
            state_home: PathBuf::from("/state"),
            actions,
        }
    }

    #[test]
    fn probe_reports_accumulate_counts_and_findings() {
        let mut first = ProbeReport::default();
        first.ok();
        first.push(DoctorFinding::warn(
            PRODUCT,
            "runtime-root.docs_home",
            None,
            None,
            "warned",
        ));
        let mut second = ProbeReport::default();
        second.ok();

        first.extend(second);

        assert_eq!(first.ok, 2);
        assert_eq!(first.findings.len(), 1);
    }

    #[test]
    fn every_present_runtime_root_counts_as_ok() {
        let tmp = TempDir::new().unwrap();
        for name in ["live", "docs", "state", "plugins"] {
            fs::create_dir_all(tmp.path().join(name)).unwrap();
        }

        let report = runtime_roots(&roots(tmp.path(), Some(tmp.path().join("plugins"))));

        assert_eq!(report.ok, 4, "all four roots resolve");
        assert!(report.findings.is_empty());
    }

    #[test]
    fn an_absent_plugin_root_is_simply_not_probed() {
        let tmp = TempDir::new().unwrap();
        for name in ["live", "docs", "state"] {
            fs::create_dir_all(tmp.path().join(name)).unwrap();
        }

        let report = runtime_roots(&roots(tmp.path(), None));

        assert_eq!(report.ok, 3);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn a_missing_live_home_blocks_while_the_others_only_warn() {
        let tmp = TempDir::new().unwrap();

        let report = runtime_roots(&roots(tmp.path(), None));

        assert_eq!(report.ok, 0);
        assert_eq!(report.findings.len(), 3);
        let live = report
            .findings
            .iter()
            .find(|f| f.check == "runtime-root.live_home")
            .expect("live_home finding");
        assert_eq!(live.severity, DoctorSeverity::Block);
        assert_eq!(live.message, "runtime-root path does not exist");
        assert!(
            report
                .findings
                .iter()
                .filter(|f| f.check != "runtime-root.live_home")
                .all(|f| f.severity == DoctorSeverity::Warn),
            "only the live home is fatal"
        );
    }

    #[test]
    fn a_relative_runtime_root_is_rejected_before_touching_the_filesystem() {
        let report = runtime_roots(&ResolvedRuntimeRoots {
            product: PRODUCT.to_string(),
            live_home: PathBuf::from("relative/live"),
            docs_home: PathBuf::from("relative/docs"),
            state_home: PathBuf::from("relative/state"),
            plugin_root: None,
        });

        assert_eq!(report.ok, 0);
        assert!(
            report
                .findings
                .iter()
                .all(|f| f.message
                    == "runtime-root path is not absolute after environment expansion"),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn a_runtime_root_that_is_a_file_is_not_a_directory() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("docs")).unwrap();
        fs::create_dir_all(tmp.path().join("state")).unwrap();
        fs::write(tmp.path().join("live"), b"not a dir").unwrap();

        let report = runtime_roots(&roots(tmp.path(), None));

        let live = report
            .findings
            .iter()
            .find(|f| f.check == "runtime-root.live_home")
            .expect("live_home finding");
        assert_eq!(
            live.message,
            "runtime-root path exists but is not a directory"
        );
        assert_eq!(live.severity, DoctorSeverity::Block);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_runtime_root_reports_the_io_error() {
        let tmp = TempDir::new().unwrap();
        for name in ["live", "docs", "state"] {
            fs::create_dir_all(tmp.path().join(name)).unwrap();
        }
        let live = tmp.path().join("live");
        // A directory at 0o000 blocks `remove_dir_all` outright, and the restore
        // below used to be a plain statement, so a panic in `runtime_roots` left
        // teardown a directory it could not empty. Restore from `Drop` instead.
        // Found by the #1411 audit review.
        let restored_live = nils_test_support::tempdir::RestoredMode::set(&live, 0o000);
        let readable_as_root = fs::read_dir(&live).is_ok();
        if readable_as_root {
            return;
        }

        let report = runtime_roots(&roots(tmp.path(), None));
        drop(restored_live);

        let live = report
            .findings
            .iter()
            .find(|f| f.check == "runtime-root.live_home")
            .expect("live_home finding");
        assert!(
            live.message
                .starts_with("runtime-root path is not readable: "),
            "{}",
            live.message
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_correct_symlink_action_counts_as_ok() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source.md");
        let dest = tmp.path().join("dest.md");
        fs::write(&source, b"tracked").unwrap();
        std::os::unix::fs::symlink(&source, &dest).unwrap();

        let report = install_plan(
            PRODUCT,
            &symlink_plan(vec![PlanAction::Symlink {
                entry_id: "entry".to_string(),
                source,
                dest,
                link_mode: SymlinkLinkMode::File,
                requires_backup: false,
            }]),
        );

        assert_eq!(report.ok, 1);
        assert!(report.findings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_drift_is_reported_with_the_observed_target() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source.md");
        let other = tmp.path().join("other.md");
        let dest = tmp.path().join("dest.md");
        fs::write(&source, b"tracked").unwrap();
        fs::write(&other, b"stale").unwrap();
        std::os::unix::fs::symlink(&other, &dest).unwrap();

        let report = install_plan(
            PRODUCT,
            &symlink_plan(vec![PlanAction::Symlink {
                entry_id: "entry".to_string(),
                source: source.clone(),
                dest,
                link_mode: SymlinkLinkMode::File,
                requires_backup: false,
            }]),
        );

        assert_eq!(report.ok, 0);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, DoctorSeverity::Block);
        assert!(
            report.findings[0]
                .message
                .contains(&format!("expected {}", source.display())),
            "{}",
            report.findings[0].message
        );
    }

    #[test]
    fn a_symlink_action_reports_missing_source_destination_and_wrong_type() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source.md");
        let dest = tmp.path().join("dest.md");

        // Source missing: the tracked file is gone from the checkout.
        let report = install_plan(
            PRODUCT,
            &symlink_plan(vec![PlanAction::Symlink {
                entry_id: "entry".to_string(),
                source: source.clone(),
                dest: dest.clone(),
                link_mode: SymlinkLinkMode::File,
                requires_backup: false,
            }]),
        );
        assert_eq!(report.findings.len(), 1);
        assert_eq!(
            report.findings[0].message,
            "tracked source file does not exist"
        );

        // Source present, destination absent.
        fs::write(&source, b"tracked").unwrap();
        let report = install_plan(
            PRODUCT,
            &symlink_plan(vec![PlanAction::Symlink {
                entry_id: "entry".to_string(),
                source: source.clone(),
                dest: dest.clone(),
                link_mode: SymlinkLinkMode::File,
                requires_backup: false,
            }]),
        );
        assert_eq!(report.findings[0].message, "destination symlink is missing");

        // Destination present but a regular file, not a link.
        fs::write(&dest, b"hand-written").unwrap();
        let report = install_plan(
            PRODUCT,
            &symlink_plan(vec![PlanAction::Symlink {
                entry_id: "entry".to_string(),
                source,
                dest,
                link_mode: SymlinkLinkMode::File,
                requires_backup: true,
            }]),
        );
        assert_eq!(
            report.findings[0].message,
            "destination exists but is not a symlink"
        );
    }

    #[test]
    fn a_managed_block_is_ok_only_when_its_markers_are_present() {
        let tmp = TempDir::new().unwrap();
        let config = tmp.path().join("config.toml");
        let block = ManagedBlock::new("surface".to_string(), ManagedBlockStyle::Hash);
        let rendered = block
            .write("existing = true\n", "managed = true\n", true)
            .expect("render managed block");
        fs::write(&config, &rendered).unwrap();

        let action = |file: PathBuf| PlanAction::ManagedBlock {
            entry_id: "entry".to_string(),
            config_file: file,
            surface: "surface".to_string(),
            comment_style: CommentStyle::Hash,
            body: "managed = true\n".to_string(),
        };

        let report = install_plan(PRODUCT, &symlink_plan(vec![action(config.clone())]));
        assert_eq!(report.ok, 1);
        assert!(report.findings.is_empty());

        // Markers stripped: the surface is no longer installed.
        fs::write(&config, b"existing = true\n").unwrap();
        let report = install_plan(PRODUCT, &symlink_plan(vec![action(config)]));
        assert_eq!(report.ok, 0);
        assert_eq!(
            report.findings[0].message,
            "managed-block markers for surface `surface` are missing"
        );

        // The config file itself is gone.
        let report = install_plan(
            PRODUCT,
            &symlink_plan(vec![action(tmp.path().join("absent.toml"))]),
        );
        assert_eq!(report.findings[0].message, "config file is missing");
        assert_eq!(report.findings[0].severity, DoctorSeverity::Block);
    }

    #[test]
    fn a_double_slash_managed_block_uses_the_matching_comment_style() {
        let tmp = TempDir::new().unwrap();
        let config = tmp.path().join("config.js");
        let block = ManagedBlock::new("surface".to_string(), ManagedBlockStyle::DoubleSlash);
        fs::write(
            &config,
            block
                .write("// existing\n", "managed();\n", true)
                .expect("render"),
        )
        .unwrap();

        let report = install_plan(
            PRODUCT,
            &symlink_plan(vec![PlanAction::ManagedBlock {
                entry_id: "entry".to_string(),
                config_file: config.clone(),
                surface: "surface".to_string(),
                comment_style: CommentStyle::DoubleSlash,
                body: "managed();\n".to_string(),
            }]),
        );
        assert_eq!(report.ok, 1);

        // The hash style cannot see a `//`-delimited block, so it is missing.
        let report = install_plan(
            PRODUCT,
            &symlink_plan(vec![PlanAction::ManagedBlock {
                entry_id: "entry".to_string(),
                config_file: config,
                surface: "surface".to_string(),
                comment_style: CommentStyle::Hash,
                body: "managed();\n".to_string(),
            }]),
        );
        assert_eq!(report.ok, 0);
        assert_eq!(report.findings.len(), 1);
    }

    #[test]
    fn an_empty_plan_probes_nothing() {
        let report = install_plan(PRODUCT, &symlink_plan(Vec::new()));

        assert_eq!(report.ok, 0);
        assert!(report.findings.is_empty());
    }
}
