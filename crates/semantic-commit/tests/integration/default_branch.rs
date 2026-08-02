use std::fs;
use std::path::Path;
use std::process::Command;

use nils_common::default_branch_receipt::read_strict;
use pretty_assertions::{assert_eq, assert_ne};
use serde_json::Value;

use crate::common;

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("UTF-8 output")
}

fn git_trim(repo: &Path, args: &[&str]) -> String {
    let output = common::git_output(repo, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        text(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_string()
}

fn init_repo_with_head() -> tempfile::TempDir {
    let repo = common::init_repo();
    common::write_file(repo.path(), "base.txt", "base\n");
    common::git(repo.path(), &["add", "base.txt"]);
    common::git(
        repo.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "chore: initial",
        ],
    );
    repo
}

fn stage_change(repo: &Path) {
    common::write_file(repo, "change.txt", "change\n");
    common::git(repo, &["add", "change.txt"]);
}

fn configure_ssh_signing(repo: &Path) -> tempfile::TempDir {
    let signing_dir = tempfile::tempdir().expect("signing directory");
    let key = signing_dir.path().join("test-signing-key");
    let output = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .output()
        .expect("launch ssh-keygen");
    assert!(
        output.status.success(),
        "ssh-keygen failed: {}",
        text(&output.stderr)
    );
    let public_key = fs::read_to_string(key.with_extension("pub")).expect("public key");
    let allowed = signing_dir.path().join("allowed-signers");
    fs::write(&allowed, format!("test@example.com {public_key}")).expect("allowed signers");
    common::git(repo, &["config", "gpg.format", "ssh"]);
    common::git(
        repo,
        &["config", "user.signingkey", key.to_str().expect("key path")],
    );
    common::git(
        repo,
        &[
            "config",
            "gpg.ssh.allowedSignersFile",
            allowed.to_str().expect("allowed signers path"),
        ],
    );
    signing_dir
}

fn add_remote(repo: &Path) {
    common::git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "ssh://invalid.example.invalid/sympoies/demo.git",
        ],
    );
}

fn configure_cached_default(repo: &Path, upstream_sha: &str) {
    add_remote(repo);
    common::git(
        repo,
        &["update-ref", "refs/remotes/origin/main", upstream_sha],
    );
    common::git(
        repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    common::git(repo, &["config", "branch.main.remote", "origin"]);
    common::git(repo, &["config", "branch.main.merge", "refs/heads/main"]);
}

fn run_default_branch(
    repo: &Path,
    message: &str,
    head: &str,
    receipt: Option<&Path>,
    extra: &[&str],
) -> std::process::Output {
    run_default_branch_with_env(repo, message, head, receipt, extra, &[])
}

fn run_default_branch_with_env(
    repo: &Path,
    message: &str,
    head: &str,
    receipt: Option<&Path>,
    extra: &[&str],
    envs: &[(&str, &str)],
) -> std::process::Output {
    let mut args = vec![
        "default-branch",
        "--message",
        message,
        "--expect-head",
        head,
    ];
    if let Some(receipt) = receipt {
        args.extend(["--receipt-out", receipt.to_str().expect("receipt path")]);
    }
    args.extend(extra.iter().copied());
    common::run_semantic_commit_output(repo, &args, envs, None)
}

fn assert_preflight_failure(repo: &Path, head: &str, receipt: &Path, expected_error: &str) {
    let output = run_default_branch(
        repo,
        "test(default-branch): reject invalid preflight",
        head,
        Some(receipt),
        &[],
    );

    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr={}",
        text(&output.stderr)
    );
    assert!(
        text(&output.stderr).contains(expected_error),
        "expected {expected_error:?}, stderr={}",
        text(&output.stderr)
    );
    assert_eq!(git_trim(repo, &["rev-parse", "HEAD"]), head);
    assert!(!receipt.exists());
}

#[test]
fn dry_run_emits_preview_without_mutating_or_writing_a_receipt() {
    let repo = init_repo_with_head();
    stage_change(repo.path());
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);

    let output = run_default_branch(
        repo.path(),
        "docs(policy): preview contract",
        &head,
        None,
        &["--dry-run", "--format", "json"],
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        text(&output.stderr)
    );
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), head);
    let preview: Value = serde_json::from_slice(&output.stdout).expect("preview JSON");
    assert_eq!(
        preview["schema_version"],
        "cli.semantic-commit.default-branch.preview.v1"
    );
    assert_eq!(preview["data"]["mode"], "default-branch");
    assert_eq!(preview["data"]["default_branch"], "main");
    assert_eq!(
        preview["data"]["completion"]["default_branch_committed"],
        false
    );
    assert_eq!(
        preview["data"]["completion"]["provider_delivery_attempted"],
        false
    );
}

#[test]
fn dry_run_rejects_a_receipt_destination() {
    let repo = init_repo_with_head();
    stage_change(repo.path());
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let receipt_dir = tempfile::tempdir().expect("receipt directory");
    let receipt = receipt_dir.path().join("receipt.json");

    let output = run_default_branch(
        repo.path(),
        "docs(policy): preview contract",
        &head,
        Some(&receipt),
        &["--dry-run"],
    );

    assert_eq!(output.status.code(), Some(64));
    assert!(text(&output.stderr).contains("not accepted with --dry-run"));
    assert!(!receipt.exists());
}

#[test]
fn dry_run_rejects_message_out_without_writing_it() {
    let repo = init_repo_with_head();
    stage_change(repo.path());
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let output_dir = tempfile::tempdir().expect("message output directory");
    let message_out = output_dir.path().join("message.txt");

    let output = run_default_branch(
        repo.path(),
        "docs(policy): reject message output",
        &head,
        None,
        &[
            "--message-out",
            message_out.to_str().expect("message output path"),
            "--dry-run",
        ],
    );

    assert_eq!(output.status.code(), Some(64));
    assert!(text(&output.stderr).contains("--message-out"));
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), head);
    assert!(!message_out.exists());
}

#[test]
fn duplicate_singleton_bindings_fail_usage_before_repository_access() {
    let repo = init_repo_with_head();
    common::git(repo.path(), &["config", "gpg.format", "ssh"]);
    common::git(
        repo.path(),
        &[
            "config",
            "user.signingkey",
            "/definitely/missing/default-branch-key",
        ],
    );
    stage_change(repo.path());
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let other_repo = init_repo_with_head();
    let receipt_dir = tempfile::tempdir().expect("receipt directory");
    let receipt_a = receipt_dir.path().join("receipt-a.json");
    let receipt_b = receipt_dir.path().join("receipt-b.json");
    let invalid_head = "a".repeat(40);
    let repo_path = repo.path().to_str().expect("repository path");
    let other_path = other_repo.path().to_str().expect("other repository path");
    let receipt_a_path = receipt_a.to_str().expect("receipt path");
    let receipt_b_path = receipt_b.to_str().expect("receipt path");

    let cases = vec![
        vec!["--repo", repo_path, "--repo", other_path, "--dry-run"],
        vec!["--repo", other_path, "--repo", repo_path, "--dry-run"],
        vec![
            "--expect-head",
            &head,
            "--expect-head",
            &invalid_head,
            "--dry-run",
        ],
        vec![
            "--expect-head",
            &invalid_head,
            "--expect-head",
            &head,
            "--dry-run",
        ],
        vec!["--format", "text", "--format", "json", "--dry-run"],
        vec!["--format", "text", "--json", "--dry-run"],
        vec![
            "--receipt-out",
            receipt_a_path,
            "--receipt-out",
            receipt_b_path,
        ],
        vec![
            "--receipt-out",
            receipt_b_path,
            "--receipt-out",
            receipt_a_path,
        ],
    ];

    for extra in cases {
        let output = run_default_branch(
            repo.path(),
            "docs(policy): reject duplicate binding",
            &head,
            None,
            &extra,
        );
        assert_eq!(
            output.status.code(),
            Some(64),
            "args={extra:?}, stderr={}",
            text(&output.stderr)
        );
        assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), head);
        assert!(!receipt_a.exists());
        assert!(!receipt_b.exists());
    }
}

#[test]
fn message_syntax_errors_use_the_default_branch_usage_exit() {
    let repo = init_repo_with_head();
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let message_file = repo.path().join("message.txt");
    fs::write(&message_file, "docs(policy): conflicting source\n").expect("message file");
    let message_file = message_file.to_str().expect("message file path");
    let cases = [
        vec!["--message"],
        vec!["--type"],
        vec![
            "--message",
            "docs(policy): one source",
            "--message-file",
            message_file,
        ],
        vec!["--type", "docs"],
        vec!["--subject", "missing type"],
    ];

    for syntax in cases {
        let mut args = vec!["default-branch", "--expect-head", &head, "--dry-run"];
        args.extend(syntax.iter().copied());
        let output = common::run_semantic_commit_output(repo.path(), &args, &[], None);
        assert_eq!(
            output.status.code(),
            Some(64),
            "args={args:?}, stderr={}",
            text(&output.stderr)
        );
        assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), head);
    }
}

#[test]
fn remote_free_primary_default_creates_one_signed_commit_and_final_receipt() {
    let repo = init_repo_with_head();
    let _signing = configure_ssh_signing(repo.path());
    stage_change(repo.path());
    let old_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let receipt_dir = tempfile::tempdir().expect("receipt directory");
    let receipt = receipt_dir.path().join("receipt.json");

    let output = run_default_branch(
        repo.path(),
        "docs(policy): complete local contract",
        &old_head,
        Some(&receipt),
        &["--automation", "--format", "json"],
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        text(&output.stderr)
    );
    let new_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    assert_ne!(new_head, old_head);
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD^1"]), old_head);
    assert_eq!(git_trim(repo.path(), &["status", "--porcelain"]), "");
    assert_eq!(
        git_trim(repo.path(), &["log", "-1", "--format=%G?", "HEAD"]),
        "G"
    );

    let parsed = read_strict(&receipt).expect("strict final receipt");
    assert_eq!(parsed.data.default_branch, "main");
    assert_eq!(parsed.data.old_head, old_head);
    assert_eq!(parsed.data.new_head, new_head);
    assert_eq!(parsed.data.remote.mode, "remote-free");
    assert_eq!(parsed.data.remote.cached_relation_before, "untracked");
    assert_eq!(parsed.data.remote.cached_relation_after, "untracked");
    assert!(parsed.data.completion.default_branch_committed);
    assert!(!parsed.data.completion.provider_delivery_attempted);
    assert!(!parsed.data.completion.provider_delivered);

    let stdout: Value = serde_json::from_slice(&output.stdout).expect("result JSON");
    let on_disk: Value =
        serde_json::from_slice(&fs::read(&receipt).expect("receipt bytes")).expect("receipt JSON");
    assert_eq!(stdout, on_disk);
}

#[test]
fn accepted_and_unstated_delivery_waivers_round_trip_in_final_receipts() {
    for (waiver, expected) in [
        (
            "maintainer authorized this local-only repair",
            Some("maintainer authorized this local-only repair"),
        ),
        ("1", None),
    ] {
        let repo = init_repo_with_head();
        let _signing = configure_ssh_signing(repo.path());
        stage_change(repo.path());
        let old_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
        let receipt_dir = tempfile::tempdir().expect("receipt directory");
        let receipt = receipt_dir.path().join("receipt.json");

        let output = run_default_branch_with_env(
            repo.path(),
            "docs(policy): record delivery waiver",
            &old_head,
            Some(&receipt),
            &["--format", "json"],
            &[("AGENT_RUNTIME_DEFAULT_DELIVERY_WAIVER", waiver)],
        );

        assert_eq!(
            output.status.code(),
            Some(0),
            "stderr={}",
            text(&output.stderr)
        );
        let parsed = read_strict(&receipt).expect("strict final receipt");
        assert_eq!(parsed.data.delivery_waiver.as_deref(), expected);
        let receipt_json: Value =
            serde_json::from_slice(&fs::read(&receipt).expect("receipt bytes"))
                .expect("receipt JSON");
        assert_eq!(
            receipt_json["data"]
                .get("delivery_waiver")
                .and_then(Value::as_str),
            expected
        );
    }
}

#[test]
fn linked_worktree_is_rejected_before_commit_or_receipt() {
    let repo = init_repo_with_head();
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let linked_root = tempfile::tempdir().expect("linked worktree root");
    let linked = linked_root.path().join("linked");
    common::git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked-test",
            linked.to_str().expect("linked worktree path"),
            "HEAD",
        ],
    );
    stage_change(&linked);
    let receipt_dir = tempfile::tempdir().expect("receipt directory");
    let receipt = receipt_dir.path().join("receipt.json");

    assert_preflight_failure(&linked, &head, &receipt, "primary checkout");
}

#[test]
fn detached_head_is_rejected_before_commit_or_receipt() {
    let repo = init_repo_with_head();
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    common::git(repo.path(), &["checkout", "-q", "--detach", "HEAD"]);
    stage_change(repo.path());
    let receipt_dir = tempfile::tempdir().expect("receipt directory");
    let receipt = receipt_dir.path().join("receipt.json");

    assert_preflight_failure(repo.path(), &head, &receipt, "attached HEAD");
}

#[test]
fn unstaged_and_untracked_dirt_are_rejected_before_commit_or_receipt() {
    for dirt in ["unstaged", "untracked"] {
        let repo = init_repo_with_head();
        stage_change(repo.path());
        if dirt == "unstaged" {
            common::write_file(repo.path(), "change.txt", "changed again\n");
        } else {
            common::write_file(repo.path(), "untracked.txt", "untracked\n");
        }
        let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
        let receipt_dir = tempfile::tempdir().expect("receipt directory");
        let receipt = receipt_dir.path().join("receipt.json");

        assert_preflight_failure(
            repo.path(),
            &head,
            &receipt,
            "unstaged or untracked changes",
        );
    }
}

#[test]
fn no_staged_changes_are_rejected_before_commit_or_receipt() {
    let repo = init_repo_with_head();
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let receipt_dir = tempfile::tempdir().expect("receipt directory");
    let receipt = receipt_dir.path().join("receipt.json");

    assert_preflight_failure(repo.path(), &head, &receipt, "no staged changes");
}

#[test]
fn in_progress_git_operation_is_rejected_before_commit_or_receipt() {
    let repo = init_repo_with_head();
    stage_change(repo.path());
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    fs::write(repo.path().join(".git/MERGE_HEAD"), format!("{head}\n")).expect("merge marker");
    let receipt_dir = tempfile::tempdir().expect("receipt directory");
    let receipt = receipt_dir.path().join("receipt.json");

    assert_preflight_failure(repo.path(), &head, &receipt, "MERGE_HEAD");
}

#[cfg(unix)]
#[test]
fn symlink_receipt_parent_is_rejected_before_commit_or_receipt() {
    use std::os::unix::fs::symlink;

    let repo = init_repo_with_head();
    stage_change(repo.path());
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let receipt_root = tempfile::tempdir().expect("receipt root");
    let actual = receipt_root.path().join("actual");
    let linked = receipt_root.path().join("linked");
    fs::create_dir(&actual).expect("actual receipt directory");
    symlink(&actual, &linked).expect("receipt parent symlink");
    let receipt = linked.join("receipt.json");

    assert_preflight_failure(repo.path(), &head, &receipt, "parent must not be a symlink");
}

#[test]
fn aligned_cached_default_succeeds_without_contacting_the_remote() {
    let repo = init_repo_with_head();
    let _signing = configure_ssh_signing(repo.path());
    let old_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    configure_cached_default(repo.path(), &old_head);
    stage_change(repo.path());
    let receipt_dir = tempfile::tempdir().expect("receipt directory");
    let receipt = receipt_dir.path().join("receipt.json");

    let output = run_default_branch(
        repo.path(),
        "docs(policy): complete cached contract",
        &old_head,
        Some(&receipt),
        &["--format", "json"],
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        text(&output.stderr)
    );
    let parsed = read_strict(&receipt).expect("strict final receipt");
    assert_eq!(parsed.data.remote.mode, "cached-upstream");
    assert_eq!(parsed.data.remote.upstream.as_deref(), Some("origin/main"));
    assert_eq!(parsed.data.remote.cached_relation_before, "aligned");
    assert_eq!(parsed.data.remote.cached_relation_after, "ahead-by-one");
    assert!(!parsed.data.remote.network_observed);
    assert!(!parsed.data.remote.provider_mutated);
}

#[test]
fn primary_non_default_branch_is_rejected_by_cached_default_identity() {
    let repo = init_repo_with_head();
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    configure_cached_default(repo.path(), &head);
    common::git(repo.path(), &["branch", "-m", "feature"]);
    common::git(
        repo.path(),
        &["update-ref", "refs/remotes/origin/feature", &head],
    );
    common::git(repo.path(), &["config", "branch.feature.remote", "origin"]);
    common::git(
        repo.path(),
        &["config", "branch.feature.merge", "refs/heads/feature"],
    );
    stage_change(repo.path());

    let output = run_default_branch(
        repo.path(),
        "docs(policy): reject non-default",
        &head,
        None,
        &["--dry-run"],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("not the authoritative cached default branch"));
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), head);
}

#[test]
fn removed_flags_are_unknown_and_never_mutate() {
    for removed in ["--expected-branch", "--remote-mode", "--validate-only"] {
        let repo = init_repo_with_head();
        stage_change(repo.path());
        let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
        let output = run_default_branch(
            repo.path(),
            "docs(policy): reject removed flag",
            &head,
            None,
            &[removed, "unsupported", "--dry-run"],
        );

        assert_eq!(
            output.status.code(),
            Some(64),
            "{removed}: stderr={}",
            text(&output.stderr)
        );
        assert!(text(&output.stderr).contains("unknown argument"));
        assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), head);
    }
}

#[test]
fn configured_remote_without_cached_default_fails_closed() {
    let repo = init_repo_with_head();
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    add_remote(repo.path());
    common::git(
        repo.path(),
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    common::git(repo.path(), &["config", "branch.main.remote", "origin"]);
    common::git(
        repo.path(),
        &["config", "branch.main.merge", "refs/heads/main"],
    );
    stage_change(repo.path());

    let output = run_default_branch(
        repo.path(),
        "docs(policy): reject missing cached default",
        &head,
        None,
        &["--dry-run"],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("cached default branch cannot be resolved"));
}

#[test]
fn ambiguous_cached_default_fails_closed() {
    let repo = init_repo_with_head();
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    configure_cached_default(repo.path(), &head);
    common::git(
        repo.path(),
        &["update-ref", "refs/remotes/origin/trunk", &head],
    );
    common::git(
        repo.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    stage_change(repo.path());

    let output = run_default_branch(
        repo.path(),
        "docs(policy): reject ambiguous default",
        &head,
        None,
        &["--dry-run"],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("not the authoritative cached default branch"));
}

#[test]
fn behind_cached_default_fails_before_commit() {
    let repo = init_repo_with_head();
    let local_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    common::write_file(repo.path(), "remote.txt", "remote\n");
    common::git(repo.path(), &["add", "remote.txt"]);
    common::git(
        repo.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "docs: remote change",
        ],
    );
    let upstream_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    common::git(repo.path(), &["reset", "--hard", &local_head]);
    configure_cached_default(repo.path(), &upstream_head);
    stage_change(repo.path());

    let output = run_default_branch(
        repo.path(),
        "docs(policy): reject behind state",
        &local_head,
        None,
        &["--dry-run"],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("behind relative"));
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), local_head);
}

#[test]
fn diverged_cached_default_fails_before_commit() {
    let repo = init_repo_with_head();
    let base = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    common::write_file(repo.path(), "remote.txt", "remote\n");
    common::git(repo.path(), &["add", "remote.txt"]);
    common::git(
        repo.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "docs: remote change",
        ],
    );
    let upstream_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    common::git(repo.path(), &["reset", "--hard", &base]);
    common::write_file(repo.path(), "local.txt", "local\n");
    common::git(repo.path(), &["add", "local.txt"]);
    common::git(
        repo.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "docs: local change",
        ],
    );
    let local_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    configure_cached_default(repo.path(), &upstream_head);
    stage_change(repo.path());

    let output = run_default_branch(
        repo.path(),
        "docs(policy): reject diverged state",
        &local_head,
        None,
        &["--dry-run"],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("diverged relative"));
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), local_head);
}

#[test]
fn already_ahead_cached_default_fails_before_commit() {
    let repo = init_repo_with_head();
    let upstream_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    configure_cached_default(repo.path(), &upstream_head);
    common::write_file(repo.path(), "local.txt", "local\n");
    common::git(repo.path(), &["add", "local.txt"]);
    common::git(
        repo.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "docs: local change",
        ],
    );
    let local_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    stage_change(repo.path());

    let output = run_default_branch(
        repo.path(),
        "docs(policy): reject ahead state",
        &local_head,
        None,
        &["--dry-run"],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("already-ahead relative"));
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), local_head);
}

#[test]
fn structured_message_construction_is_shared_with_ordinary_commit() {
    let repo = init_repo_with_head();
    stage_change(repo.path());
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "default-branch",
            "--expect-head",
            &head,
            "--type",
            "DOCS",
            "--scope",
            "POLICY",
            "--subject",
            "share typed message construction",
            "--body-bullet",
            "use one parser for both commands",
            "--dry-run",
            "--format",
            "json",
        ],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        text(&output.stderr)
    );
}

#[test]
fn relative_repo_path_is_rejected() {
    let repo = init_repo_with_head();
    stage_change(repo.path());
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "default-branch",
            "--repo",
            ".",
            "--expect-head",
            &head,
            "--message",
            "docs(policy): reject relative target",
            "--dry-run",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("--repo must be an absolute"));
}

#[test]
fn signing_failure_leaves_head_unchanged_and_writes_no_receipt() {
    let repo = init_repo_with_head();
    common::git(repo.path(), &["config", "gpg.format", "ssh"]);
    common::git(
        repo.path(),
        &[
            "config",
            "user.signingkey",
            "/definitely/missing/default-branch-key",
        ],
    );
    stage_change(repo.path());
    let old_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let receipt_dir = tempfile::tempdir().expect("receipt directory");
    let receipt = receipt_dir.path().join("receipt.json");

    let output = run_default_branch(
        repo.path(),
        "docs(policy): preserve head on signing failure",
        &old_head,
        Some(&receipt),
        &[],
    );

    assert_ne!(output.status.code(), Some(0));
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), old_head);
    assert!(!receipt.exists());
}
