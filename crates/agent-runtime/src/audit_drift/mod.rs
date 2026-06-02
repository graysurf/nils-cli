//! Minimal `audit-drift` body covering the four blocking classes the
//! Phase 2 reporting POC depends on. See
//! `agent-runtime-kit/docs/source/inventory-target-architecture.md` for
//! the class definitions and Resolved Decisions #5 (no `$AGENT_HOME`)
//! and #9 (determinism / per-product docs-home).
//!
//! Exit code policy:
//!
//! - `0` — no findings.
//! - `1` — only `warn`-tier findings (source-manifest validity,
//!   rendered-target diff).
//! - `2` — any `block`-tier finding (`$AGENT_HOME` leak, docs-home
//!   per product).
//!
//! Plan 04 expands the matrix with unsafe scoring, unsafe allowlist
//! demotion, `extra`, `intentional-difference`, and root-map checks.

use crate::render::manifest::SourceRoot;
use anyhow::Result;
use serde::Serialize;
use std::path::PathBuf;

pub mod agent_home_leak;
pub mod allowlist;
pub mod classes;
pub mod docs_home;
pub mod rendered_target;
pub mod source_manifest;
pub mod unsafe_score;
pub mod walk;

pub const PRODUCTS: &[&str] = &["codex", "claude"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Drift that is only visible in verbose output and never affects
    /// the exit code.
    Suppressed,
    /// Drift that should be reported but never affects the exit code.
    Info,
    /// Drift that the reporting POC will surface but not block on.
    Warn,
    /// Drift that breaks an explicit Resolved Decision contract.
    Block,
}

impl Severity {
    pub fn exit_code(self) -> u8 {
        match self {
            Severity::Suppressed => 0,
            Severity::Info => 0,
            Severity::Warn => 1,
            Severity::Block => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Severity::Suppressed => "suppressed",
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Block => "block",
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Stable class name (e.g. `agent-home-leak`). Matches the source
    /// doc's class taxonomy.
    pub class: &'static str,
    pub severity: Severity,
    /// Optional product context. `None` for product-agnostic classes
    /// (source-manifest validity, source-tree `$AGENT_HOME` leak).
    pub product: Option<String>,
    /// File the finding is attached to, relative to the source root
    /// when possible. Absolute paths are preserved when the location
    /// is outside the source root (rare; only the source-doc allowlist
    /// today).
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct DriftReport {
    pub findings: Vec<Finding>,
}

impl DriftReport {
    pub fn exit_code(&self) -> u8 {
        self.findings
            .iter()
            .map(|f| f.severity.exit_code())
            .max()
            .unwrap_or(0)
    }

    pub fn push(&mut self, finding: Finding) {
        self.findings.push(finding);
    }

    /// Sort findings into a stable order so two runs against the same
    /// source root print the same line sequence. Class first (the
    /// taxonomy ordering matters more to readers than path ordering),
    /// then severity, then product, then path, then message.
    pub fn sort(&mut self) {
        self.findings.sort_by(|a, b| {
            a.class
                .cmp(b.class)
                .then_with(|| a.severity.label().cmp(b.severity.label()))
                .then_with(|| a.product.cmp(&b.product))
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.message.cmp(&b.message))
        });
    }
}

/// Run every class against the source root and return the aggregated
/// report. Each class is best-effort: a class that errors at the I/O
/// layer (e.g. unreadable file) propagates as `Err`; a class that
/// surfaces a *finding* (manifest schema mismatch, `<TBD>` placeholder,
/// `$AGENT_HOME` substring) records it in the report and continues.
pub fn run(root: &SourceRoot) -> Result<DriftReport> {
    let mut report = DriftReport::default();
    let allowlist = allowlist::load(root)?;

    let manifests = source_manifest::check(root, &mut report)?;

    for product in PRODUCTS {
        rendered_target::check(root, manifests.as_deref(), product, &mut report)?;
        agent_home_leak::check_product_build(root, product, &mut report)?;
        docs_home::check(root, product, &mut report)?;
        classes::extra::check(root, manifests.as_deref(), product, &mut report)?;
    }
    classes::intentional::check(root, manifests.as_deref(), &mut report)?;
    classes::plugin_manifest_skills::check(root, manifests.as_deref(), &mut report)?;
    agent_home_leak::check_source_tree(root, &mut report)?;
    unsafe_score::check(root, &mut report)?;
    allowlist.apply(&mut report);

    report.sort();
    Ok(report)
}

#[cfg(test)]
mod tests {
    //! Severity aggregation + exit-code policy. The integration tests
    //! cover each class firing in isolation; these unit tests prove
    //! the `.max()` aggregation when multiple severities are
    //! present, which no integration test would otherwise exercise.

    use super::*;
    use std::path::PathBuf;

    fn finding(class: &'static str, severity: Severity) -> Finding {
        Finding {
            class,
            severity,
            product: None,
            path: PathBuf::from("test"),
            message: String::new(),
        }
    }

    #[test]
    fn empty_report_exits_zero() {
        let report = DriftReport::default();
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn only_warn_findings_exit_one() {
        let mut report = DriftReport::default();
        report.push(finding("source-manifest", Severity::Warn));
        report.push(finding("rendered-target", Severity::Warn));
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn any_block_finding_exits_two() {
        let mut report = DriftReport::default();
        report.push(finding("agent-home-leak", Severity::Block));
        assert_eq!(report.exit_code(), 2);
    }

    #[test]
    fn mixed_severities_take_the_max() {
        // Block must win over Warn — the `.max()` aggregation in
        // `DriftReport::exit_code()` is the contract Phase 2 reporting
        // pins against; a regression to first-wins or warn-priority
        // would break the reporting POC's gating policy.
        let mut report = DriftReport::default();
        report.push(finding("source-manifest", Severity::Warn));
        report.push(finding("agent-home-leak", Severity::Block));
        report.push(finding("rendered-target", Severity::Warn));
        assert_eq!(report.exit_code(), 2);
    }

    #[test]
    fn severity_exit_codes_match_documented_policy() {
        assert_eq!(Severity::Suppressed.exit_code(), 0);
        assert_eq!(Severity::Info.exit_code(), 0);
        assert_eq!(Severity::Warn.exit_code(), 1);
        assert_eq!(Severity::Block.exit_code(), 2);
        assert_eq!(Severity::Suppressed.label(), "suppressed");
        assert_eq!(Severity::Info.label(), "info");
        assert_eq!(Severity::Warn.label(), "warn");
        assert_eq!(Severity::Block.label(), "block");
    }
}
