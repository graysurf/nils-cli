use std::fs;
use std::path::Path;
use std::process::Command;

use nils_test_support::cmd::{CmdOutput, run_resolved_in_dir};

fn run(bin: &str, dir: &Path, args: &[&str]) -> CmdOutput {
    run_resolved_in_dir(bin, dir, args, &[], None)
}

fn init_repo(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n").unwrap();
    fs::write(root.join("tests/value.rs"), "#[test] fn value() {}\n").unwrap();
    fs::write(root.join("docs/guide.md"), "# Guide\n").unwrap();
    fs::write(
        root.join("AGENT_DOCS.toml"),
        r#"[path_classes]
production = ["src/**", "shared/**"]
test = ["tests/**", "shared/**"]
docs = ["docs/**", "**/*.md"]
generated = ["build/**"]
unmatched = "unknown"
"#,
    )
    .unwrap();
    assert!(
        Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(root)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["config", "user.email", "fixture@example.invalid"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["config", "user.name", "Fixture"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["add", "."])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["commit", "-qm", "fixture"])
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn test_first_check_is_phase_and_path_class_aware() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);
    let out = tmp.path().join("evidence");
    let out_arg = out.to_str().unwrap();
    let repo_arg = repo.to_str().unwrap();

    let init = run(
        "test-first-evidence",
        tmp.path(),
        &[
            "init",
            "--out",
            out_arg,
            "--classification",
            "behavior-change",
            "--changed-behavior",
            "production edits require durable pre-edit evidence",
        ],
    );
    assert_eq!(init.code, 0, "stderr: {}", init.stderr_text());

    let test_path = run(
        "test-first-evidence",
        tmp.path(),
        &[
            "check",
            "--out",
            out_arg,
            "--phase",
            "pre-edit",
            "--project-path",
            repo_arg,
            "--path",
            "tests/value.rs",
            "--format",
            "json",
        ],
    );
    assert_eq!(test_path.code, 0, "stderr: {}", test_path.stderr_text());
    assert_eq!(test_path.stdout_json()["result"]["path_class"], "test");
    assert_eq!(test_path.stdout_json()["result"]["allowed"], true);

    let production = run(
        "test-first-evidence",
        tmp.path(),
        &[
            "check",
            "--out",
            out_arg,
            "--phase",
            "pre-edit",
            "--project-path",
            repo_arg,
            "--path",
            "src/lib.rs",
            "--format",
            "json",
        ],
    );
    assert_eq!(production.code, 65);
    assert_eq!(
        production.stdout_json()["error"]["details"]["reason_code"],
        "missing-durable-pre-edit-evidence"
    );

    let ambiguous = run(
        "test-first-evidence",
        tmp.path(),
        &[
            "check",
            "--out",
            out_arg,
            "--phase",
            "pre-edit",
            "--project-path",
            repo_arg,
            "--path",
            "shared/value.rs",
            "--format",
            "json",
        ],
    );
    assert_eq!(ambiguous.code, 65);
    assert_eq!(
        ambiguous.stdout_json()["error"]["details"]["path_class"],
        "ambiguous"
    );

    for (path, expected_code) in [
        ("unclassified/value.txt", "unknown-path-class"),
        ("../outside.rs", "invalid-pre-edit-path"),
        (
            repo.join("src/lib.rs").to_str().unwrap(),
            "invalid-pre-edit-path",
        ),
    ] {
        let rejected = run(
            "test-first-evidence",
            tmp.path(),
            &[
                "check",
                "--out",
                out_arg,
                "--phase",
                "pre-edit",
                "--project-path",
                repo_arg,
                "--path",
                path,
                "--format",
                "json",
            ],
        );
        assert_eq!(
            rejected.code,
            65,
            "path={path} stderr={}",
            rejected.stderr_text()
        );
        let payload = rejected.stdout_json();
        let actual = if expected_code == "unknown-path-class" {
            payload["error"]["details"]["reason_code"].as_str()
        } else {
            payload["error"]["code"].as_str()
        };
        assert_eq!(actual, Some(expected_code), "path={path} payload={payload}");
    }

    let classified = run(
        "test-first-evidence",
        tmp.path(),
        &[
            "check",
            "--out",
            out_arg,
            "--phase",
            "classified",
            "--format",
            "json",
        ],
    );
    assert_eq!(classified.code, 0, "stderr: {}", classified.stderr_text());
    let delivery = run(
        "test-first-evidence",
        tmp.path(),
        &[
            "check", "--out", out_arg, "--phase", "delivery", "--format", "json",
        ],
    );
    assert_eq!(delivery.code, 65);
    assert_eq!(
        delivery.stdout_json()["error"]["details"]["reason_code"],
        "delivery-incomplete"
    );

    let impact = run(
        "test-first-evidence",
        tmp.path(),
        &[
            "record-impact",
            "--out",
            out_arg,
            "--target",
            "tests/value.rs::regression",
            "--disposition",
            "add-missing",
            "--protected-behavior",
            "production edits require durable pre-edit evidence",
            "--reason",
            "the regression had no owner test",
        ],
    );
    assert_eq!(impact.code, 0, "stderr: {}", impact.stderr_text());

    let before_fix = run(
        "test-first-evidence",
        tmp.path(),
        &[
            "record-failing",
            "--out",
            out_arg,
            "--command",
            "cargo test regression",
            "--exit-code",
            "101",
            "--summary",
            "regression reproduced",
            "--expected-failure",
            "the new contract is not implemented",
            "--observed-failure",
            "the regression assertion failed",
        ],
    );
    assert_eq!(before_fix.code, 0, "stderr: {}", before_fix.stderr_text());
    let production_ready = run(
        "test-first-evidence",
        tmp.path(),
        &[
            "check",
            "--out",
            out_arg,
            "--phase",
            "pre-edit",
            "--project-path",
            repo_arg,
            "--path",
            "src/lib.rs",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        production_ready.code,
        0,
        "stderr: {}",
        production_ready.stderr_text()
    );
    let final_validation = run(
        "test-first-evidence",
        tmp.path(),
        &[
            "record-final",
            "--out",
            out_arg,
            "--command",
            "cargo test regression",
            "--status",
            "pass",
            "--scope",
            "focused",
        ],
    );
    assert_eq!(final_validation.code, 0);
    let gaps = run(
        "test-first-evidence",
        tmp.path(),
        &["record-gap", "--out", out_arg, "--none"],
    );
    assert_eq!(gaps.code, 0);
    let delivery_ready = run(
        "test-first-evidence",
        tmp.path(),
        &[
            "check", "--out", out_arg, "--phase", "delivery", "--format", "json",
        ],
    );
    assert_eq!(
        delivery_ready.code,
        0,
        "stderr: {}",
        delivery_ready.stderr_text()
    );
}

#[test]
fn workflow_owner_uses_v2_skill_usage_record() {
    let tmp = tempfile::TempDir::new().unwrap();
    let out = tmp.path().join("usage");
    let out_arg = out.to_str().unwrap();
    let init = run(
        "skill-usage",
        tmp.path(),
        &[
            "init",
            "--out",
            out_arg,
            "--owner-kind",
            "workflow",
            "--owner-id",
            "deliver-pr",
            "--intent",
            "deliver reviewed change",
            "--user-request-summary",
            "deliver the change",
        ],
    );
    assert_eq!(init.code, 0, "stderr: {}", init.stderr_text());
    let validation = run(
        "skill-usage",
        tmp.path(),
        &[
            "record-validation",
            "--out",
            out_arg,
            "--command",
            "cargo test",
            "--status",
            "pass",
            "--summary",
            "passed",
        ],
    );
    assert_eq!(validation.code, 0);
    let outcome = run(
        "skill-usage",
        tmp.path(),
        &[
            "record-outcome",
            "--out",
            out_arg,
            "--status",
            "pass",
            "--summary",
            "complete",
        ],
    );
    assert_eq!(outcome.code, 0);
    let verify = run(
        "skill-usage",
        tmp.path(),
        &["verify", "--out", out_arg, "--format", "json"],
    );
    assert_eq!(verify.code, 0, "stderr: {}", verify.stderr_text());
    let json = verify.stdout_json();
    assert_eq!(json["data"]["record"]["schema"], "skill-usage.record.v2");
    assert_eq!(json["data"]["record"]["owner"]["kind"], "workflow");
    assert_eq!(json["data"]["record"]["owner"]["id"], "deliver-pr");
}

#[test]
fn docs_impact_record_detects_stale_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);
    fs::write(repo.join("src/lib.rs"), "pub fn value() -> u8 { 2 }\n").unwrap();
    let out = tmp.path().join("docs-impact");
    let out_arg = out.to_str().unwrap();
    let repo_arg = repo.to_str().unwrap();

    let record = run(
        "docs-impact",
        tmp.path(),
        &[
            "record",
            "--out",
            out_arg,
            "--repo",
            repo_arg,
            "--disposition",
            "no-docs-needed",
            "--rationale",
            "Internal implementation detail",
            "--format",
            "json",
        ],
    );
    assert_eq!(record.code, 0, "stderr: {}", record.stderr_text());
    let verify = run(
        "docs-impact",
        tmp.path(),
        &[
            "verify", "--out", out_arg, "--repo", repo_arg, "--format", "json",
        ],
    );
    assert_eq!(verify.code, 0, "stderr: {}", verify.stderr_text());
    fs::write(repo.join("src/new.rs"), "pub fn new_value() {}\n").unwrap();
    let stale = run(
        "docs-impact",
        tmp.path(),
        &[
            "verify", "--out", out_arg, "--repo", repo_arg, "--format", "json",
        ],
    );
    assert_eq!(stale.code, 65);
    assert_eq!(stale.stdout_json()["error"]["code"], "stale-scan");
}

#[test]
fn docs_impact_disposition_matrix_fails_closed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);
    fs::write(repo.join("src/lib.rs"), "pub fn value() -> u8 { 2 }\n").unwrap();
    let repo_arg = repo.to_str().unwrap();

    for (disposition, expected) in [
        ("docs-updated", "docs-update-missing"),
        ("no-docs-needed", "rationale-required"),
    ] {
        let out = tmp.path().join(disposition);
        let rejected = run(
            "docs-impact",
            tmp.path(),
            &[
                "record",
                "--out",
                out.to_str().unwrap(),
                "--repo",
                repo_arg,
                "--disposition",
                disposition,
                "--format",
                "json",
            ],
        );
        assert_eq!(rejected.code, 65, "stderr: {}", rejected.stderr_text());
        assert_eq!(rejected.stdout_json()["error"]["code"], expected);
    }

    let pending = tmp.path().join("pending");
    let record = run(
        "docs-impact",
        tmp.path(),
        &[
            "record",
            "--out",
            pending.to_str().unwrap(),
            "--repo",
            repo_arg,
            "--disposition",
            "pending",
            "--format",
            "json",
        ],
    );
    assert_eq!(record.code, 0, "stderr: {}", record.stderr_text());
    let verify = run(
        "docs-impact",
        tmp.path(),
        &[
            "verify",
            "--out",
            pending.to_str().unwrap(),
            "--repo",
            repo_arg,
            "--format",
            "json",
        ],
    );
    assert_eq!(verify.code, 65);
    assert_eq!(verify.stdout_json()["error"]["code"], "pending-disposition");

    fs::write(repo.join("docs/guide.md"), "# Guide\n\nUpdated.\n").unwrap();
    let docs_updated = tmp.path().join("docs-updated-valid");
    let accepted = run(
        "docs-impact",
        tmp.path(),
        &[
            "record",
            "--out",
            docs_updated.to_str().unwrap(),
            "--repo",
            repo_arg,
            "--disposition",
            "docs-updated",
            "--format",
            "json",
        ],
    );
    assert_eq!(accepted.code, 0, "stderr: {}", accepted.stderr_text());
}
