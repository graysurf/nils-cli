//! Integration coverage for `drift-audit.allow.yaml` unsafe allowlist handling.

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
fn allowlist_demotes_block_to_warn_under_tests_fixture_glob() {
    let tmp = copy_fixture();
    write(
        &tmp.path().join("tests/drift/fixtures/auth.json"),
        "token: 4fK9zQm2Lp8sVx7Tn3Bc6Rj0WaYd\n",
    );
    write(
        &tmp.path().join("drift-audit.allow.yaml"),
        r#"schema_version: 1
unsafe_allow:
  - path: "tests/drift/fixtures/**"
    reason: "fixture intentionally contains fake token material"
"#,
    );

    let out = audit(tmp.path(), &[]);
    assert_eq!(
        out.code,
        1,
        "allowlisted block finding should demote to warn; stderr=\n{}",
        out.stderr_text(),
    );
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("[unsafe/warn"),
        "expected demoted unsafe/warn finding; got\n{stderr}",
    );
    assert!(
        stderr.contains("allowlist demoted block->warn"),
        "expected allowlist demotion note; got\n{stderr}",
    );
}

#[test]
fn allowlist_demotes_warn_to_suppressed_without_hiding_verbose_evidence() {
    let tmp = copy_fixture();
    write(&tmp.path().join("core/auth.json"), "{}\n");
    write(
        &tmp.path().join("drift-audit.allow.yaml"),
        r#"schema_version: 1
unsafe_allow:
  - path: "core/auth.json"
    reason: "empty fixture path only"
"#,
    );

    let normal = audit(tmp.path(), &[]);
    assert_eq!(
        normal.code,
        0,
        "allowlisted warn should demote to suppressed by default; stderr=\n{}",
        normal.stderr_text(),
    );
    assert!(
        !normal.stderr_text().contains("[unsafe/suppressed"),
        "suppressed finding should stay hidden by default; stderr=\n{}",
        normal.stderr_text(),
    );

    let verbose = audit(tmp.path(), &["--verbose"]);
    assert_eq!(verbose.code, 0, "stderr=\n{}", verbose.stderr_text());
    let stderr = verbose.stderr_text();
    assert!(
        stderr.contains("[unsafe/suppressed"),
        "expected verbose suppressed finding; got\n{stderr}",
    );
    assert!(
        stderr.contains("allowlist demoted warn->suppressed"),
        "expected allowlist demotion note; got\n{stderr}",
    );
}

#[test]
fn allowlist_entry_missing_reason_errors_at_audit_start() {
    let tmp = copy_fixture();
    write(&tmp.path().join("core/auth.json"), "{}\n");
    write(
        &tmp.path().join("drift-audit.allow.yaml"),
        r#"schema_version: 1
unsafe_allow:
  - path: "core/auth.json"
"#,
    );

    let out = audit(tmp.path(), &[]);
    assert_ne!(out.code, 0, "missing reason should fail");
    assert!(
        out.stderr_text().contains("reason"),
        "expected schema error naming reason; stderr=\n{}",
        out.stderr_text(),
    );
}

#[test]
fn private_allowlist_is_rejected_at_config_load() {
    let tmp = copy_fixture();
    write(
        &tmp.path().join(".private/drift-audit.allow.yaml"),
        r#"schema_version: 1
unsafe_allow:
  - path: "core/auth.json"
    reason: "not supported from private overlay"
"#,
    );

    let out = audit(tmp.path(), &[]);
    assert_ne!(out.code, 0, ".private allowlist should fail");
    let stderr = out.stderr_text();
    assert!(
        stderr.contains(".private") && stderr.contains("drift-audit.allow.yaml"),
        "expected .private allowlist rejection; got\n{stderr}",
    );
}
