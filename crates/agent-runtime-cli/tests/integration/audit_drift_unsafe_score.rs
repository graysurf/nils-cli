//! Integration coverage for Plan 04 Task 4.1 `audit-drift` unsafe scoring.
//!
//! The plan still names a standalone `crates/audit-drift` crate, but the
//! current implementation lives inside `agent-runtime-cli::audit_drift`.
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
