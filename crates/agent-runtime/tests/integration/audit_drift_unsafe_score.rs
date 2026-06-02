//! Integration coverage for Plan 04 Task 4.1 `audit-drift` unsafe scoring.
//!
//! The plan still names a standalone `crates/audit-drift` crate, but the
//! current implementation lives inside `agent-runtime::audit_drift`.
//! These tests pin the user-facing `agent-runtime audit-drift` contract so
//! the internal module layout can remain local to this repo.

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn agent_runtime_bin() -> PathBuf {
    bin::resolve("agent-runtime")
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/render-determinism")
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_tree(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).unwrap();
        }
    }
}

fn copy_fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    copy_tree(&fixture_root(), tmp.path());
    tmp
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn audit(root: &Path, extra_args: &[&str]) -> CmdOutput {
    let mut args = vec!["audit-drift", "--source-root", root.to_str().unwrap()];
    args.extend(extra_args);
    cmd::run(&agent_runtime_bin(), &args, &[], None)
}

#[test]
fn unsafe_score_path_keyword_entropy_blocks() {
    let tmp = copy_fixture();
    write(
        &tmp.path().join("core/secrets/auth.json"),
        "token: 4fK9zQm2Lp8sVx7Tn3Bc6Rj0WaYd\n",
    );

    let out = audit(tmp.path(), &[]);
    assert_eq!(
        out.code,
        2,
        "path + keyword + entropy should block; stderr=\n{}",
        out.stderr_text(),
    );
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("[unsafe/block"),
        "expected unsafe/block finding; got\n{stderr}",
    );
    assert!(
        stderr.contains("score=1.2"),
        "expected composite score in finding; got\n{stderr}",
    );
}

#[test]
fn unsafe_score_path_only_warns() {
    let tmp = copy_fixture();
    write(&tmp.path().join("core/auth.json"), "{}\n");

    let out = audit(tmp.path(), &[]);
    assert_eq!(
        out.code,
        1,
        "path-only unsafe score should warn; stderr=\n{}",
        out.stderr_text(),
    );
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("[unsafe/warn"),
        "expected unsafe/warn finding; got\n{stderr}",
    );
    assert!(
        stderr.contains("score=0.4"),
        "expected path-only score; got\n{stderr}",
    );
}

#[test]
fn unsafe_score_entropy_only_warns() {
    let tmp = copy_fixture();
    write(
        &tmp.path().join("core/skills/sample/high-entropy.txt"),
        "opaque 4fK9zQm2Lp8sVx7Tn3Bc6Rj0WaYd\n",
    );

    let out = audit(tmp.path(), &[]);
    assert_eq!(
        out.code,
        1,
        "entropy-only unsafe score should warn; stderr=\n{}",
        out.stderr_text(),
    );
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("[unsafe/warn"),
        "expected unsafe/warn finding; got\n{stderr}",
    );
    assert!(
        stderr.contains("entropy_above_threshold"),
        "expected entropy signal; got\n{stderr}",
    );
}

#[test]
fn unsafe_score_keyword_without_value_is_suppressed_until_verbose() {
    let tmp = copy_fixture();
    write(
        &tmp.path().join("core/skills/sample/rotation-notes.md"),
        "password rotation policy only; no value appears here\n",
    );

    let normal = audit(tmp.path(), &[]);
    assert_eq!(
        normal.code,
        0,
        "suppressed unsafe finding should not affect default exit; stderr=\n{}",
        normal.stderr_text(),
    );
    assert!(
        !normal.stderr_text().contains("[unsafe/suppressed"),
        "suppressed finding should be hidden by default; stderr=\n{}",
        normal.stderr_text(),
    );

    let verbose = audit(tmp.path(), &["--verbose"]);
    assert_eq!(
        verbose.code,
        0,
        "suppressed unsafe finding should still exit 0; stderr=\n{}",
        verbose.stderr_text(),
    );
    assert!(
        verbose.stderr_text().contains("[unsafe/suppressed"),
        "suppressed finding should be visible with --verbose; stderr=\n{}",
        verbose.stderr_text(),
    );
}

#[test]
fn json_format_emits_envelope_with_findings_and_exit_code() {
    let tmp = copy_fixture();
    write(
        &tmp.path().join("core/secrets/auth.json"),
        "token: 4fK9zQm2Lp8sVx7Tn3Bc6Rj0WaYd\n",
    );

    let out = audit(tmp.path(), &["--format", "json"]);
    assert_eq!(
        out.code, 2,
        "block finding should still exit 2 in json mode"
    );
    let stdout = out.stdout_text();
    assert!(
        stdout.contains("\"schema_version\": \"agent-runtime-cli.audit-drift.v1\""),
        "json stdout should carry the schema_version; got\n{stdout}",
    );
    assert!(
        stdout.contains("\"exit_code\": 2"),
        "json envelope should report exit_code 2; got\n{stdout}",
    );
    assert!(
        stdout.contains("\"block\": 1"),
        "json envelope should count the block finding; got\n{stdout}",
    );
    assert!(
        stdout.contains("\"severity\": \"block\""),
        "json findings should serialize severity as a lowercase label; got\n{stdout}",
    );
}

#[test]
fn fail_on_block_demotes_a_warn_only_run_to_exit_zero() {
    let tmp = copy_fixture();
    // Entropy-only finding at a non-sensitive path with no keyword scores
    // 0.4 -> warn (not block).
    write(
        &tmp.path().join("core/notes/scratch.md"),
        "value = 4fK9zQm2Lp8sVx7Tn3Bc6Rj0WaYd\n",
    );

    let default_run = audit(tmp.path(), &[]);
    assert_eq!(
        default_run.code,
        1,
        "a warn finding fails by default; stderr=\n{}",
        default_run.stderr_text(),
    );

    let block_only = audit(tmp.path(), &["--fail-on", "block"]);
    assert_eq!(
        block_only.code,
        0,
        "--fail-on block makes a warn-only run non-fatal; stderr=\n{}",
        block_only.stderr_text(),
    );
    assert!(
        block_only.stderr_text().contains("[unsafe/warn"),
        "the warn finding should still be reported; stderr=\n{}",
        block_only.stderr_text(),
    );
}

#[test]
fn fail_on_block_keeps_block_findings_fatal() {
    let tmp = copy_fixture();
    write(
        &tmp.path().join("core/secrets/auth.json"),
        "token: 4fK9zQm2Lp8sVx7Tn3Bc6Rj0WaYd\n",
    );

    let out = audit(tmp.path(), &["--fail-on", "block"]);
    assert_eq!(
        out.code,
        2,
        "--fail-on block must still fail on a block finding; stderr=\n{}",
        out.stderr_text(),
    );
}

#[test]
fn dated_path_reference_does_not_trip_the_entropy_signal() {
    let tmp = copy_fixture();
    // A retained record cross-referencing a dated plan bundle by path: the
    // exact false positive this scorer change removes.
    write(
        &tmp.path()
            .join("core/policies/heuristic-system/operation-records/foo/RECORD.md"),
        "- See `docs/plans/2026-05-27-plan-archive-discover/plan-archive-discover-execution-state.md`\n",
    );

    let out = audit(tmp.path(), &["--verbose"]);
    assert_eq!(
        out.code,
        0,
        "a dated path reference must not produce a warn; stderr=\n{}",
        out.stderr_text(),
    );
    assert!(
        !out.stderr_text()
            .contains("operation-records/foo/RECORD.md"),
        "the record should produce no unsafe finding at all; stderr=\n{}",
        out.stderr_text(),
    );
}
