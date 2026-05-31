//! Integration coverage for Plan 04 Sprint 3 Task 3.2 version probes.

use agent_runtime::doctor::DoctorSeverity;
use agent_runtime::doctor::version::{self, VersionProbeInput, VersionStatus};

fn input(
    output: &str,
    min_version: &str,
    recommended_version: &str,
    effective_from: &str,
) -> VersionProbeInput {
    VersionProbeInput {
        product: "codex".to_string(),
        command: "codex --version".to_string(),
        min_version: min_version.to_string(),
        recommended_version: recommended_version.to_string(),
        min_version_effective_from: effective_from.to_string(),
        raw_output: output.to_string(),
        today: "2026-05-21".to_string(),
    }
}

#[test]
fn version_above_recommended_reports_ok() {
    let finding = version::classify(input(
        "codex 0.18.2 (build abc1234)",
        "0.18.0",
        "0.18.1",
        "2026-05-01",
    ));

    assert_eq!(finding.status, VersionStatus::Ok);
    assert_eq!(finding.severity, DoctorSeverity::Ok);
    assert_eq!(finding.parsed_version.as_deref(), Some("0.18.2"));
}

#[test]
fn version_between_minimum_and_recommended_reports_recommended_only() {
    let finding = version::classify(input("codex-cli v0.18.0", "0.18.0", "0.18.2", "2026-05-01"));

    assert_eq!(finding.status, VersionStatus::RecommendedOnly);
    assert_eq!(finding.severity, DoctorSeverity::Warn);
    assert_eq!(finding.parsed_version.as_deref(), Some("0.18.0"));
}

#[test]
fn version_below_minimum_before_effective_date_warns() {
    let finding = version::classify(input("codex 0.17.9", "0.18.0", "0.18.2", "2026-06-03"));

    assert_eq!(finding.status, VersionStatus::Warn);
    assert_eq!(finding.severity, DoctorSeverity::Warn);
}

#[test]
fn version_below_minimum_after_effective_date_blocks() {
    let finding = version::classify(input("codex 0.17.9", "0.18.0", "0.18.2", "2026-05-21"));

    assert_eq!(finding.status, VersionStatus::Outdated);
    assert_eq!(finding.severity, DoctorSeverity::Block);
}

#[test]
fn unparseable_version_output_warns_with_raw_output() {
    let finding = version::classify(input(
        "codex development build",
        "0.18.0",
        "0.18.2",
        "2026-05-21",
    ));

    assert_eq!(finding.status, VersionStatus::Unparseable);
    assert_eq!(finding.severity, DoctorSeverity::Warn);
    assert_eq!(finding.raw_output, "codex development build");
}
