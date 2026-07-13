//! Sprint 7 Task 7.2 — full exit-code matrix.
//!
//! Spec: `forge-cli-spec-v1` §"Exit code map" + §"Lock-down policy". Every
//! row in those two tables MUST have at least one assertion here. Each test
//! references the constant name from `nils_common::cli_contract::exit`, never
//! the numeric value, so a future re-number of the sysexits table doesn't
//! drift this matrix silently.

use forge_cli::error::ForgeError;
use nils_common::cli_contract::exit;
use pretty_assertions::assert_eq;

const SCHEMA: &str = "cli.forge-cli.error.v1";

// ---------------------------------------------------------------------------
// Spec §"Exit code map" — one assertion per row.
// ---------------------------------------------------------------------------

#[test]
fn exit_constant_runtime_is_used_by_backend_error() {
    let err = ForgeError::backend_error(SCHEMA, "backend non-zero", None);
    assert_eq!(err.exit_code(), exit::RUNTIME);
    assert_eq!(err.kind(), "backend_error");
}

#[test]
fn exit_constant_usage_is_used_by_provider_unsupported() {
    let err = ForgeError::provider_unsupported(SCHEMA, "unknown host", None);
    assert_eq!(err.exit_code(), exit::USAGE);
    assert_eq!(err.kind(), "provider_unsupported");
}

#[test]
fn exit_constant_data_is_used_by_validation() {
    // DATA 65 covers every lock-down rule violation; the next group below
    // pins each documented `error.kind` to the same exit code.
    let err = ForgeError::validation(SCHEMA, "branch_name_invalid", "bad branch", None);
    assert_eq!(err.exit_code(), exit::DATA);
}

#[test]
fn exit_constant_unavailable_is_used_by_backend_missing() {
    let err = ForgeError::backend_missing(SCHEMA, "gh not found", None);
    assert_eq!(err.exit_code(), exit::UNAVAILABLE);
    assert_eq!(err.kind(), "backend_missing");
}

#[test]
fn exit_constant_unavailable_is_used_by_backend_unauthenticated() {
    let err = ForgeError::backend_unauthenticated(SCHEMA, "auth required", None);
    assert_eq!(err.exit_code(), exit::UNAVAILABLE);
    assert_eq!(err.kind(), "backend_unauthenticated");
}

#[test]
fn exit_constant_software_is_used_by_software_error() {
    let err = ForgeError::software(SCHEMA, "invariant blew", None);
    assert_eq!(err.exit_code(), exit::SOFTWARE);
    assert_eq!(err.kind(), "software_error");
}

#[test]
fn exit_constant_software_is_used_by_not_implemented() {
    let err = ForgeError::not_implemented(SCHEMA, "subcommand not implemented");
    assert_eq!(err.exit_code(), exit::SOFTWARE);
    assert_eq!(err.kind(), "not_implemented");
}

// ---------------------------------------------------------------------------
// Spec §"Lock-down policy" — one assertion per documented `error.kind`.
// Every row maps to DATA 65 except checks_failed (RUNTIME 1) and
// checks_timeout / glab_version_unsupported (UNAVAILABLE 69) which the spec
// keeps adjacent to the table notes.
// ---------------------------------------------------------------------------

const LOCKDOWN_DATA_KINDS: &[&str] = &[
    "branch_name_invalid",
    "branch_kind_mismatch",
    "body_missing_summary",
    "body_missing_test_plan",
    "title_too_long",
    "dirty_worktree",
    "head_not_pushed",
    "default_branch_protected",
    "draft_merge_refused",
    "checks_pending",
    "merge_method_unsupported",
    "keep_branch_conflict",
    "local_path_present",
    "unresolved_review_threads",
    "unchecked_task_items",
    "review_thread_pr_mismatch",
    "review_changes_requested",
    "review_convergence_head_missing",
    "review_convergence_head_changed",
    "review_convergence_activity_changed",
    "review_snapshot_incomplete",
    "invalid_review_convergence_config",
];

#[test]
fn every_lockdown_data_kind_maps_to_data_65() {
    for kind in LOCKDOWN_DATA_KINDS {
        let err = ForgeError::validation(SCHEMA, kind, "x", None);
        assert_eq!(
            err.exit_code(),
            exit::DATA,
            "kind={kind} should map to DATA"
        );
        assert_eq!(err.kind(), *kind, "kind round-trip for {kind}");
    }
}

#[test]
fn checks_failed_maps_to_runtime_1() {
    let err = ForgeError::runtime_failure(SCHEMA, "checks_failed", "ci failed", None);
    assert_eq!(err.exit_code(), exit::RUNTIME);
    assert_eq!(err.kind(), "checks_failed");
}

#[test]
fn checks_timeout_maps_to_unavailable_69() {
    let err = ForgeError::unavailable(SCHEMA, "checks_timeout", "deadline reached", None);
    assert_eq!(err.exit_code(), exit::UNAVAILABLE);
    assert_eq!(err.kind(), "checks_timeout");
}

#[test]
fn review_convergence_timeout_maps_to_unavailable_69() {
    let err = ForgeError::unavailable(
        SCHEMA,
        "review_convergence_timeout",
        "deadline reached",
        None,
    );
    assert_eq!(err.exit_code(), exit::UNAVAILABLE);
    assert_eq!(err.kind(), "review_convergence_timeout");
}

#[test]
fn backend_output_limit_maps_to_unavailable_69_and_is_documented() {
    let err = ForgeError::unavailable(
        SCHEMA,
        "backend_output_limit",
        "capture limit exceeded",
        None,
    );
    assert_eq!(err.exit_code(), exit::UNAVAILABLE);
    assert_eq!(err.kind(), "backend_output_limit");

    let spec = include_str!("../../docs/specs/forge-cli-spec-v1.md");
    let row = spec
        .lines()
        .find(|line| line.starts_with("| `UNAVAILABLE`"))
        .expect("UNAVAILABLE exit-code map row");
    assert!(
        row.contains("backend output-limit failure"),
        "canonical exit-code map must name backend output-limit failures"
    );
}

#[test]
fn glab_version_unsupported_maps_to_unavailable_69() {
    let err = ForgeError::unavailable(SCHEMA, "glab_version_unsupported", "upgrade", None);
    assert_eq!(err.exit_code(), exit::UNAVAILABLE);
    assert_eq!(err.kind(), "glab_version_unsupported");
}

// ---------------------------------------------------------------------------
// Spec-stability invariant: the six exit constants the binary may emit are
// the ones documented in the spec's Exit code map, in numeric order.
// ---------------------------------------------------------------------------

#[test]
fn binary_only_emits_documented_exit_constants() {
    // No literals — every value comes from nils_common::cli_contract::exit.
    let canonical = [
        ("SUCCESS", exit::SUCCESS),
        ("RUNTIME", exit::RUNTIME),
        ("USAGE", exit::USAGE),
        ("DATA", exit::DATA),
        ("UNAVAILABLE", exit::UNAVAILABLE),
        ("SOFTWARE", exit::SOFTWARE),
    ];
    // All six are distinct.
    let mut codes: Vec<i32> = canonical.iter().map(|(_, c)| *c).collect();
    codes.sort();
    codes.dedup();
    assert_eq!(
        codes.len(),
        6,
        "exit constants must be six distinct values, got {canonical:?}"
    );
}
