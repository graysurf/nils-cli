//! Integration tests for the `plan-archive validate-*` subcommands.
//!
//! These tests exercise the validators through their public library API
//! and against the shipped fixture set; they intentionally avoid
//! shelling out so they remain fast and runnable without a release
//! binary in PATH.

use std::path::PathBuf;

use plan_archive::validate::hosts::{HostClass, validate_hosts_yaml};
use plan_archive::validate::local::{LocalSource, validate_local_path, validate_local_yaml};
use plan_archive::validate::metadata::validate_metadata_yaml;

fn fixture(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read fixture {}: {}", path.display(), err))
}

#[test]
fn hosts_personal_only_fixture() {
    let v = validate_hosts_yaml(&fixture("hosts/personal-only.yaml")).expect("validation");
    assert_eq!(v.data.summary.host_count, 2);
    assert_eq!(v.data.summary.personal_count, 2);
    assert_eq!(v.data.summary.employer_count, 0);
    let github = v.data.config.hosts.get("github.com").expect("github.com");
    assert!(matches!(github.class, HostClass::Personal));
    assert_eq!(github.primary_identity.as_deref(), Some("graysurf"));
}

#[test]
fn hosts_employer_mixed_fixture() {
    let v = validate_hosts_yaml(&fixture("hosts/employer-mixed.yaml")).expect("validation");
    assert_eq!(v.data.summary.host_count, 3);
    assert_eq!(v.data.summary.employer_count, 1);
    let gitlab = v
        .data
        .config
        .hosts
        .get("gitlab.example.com")
        .expect("gitlab.example.com");
    assert!(matches!(gitlab.class, HostClass::Employer));
    assert_eq!(gitlab.employer.as_deref(), Some("ExampleCorp"));
    assert_eq!(gitlab.retention.as_deref(), Some("delete-on-termination"));
}

#[test]
fn hosts_invalid_unknown_class_fixture() {
    let err = validate_hosts_yaml(&fixture("hosts/invalid-unknown-class.yaml"))
        .expect_err("invalid class");
    assert_eq!(err.code(), "hosts-unknown-class");
}

#[test]
fn hosts_invalid_employer_missing_name_fixture() {
    let err = validate_hosts_yaml(&fixture("hosts/invalid-employer-missing-name.yaml"))
        .expect_err("missing employer name");
    assert_eq!(err.code(), "hosts-employer-missing-name");
}

#[test]
fn local_defaults_fixture() {
    let v = validate_local_yaml(&fixture("local/defaults.yaml")).expect("validation");
    assert!(matches!(v.data.source, LocalSource::File));
    assert!(v.data.config.working_repo_roots.is_empty());
    assert_eq!(v.data.config.performance.refresh_batch_size, 50);
}

#[test]
fn local_overrides_fixture() {
    unsafe { std::env::set_var("HOME", "/Users/test-overrides") };
    let v = validate_local_yaml(&fixture("local/overrides.yaml")).expect("validation");
    assert_eq!(v.data.config.performance.refresh_batch_size, 25);
    assert_eq!(v.data.config.working_repo_roots.len(), 2);
    assert_eq!(
        v.data.config.archive_clone_path,
        PathBuf::from("/Users/test-overrides/Project/graysurf/agent-plan-archive")
    );
}

#[test]
fn local_missing_path_returns_defaults() {
    let path = PathBuf::from("/nonexistent/agent-plan-archive/config.yaml");
    let v = validate_local_path(&path).expect("defaults");
    assert!(matches!(v.data.source, LocalSource::Defaults));
    assert_eq!(v.warnings.len(), 1);
    assert_eq!(v.warnings[0].code, "local-defaults-used");
}

#[test]
fn metadata_github_pr_fixture() {
    let v = validate_metadata_yaml(&fixture("metadata/github-pr.yaml")).expect("validation");
    assert!(v.warnings.is_empty());
    assert_eq!(v.data.config.source.host, "github.com");
    assert!(v.data.config.captured_classification.is_some());
    let refs = &v.data.config.refs;
    assert!(refs.issue.is_some());
    assert!(refs.pr.is_some());
    assert!(refs.mr.is_none());
}

#[test]
fn metadata_gitlab_mr_fixture() {
    let v = validate_metadata_yaml(&fixture("metadata/gitlab-mr.yaml")).expect("validation");
    let cls = v.data.config.captured_classification.expect("captured");
    assert!(matches!(cls.class, HostClass::Employer));
    assert_eq!(cls.employer.as_deref(), Some("ExampleCorp"));
    assert!(v.data.config.refs.mr.is_some());
}

#[test]
fn metadata_orphan_plan_warns_on_missing_classification() {
    let v = validate_metadata_yaml(&fixture("metadata/orphan-plan.yaml")).expect("validation");
    assert!(v.data.config.captured_classification.is_none());
    assert_eq!(v.warnings.len(), 1);
    assert_eq!(
        v.warnings[0].code,
        "metadata-captured-classification-missing"
    );
}
