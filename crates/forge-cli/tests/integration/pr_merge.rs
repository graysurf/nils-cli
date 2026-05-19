//! `pr merge` integration tests covering the dry-run plan envelope and the
//! configuration-driven `keep_branch_conflict` rule. The wider lock-down chain
//! (rules 4 / 6 / 7 / 8 / 9) is unit-tested in
//! `crates/forge-cli/src/ops/pr_merge.rs` and the dedicated gate suite at
//! `tests/integration/required_check_gate.rs`; this module pins the CLI surface
//! and the per-repo `.forge-cli.toml` precedence end-to-end.

use std::fs;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::support::{StubEnv, parse_envelope, run_forge_cli_in};

const FORBIDDEN_STUB: &str = "#!/bin/sh\necho 'should not run during dry-run' >&2\nexit 99\n";

#[test]
fn pr_merge_dry_run_renders_squash_plan_with_delete_branch() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "merge",
            "42",
        ],
        None,
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.merge.v1");
    let plan: Vec<String> = env["data"]["plan"]
        .as_array()
        .expect("plan array")
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert!(plan.iter().any(|s| s == "merge"), "{plan:?}");
    assert!(plan.iter().any(|s| s == "42"), "{plan:?}");
    assert!(plan.iter().any(|s| s == "--squash"), "{plan:?}");
    assert!(plan.iter().any(|s| s == "--delete-branch"), "{plan:?}");
}

#[test]
fn pr_merge_dry_run_method_override_uses_merge_flag() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "merge",
            "1",
            "--method",
            "merge",
        ],
        None,
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let plan: Vec<String> = env["data"]["plan"]
        .as_array()
        .expect("plan array")
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert!(plan.iter().any(|s| s == "--merge"), "{plan:?}");
    assert!(!plan.iter().any(|s| s == "--squash"), "{plan:?}");
}

#[test]
fn pr_merge_keep_branch_drops_delete_branch_in_dry_run_plan() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "merge",
            "1",
            "--keep-branch",
        ],
        None,
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let plan: Vec<String> = env["data"]["plan"]
        .as_array()
        .expect("plan array")
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert!(!plan.iter().any(|s| s == "--delete-branch"), "{plan:?}");
}

#[test]
fn pr_merge_keep_branch_conflicts_with_config_delete_branch_true() {
    // The lock-down rule (rule 10) requires an *explicit* config opting into
    // branch deletion before --keep-branch becomes a hard error. We mock that
    // by writing a .forge-cli.toml inside a tempdir and invoking the binary
    // from there so the loader picks it up.
    let repo = TempDir::new().expect("tempdir");
    fs::write(
        repo.path().join(".forge-cli.toml"),
        "[merge]\ndelete_branch = true\n",
    )
    .expect("write config");
    fs::create_dir_all(repo.path().join(".git")).expect("fake .git");

    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "merge",
            "1",
            "--keep-branch",
        ],
        Some(repo.path()),
    );
    assert_eq!(
        out.code, 65,
        "expected DATA 65 on keep_branch_conflict, stderr={}",
        out.stderr
    );
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "keep_branch_conflict");
}
