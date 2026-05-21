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
