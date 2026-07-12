use std::fs;
use std::path::Path;
use std::process::Command;

use nils_test_support::cmd::{CmdOutput, run_resolved_in_dir};
use pretty_assertions::assert_eq;

fn run(dir: &Path, args: &[&str]) -> CmdOutput {
    run_resolved_in_dir("test-first-evidence", dir, args, &[], None)
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git spawn");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git stdout")
        .trim()
        .to_string()
}

fn init_repo(root: &Path, name: &str, remote: Option<&str>) -> std::path::PathBuf {
    let repo = root.join(name);
    fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Tester"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), format!("{name}\n")).expect("readme");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    if let Some(remote) = remote {
        git(&repo, &["remote", "add", "origin", remote]);
    }
    repo
}

fn complete_record(dir: &Path, out_dir: &Path) {
    let out = out_dir.to_string_lossy();
    for args in [
        vec![
            "init",
            "--out",
            out.as_ref(),
            "--classification",
            "behavior-change",
            "--changed-behavior",
            "subject binding is enforced",
        ],
        vec![
            "record-impact",
            "--out",
            out.as_ref(),
            "--none",
            "--reason",
            "fixture has no existing owner",
        ],
        vec![
            "record-waiver",
            "--out",
            out.as_ref(),
            "--reason",
            "fixture setup",
            "--waiver-kind",
            "non-testable",
            "--why-no-red",
            "fixture exercises metadata only",
            "--substitute-validation",
            "subject assertions",
        ],
        vec![
            "record-final",
            "--out",
            out.as_ref(),
            "--command",
            "cargo test subject_binding",
            "--status",
            "pass",
            "--scope",
            "focused",
        ],
        vec!["record-gap", "--out", out.as_ref(), "--none"],
    ] {
        let output = run(dir, &args);
        assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    }
}

#[test]
fn baseline_is_immutable_and_delivery_reattestation_preserves_history() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = init_repo(
        tmp.path(),
        "repo",
        Some("https://user:password@github.com/Acme/Widget.git"),
    );
    let evidence = tmp.path().join("evidence");
    complete_record(tmp.path(), &evidence);
    let evidence_arg = evidence.to_string_lossy();
    let repo_arg = repo.to_string_lossy();

    let baseline = run(
        tmp.path(),
        &[
            "bind-baseline",
            "--out",
            evidence_arg.as_ref(),
            "--project-path",
            repo_arg.as_ref(),
            "--format",
            "json",
        ],
    );
    assert_eq!(baseline.code, 0, "stderr={}", baseline.stderr_text());
    let baseline_json = baseline.stdout_json();
    assert_eq!(
        baseline_json["schema_version"],
        "cli.test-first-evidence.bind-baseline.v2"
    );
    assert_eq!(
        baseline_json["result"]["record"]["subject"]["repository"]["kind"],
        "provider"
    );
    assert_eq!(
        baseline_json["result"]["record"]["subject"]["repository"]["id"],
        "github.com/acme/widget"
    );
    let baseline_commit = baseline_json["result"]["record"]["subject"]["baseline"]["commit"]
        .as_str()
        .expect("baseline commit")
        .to_string();
    let rendered = baseline_json.to_string();
    assert!(!rendered.contains(repo_arg.as_ref()));
    assert!(!rendered.contains("password"));

    let duplicate = run(
        tmp.path(),
        &[
            "bind-baseline",
            "--out",
            evidence_arg.as_ref(),
            "--project-path",
            repo_arg.as_ref(),
            "--format",
            "json",
        ],
    );
    assert_eq!(duplicate.code, 65);
    assert_eq!(
        duplicate.stdout_json()["error"]["code"],
        "baseline-subject-already-bound"
    );

    git(&repo, &["checkout", "-q", "-b", "feat/evidence"]);
    fs::write(repo.join("src.txt"), "first delivery\n").expect("delivery file");
    git(&repo, &["add", "src.txt"]);
    git(&repo, &["commit", "-q", "-m", "delivery"]);
    let first_head = git(&repo, &["rev-parse", "HEAD"]);
    let first = run(
        tmp.path(),
        &[
            "bind-delivery",
            "--out",
            evidence_arg.as_ref(),
            "--project-path",
            repo_arg.as_ref(),
            "--format",
            "json",
        ],
    );
    assert_eq!(first.code, 0, "stderr={}", first.stderr_text());
    assert_eq!(
        first.stdout_json()["result"]["record"]["subject"]["deliveries"][0]["head"],
        first_head
    );
    let first_digest =
        first.stdout_json()["result"]["record"]["subject"]["deliveries"][0]["diff_digest"]
            .as_str()
            .expect("diff digest")
            .to_string();
    git(&repo, &["config", "diff.noprefix", "true"]);
    let config_independent = run(
        tmp.path(),
        &[
            "verify",
            "--out",
            evidence_arg.as_ref(),
            "--project-path",
            repo_arg.as_ref(),
            "--format",
            "json",
        ],
    );
    assert_eq!(
        config_independent.code,
        0,
        "stderr={}",
        config_independent.stderr_text()
    );
    assert_eq!(
        config_independent.stdout_json()["result"]["record"]["subject"]["deliveries"][0]["diff_digest"],
        first_digest
    );

    fs::write(repo.join("src.txt"), "amended delivery\n").expect("amended file");
    git(&repo, &["add", "src.txt"]);
    git(&repo, &["commit", "-q", "--amend", "--no-edit"]);
    let amended_head = git(&repo, &["rev-parse", "HEAD"]);
    assert_ne!(first_head, amended_head);

    let stale = run(
        tmp.path(),
        &[
            "verify",
            "--out",
            evidence_arg.as_ref(),
            "--project-path",
            repo_arg.as_ref(),
            "--format",
            "json",
        ],
    );
    assert_eq!(stale.code, 65);
    assert_eq!(stale.stdout_json()["error"]["code"], "subject-mismatch");

    let second = run(
        tmp.path(),
        &[
            "bind-delivery",
            "--out",
            evidence_arg.as_ref(),
            "--project-path",
            repo_arg.as_ref(),
            "--format",
            "json",
        ],
    );
    assert_eq!(second.code, 0, "stderr={}", second.stderr_text());
    let second_json = second.stdout_json();
    assert_eq!(
        second_json["result"]["record"]["subject"]["baseline"]["commit"],
        baseline_commit
    );
    assert_eq!(
        second_json["result"]["record"]["subject"]["deliveries"]
            .as_array()
            .expect("deliveries")
            .len(),
        2
    );
    assert_eq!(
        second_json["result"]["record"]["subject"]["deliveries"][1]["attempt"],
        2
    );

    git(&repo, &["checkout", "-q", "main"]);
    fs::write(repo.join("upstream.txt"), "new upstream baseline\n").expect("upstream file");
    git(&repo, &["add", "upstream.txt"]);
    git(&repo, &["commit", "-q", "-m", "upstream"]);
    git(&repo, &["checkout", "-q", "feat/evidence"]);
    git(&repo, &["rebase", "main"]);
    let rebased = run(
        tmp.path(),
        &[
            "verify",
            "--out",
            evidence_arg.as_ref(),
            "--project-path",
            repo_arg.as_ref(),
            "--format",
            "json",
        ],
    );
    assert_eq!(rebased.code, 65);
    assert_eq!(rebased.stdout_json()["error"]["code"], "subject-mismatch");
    let third = run(
        tmp.path(),
        &[
            "bind-delivery",
            "--out",
            evidence_arg.as_ref(),
            "--project-path",
            repo_arg.as_ref(),
            "--format",
            "json",
        ],
    );
    assert_eq!(third.code, 0, "stderr={}", third.stderr_text());
    assert_eq!(
        third.stdout_json()["result"]["record"]["subject"]["deliveries"]
            .as_array()
            .expect("deliveries")
            .len(),
        3
    );
    assert_eq!(
        third.stdout_json()["result"]["record"]["subject"]["baseline"]["commit"],
        baseline_commit
    );

    let verify = run(
        tmp.path(),
        &[
            "verify",
            "--out",
            evidence_arg.as_ref(),
            "--project-path",
            repo_arg.as_ref(),
            "--format",
            "json",
        ],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
}

#[test]
fn provider_transport_alias_does_not_change_repository_identity() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = init_repo(
        tmp.path(),
        "repo",
        Some("ssh://git@ssh.github.com:443/Acme/Widget.git"),
    );
    let evidence = tmp.path().join("evidence");
    complete_record(tmp.path(), &evidence);
    let evidence_arg = evidence.to_string_lossy();
    let repo_arg = repo.to_string_lossy();

    for command in ["bind-baseline", "bind-delivery"] {
        if command == "bind-delivery" {
            fs::write(repo.join("delivery.txt"), "delivery\n").expect("delivery");
            git(&repo, &["add", "delivery.txt"]);
            git(&repo, &["commit", "-q", "-m", "delivery"]);
        }
        let output = run(
            tmp.path(),
            &[
                command,
                "--out",
                evidence_arg.as_ref(),
                "--project-path",
                repo_arg.as_ref(),
            ],
        );
        assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    }

    git(
        &repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/widget.git",
        ],
    );
    let verify = run(
        tmp.path(),
        &[
            "verify",
            "--out",
            evidence_arg.as_ref(),
            "--project-path",
            repo_arg.as_ref(),
        ],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
}

#[test]
fn strict_subject_verification_rejects_symbolic_oids_and_noncanonical_attempts() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = init_repo(
        tmp.path(),
        "repo",
        Some("https://github.com/acme/widget.git"),
    );
    let evidence = tmp.path().join("evidence");
    complete_record(tmp.path(), &evidence);
    let evidence_arg = evidence.to_string_lossy();
    let repo_arg = repo.to_string_lossy();
    let record_file = evidence.join("test-first-evidence.json");

    for command in ["bind-baseline", "bind-delivery"] {
        if command == "bind-delivery" {
            fs::write(repo.join("delivery.txt"), "delivery\n").expect("delivery");
            git(&repo, &["add", "delivery.txt"]);
            git(&repo, &["commit", "-q", "-m", "delivery"]);
        }
        assert_eq!(
            run(
                tmp.path(),
                &[
                    command,
                    "--out",
                    evidence_arg.as_ref(),
                    "--project-path",
                    repo_arg.as_ref(),
                ],
            )
            .code,
            0
        );
    }

    let canonical = fs::read_to_string(&record_file).expect("canonical record");
    let mut symbolic: serde_json::Value = serde_json::from_str(&canonical).expect("record json");
    symbolic["subject"]["baseline"]["commit"] = serde_json::json!("HEAD");
    fs::write(
        &record_file,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&symbolic).expect("render symbolic")
        ),
    )
    .expect("write symbolic");
    let rejected = run(
        tmp.path(),
        &[
            "verify",
            "--out",
            evidence_arg.as_ref(),
            "--project-path",
            repo_arg.as_ref(),
            "--format",
            "json",
        ],
    );
    assert_eq!(rejected.code, 65);
    assert_eq!(
        rejected.stdout_json()["error"]["details"]["reason_code"],
        "invalid-subject-object-id"
    );

    let mut invalid_attempt: serde_json::Value =
        serde_json::from_str(&canonical).expect("record json");
    invalid_attempt["subject"]["deliveries"][0]["attempt"] = serde_json::json!(2);
    fs::write(
        &record_file,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&invalid_attempt).expect("render attempt")
        ),
    )
    .expect("write attempt");
    let rejected = run(
        tmp.path(),
        &[
            "verify",
            "--out",
            evidence_arg.as_ref(),
            "--project-path",
            repo_arg.as_ref(),
            "--format",
            "json",
        ],
    );
    assert_eq!(rejected.code, 65);
    assert_eq!(
        rejected.stdout_json()["error"]["details"]["reason_code"],
        "invalid-delivery-attempt"
    );
}

#[test]
fn strict_subject_verification_rejects_unbound_and_other_local_repositories() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo_a = init_repo(tmp.path(), "repo-a", None);
    let repo_b = init_repo(tmp.path(), "repo-b", None);
    let evidence = tmp.path().join("evidence-a");
    complete_record(tmp.path(), &evidence);
    let evidence_arg = evidence.to_string_lossy();
    let repo_a_arg = repo_a.to_string_lossy();
    let repo_b_arg = repo_b.to_string_lossy();

    let unbound = run(
        tmp.path(),
        &[
            "verify",
            "--out",
            evidence_arg.as_ref(),
            "--project-path",
            repo_a_arg.as_ref(),
            "--format",
            "json",
        ],
    );
    assert_eq!(unbound.code, 65);
    assert_eq!(unbound.stdout_json()["error"]["code"], "unbound-subject");

    for command in ["bind-baseline", "bind-delivery"] {
        if command == "bind-delivery" {
            fs::write(repo_a.join("delivery.txt"), "delivery\n").expect("delivery");
            git(&repo_a, &["add", "delivery.txt"]);
            git(&repo_a, &["commit", "-q", "-m", "delivery"]);
        }
        let output = run(
            tmp.path(),
            &[
                command,
                "--out",
                evidence_arg.as_ref(),
                "--project-path",
                repo_a_arg.as_ref(),
            ],
        );
        assert_eq!(
            output.code,
            0,
            "command={command} stderr={}",
            output.stderr_text()
        );
    }

    let mismatch = run(
        tmp.path(),
        &[
            "verify",
            "--out",
            evidence_arg.as_ref(),
            "--project-path",
            repo_b_arg.as_ref(),
            "--format",
            "json",
        ],
    );
    assert_eq!(mismatch.code, 65);
    assert_eq!(mismatch.stdout_json()["error"]["code"], "subject-mismatch");
}
