use std::fs;
use std::process::Command;

use nils_common::local_default_receipt::read_strict;
use serde_json::Value;

use crate::common;

fn as_str(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("utf-8 output")
}

fn git_trim(repo: &std::path::Path, args: &[&str]) -> String {
    let output = common::git_output(repo, args);
    assert!(output.status.success(), "git {:?} failed", args);
    String::from_utf8(output.stdout)
        .expect("utf-8 git output")
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

fn configure_ssh_signing(repo: &std::path::Path) -> tempfile::TempDir {
    let signing_dir = tempfile::tempdir().expect("signing dir");
    let key = signing_dir.path().join("test-signing-key");
    let output = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .output()
        .expect("launch ssh-keygen");
    assert!(
        output.status.success(),
        "ssh-keygen failed: {}",
        as_str(&output.stderr)
    );
    let public_key = fs::read_to_string(key.with_extension("pub")).expect("public key");
    let allowed = signing_dir.path().join("allowed-signers");
    fs::write(&allowed, format!("test@example.com {public_key}")).expect("write allowed signers");
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

fn configure_cached_upstream(repo: &std::path::Path, sha: &str) {
    common::git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "ssh://invalid.example.invalid/sympoies/demo.git",
        ],
    );
    common::git(repo, &["update-ref", "refs/remotes/origin/main", sha]);
    common::git(repo, &["config", "branch.main.remote", "origin"]);
    common::git(repo, &["config", "branch.main.merge", "refs/heads/main"]);
}

#[test]
fn top_level_help_exposes_local_default_as_a_distinct_command() {
    let repo = init_repo_with_head();
    let output = common::run_semantic_commit_output(repo.path(), &["--help"], &[], None);

    assert_eq!(output.status.code(), Some(0));
    assert!(
        as_str(&output.stdout).contains("local-default"),
        "stdout was: {}",
        as_str(&output.stdout)
    );
}

#[test]
fn local_default_validate_only_requires_remote_acknowledgement_when_remotes_exist() {
    let repo = init_repo_with_head();
    common::git(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/demo.git",
        ],
    );
    common::write_file(repo.path(), "change.txt", "change\n");
    common::git(repo.path(), &["add", "change.txt"]);
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let receipt = repo.path().parent().expect("parent").join("receipt.json");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "local-default",
            "--message",
            "docs(policy): update contract",
            "--expect-head",
            &head,
            "--expected-branch",
            "main",
            "--receipt-out",
            receipt.to_str().expect("receipt path"),
            "--validate-only",
            "--format",
            "json",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("--remote-mode local-only"));
    assert!(!receipt.exists());
}

#[test]
fn local_default_dry_run_reports_contract_without_mutating_head_or_receipt() {
    let repo = init_repo_with_head();
    common::write_file(repo.path(), "change.txt", "change\n");
    common::git(repo.path(), &["add", "change.txt"]);
    let head = git_trim(repo.path(), &["rev-parse", "HEAD"]);

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "local-default",
            "--message",
            "docs(policy): update contract",
            "--expect-head",
            &head,
            "--expected-branch",
            "main",
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
        "stderr was: {}",
        as_str(&output.stderr)
    );
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), head);
    let json: Value = serde_json::from_slice(&output.stdout).expect("json envelope");
    assert_eq!(
        json["schema_version"],
        "cli.semantic-commit.local-default.v1"
    );
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["mode"], "local-default");
    assert_eq!(json["data"]["branch"], "main");
    assert_eq!(json["data"]["old_head"], head);
    assert_eq!(json["data"]["remote"]["configured_count"], 0);
    assert_eq!(json["data"]["remote"]["network_observed"], false);
    assert_eq!(json["data"]["completion"]["local_default_committed"], false);
    assert_eq!(json["data"]["completion"]["provider_delivered"], false);

    let serialized = fs::read_to_string(repo.path().join(".git/config")).expect("config");
    assert!(!as_str(&output.stdout).contains(repo.path().to_str().expect("repo path")));
    assert!(!as_str(&output.stdout).contains(&serialized));
}

#[test]
fn local_default_rejects_a_symlink_receipt_parent_before_commit() {
    use std::os::unix::fs::symlink;

    let repo = init_repo_with_head();
    common::write_file(repo.path(), "change.txt", "change\n");
    common::git(repo.path(), &["add", "change.txt"]);
    let old_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let receipt_root = tempfile::tempdir().expect("receipt root");
    let actual = receipt_root.path().join("actual");
    let linked = receipt_root.path().join("linked");
    fs::create_dir(&actual).expect("actual receipt dir");
    symlink(&actual, &linked).expect("receipt parent symlink");
    let receipt = linked.join("receipt.json");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "local-default",
            "--message",
            "docs(policy): update contract",
            "--expect-head",
            &old_head,
            "--expected-branch",
            "main",
            "--receipt-out",
            receipt.to_str().expect("receipt path"),
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("parent must not be a symlink"));
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), old_head);
    assert!(!receipt.exists());
}

#[test]
fn local_default_remote_mode_without_upstream_remains_untracked() {
    let repo = init_repo_with_head();
    let _signing = configure_ssh_signing(repo.path());
    common::git(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/demo.git",
        ],
    );
    common::write_file(repo.path(), "change.txt", "change\n");
    common::git(repo.path(), &["add", "change.txt"]);
    let old_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let receipt_dir = tempfile::tempdir().expect("receipt dir");
    let receipt = receipt_dir.path().join("receipt.json");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "local-default",
            "--message",
            "docs(policy): update contract",
            "--expect-head",
            &old_head,
            "--expected-branch",
            "main",
            "--remote-mode",
            "local-only",
            "--receipt-out",
            receipt.to_str().expect("receipt path"),
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
        as_str(&output.stderr)
    );
    let parsed = read_strict(&receipt).expect("strict receipt");
    assert_eq!(parsed.data.remote.upstream, None);
    assert_eq!(parsed.data.remote.cached_relation_before, "untracked");
    assert_eq!(parsed.data.remote.cached_relation_after, "untracked");
}

#[test]
fn local_default_creates_one_verified_signed_commit_and_strict_receipt() {
    let repo = init_repo_with_head();
    let _signing = configure_ssh_signing(repo.path());
    common::write_file(repo.path(), "change.txt", "change\n");
    common::git(repo.path(), &["add", "change.txt"]);
    let old_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let receipt_dir = tempfile::tempdir().expect("receipt dir");
    let receipt = receipt_dir.path().join("receipt.json");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "local-default",
            "--message",
            "docs(policy): update contract",
            "--expect-head",
            &old_head,
            "--expected-branch",
            "main",
            "--receipt-out",
            receipt.to_str().expect("receipt path"),
            "--automation",
            "--format",
            "json",
        ],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
    let new_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    assert_ne!(new_head, old_head);
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD^1"]), old_head);
    assert_eq!(
        git_trim(repo.path(), &["rev-list", "--count", "HEAD^..HEAD"]),
        "1"
    );
    assert_eq!(
        git_trim(repo.path(), &["log", "-1", "--format=%G?", "HEAD"]),
        "G"
    );
    assert_eq!(git_trim(repo.path(), &["status", "--porcelain"]), "");

    let parsed = read_strict(&receipt).expect("strict receipt");
    assert_eq!(parsed.data.old_head, old_head);
    assert_eq!(parsed.data.new_head, new_head);
    assert_eq!(parsed.data.parent_sha, parsed.data.old_head);
    assert_eq!(parsed.data.signature, "verified-good");
    assert_eq!(parsed.data.staged_file_count, 1);
    assert_eq!(parsed.data.remote.configured_count, 0);
    assert_eq!(parsed.data.remote.mode, "none");
    assert!(!parsed.data.remote.network_observed);
    assert!(!parsed.data.remote.provider_mutated);

    let stdout: Value = serde_json::from_slice(&output.stdout).expect("json output");
    let receipt_json: Value =
        serde_json::from_slice(&fs::read(&receipt).expect("receipt bytes")).expect("receipt json");
    assert_eq!(stdout, receipt_json);
    assert!(!as_str(&output.stdout).contains(repo.path().to_str().expect("repo path")));
    assert!(!as_str(&output.stdout).contains("change.txt"));
}

#[test]
fn local_default_remote_mode_uses_only_cached_upstream_state() {
    let repo = init_repo_with_head();
    let _signing = configure_ssh_signing(repo.path());
    common::git(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            "ssh://invalid.example.invalid/sympoies/demo.git",
        ],
    );
    let old_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    common::git(
        repo.path(),
        &["update-ref", "refs/remotes/origin/main", &old_head],
    );
    common::git(repo.path(), &["config", "branch.main.remote", "origin"]);
    common::git(
        repo.path(),
        &["config", "branch.main.merge", "refs/heads/main"],
    );
    common::write_file(repo.path(), "change.txt", "change\n");
    common::git(repo.path(), &["add", "change.txt"]);
    let receipt_dir = tempfile::tempdir().expect("receipt dir");
    let receipt = receipt_dir.path().join("receipt.json");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "local-default",
            "--message",
            "docs(policy): update contract",
            "--expect-head",
            &old_head,
            "--expected-branch",
            "main",
            "--remote-mode",
            "local-only",
            "--receipt-out",
            receipt.to_str().expect("receipt path"),
            "--format",
            "json",
        ],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
    let parsed = read_strict(&receipt).expect("strict receipt");
    assert_eq!(parsed.data.remote.configured_count, 1);
    assert_eq!(parsed.data.remote.mode, "local-only");
    assert_eq!(parsed.data.remote.upstream.as_deref(), Some("origin/main"));
    assert_eq!(parsed.data.remote.cached_relation_before, "aligned");
    assert_eq!(parsed.data.remote.cached_relation_after, "ahead-by-one");
    assert!(!parsed.data.remote.network_observed);
}

#[test]
fn local_default_accepts_ahead_only_cached_upstream_and_records_exact_counts() {
    let repo = init_repo_with_head();
    let _signing = configure_ssh_signing(repo.path());
    let upstream_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    configure_cached_upstream(repo.path(), &upstream_head);

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

    common::write_file(repo.path(), "change.txt", "change\n");
    common::git(repo.path(), &["add", "change.txt"]);
    let old_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let receipt_dir = tempfile::tempdir().expect("receipt dir");
    let receipt = receipt_dir.path().join("receipt.json");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "local-default",
            "--message",
            "docs(policy): update ahead-only contract",
            "--expect-head",
            &old_head,
            "--expected-branch",
            "main",
            "--remote-mode",
            "local-only",
            "--receipt-out",
            receipt.to_str().expect("receipt path"),
            "--format",
            "json",
        ],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
    let new_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD^1"]), old_head);
    assert_eq!(
        git_trim(repo.path(), &["rev-list", "--count", "origin/main..HEAD"]),
        "2"
    );

    let parsed = read_strict(&receipt).expect("strict receipt");
    assert_eq!(parsed.data.old_head, old_head);
    assert_eq!(parsed.data.new_head, new_head);
    assert_eq!(parsed.data.remote.cached_relation_before, "ahead-by-one");
    assert_eq!(parsed.data.remote.cached_relation_after, "ahead-by-2");
    assert!(!parsed.data.remote.network_observed);
    assert!(!parsed.data.remote.provider_mutated);
    assert!(!parsed.data.completion.provider_delivered);
}

#[test]
fn local_default_rejects_head_behind_cached_upstream() {
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
    configure_cached_upstream(repo.path(), &upstream_head);
    common::write_file(repo.path(), "change.txt", "change\n");
    common::git(repo.path(), &["add", "change.txt"]);

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "local-default",
            "--message",
            "docs(policy): reject behind state",
            "--expect-head",
            &local_head,
            "--expected-branch",
            "main",
            "--remote-mode",
            "local-only",
            "--validate-only",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("behind relative to the cached upstream"));
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), local_head);
}

#[test]
fn local_default_rejects_head_diverged_from_cached_upstream() {
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
    configure_cached_upstream(repo.path(), &upstream_head);
    common::write_file(repo.path(), "change.txt", "change\n");
    common::git(repo.path(), &["add", "change.txt"]);

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "local-default",
            "--message",
            "docs(policy): reject diverged state",
            "--expect-head",
            &local_head,
            "--expected-branch",
            "main",
            "--remote-mode",
            "local-only",
            "--validate-only",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("diverged relative to the cached upstream"));
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), local_head);
}

#[test]
fn local_default_rejects_configured_upstream_with_missing_cached_ref() {
    let repo = init_repo_with_head();
    common::git(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            "ssh://invalid.example.invalid/sympoies/demo.git",
        ],
    );
    common::git(repo.path(), &["config", "branch.main.remote", "origin"]);
    common::git(
        repo.path(),
        &["config", "branch.main.merge", "refs/heads/main"],
    );
    common::write_file(repo.path(), "change.txt", "change\n");
    common::git(repo.path(), &["add", "change.txt"]);
    let local_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "local-default",
            "--message",
            "docs(policy): reject missing upstream ref",
            "--expect-head",
            &local_head,
            "--expected-branch",
            "main",
            "--remote-mode",
            "local-only",
            "--validate-only",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("cached ref cannot be resolved"));
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), local_head);
}

#[test]
fn local_default_signing_failure_leaves_head_unchanged() {
    let repo = init_repo_with_head();
    common::git(repo.path(), &["config", "gpg.format", "ssh"]);
    common::git(
        repo.path(),
        &[
            "config",
            "user.signingkey",
            "/definitely/missing/local-default-key",
        ],
    );
    common::write_file(repo.path(), "change.txt", "change\n");
    common::git(repo.path(), &["add", "change.txt"]);
    let old_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let receipt_dir = tempfile::tempdir().expect("receipt dir");
    let receipt = receipt_dir.path().join("receipt.json");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "local-default",
            "--message",
            "docs(policy): update contract",
            "--expect-head",
            &old_head,
            "--expected-branch",
            "main",
            "--receipt-out",
            receipt.to_str().expect("receipt path"),
        ],
        &[],
        None,
    );

    assert_ne!(output.status.code(), Some(0));
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), old_head);
    assert!(!receipt.exists());
}

#[test]
fn local_default_usage_documents_the_cross_repository_target() {
    // `--repo` is what binds a foreign target without moving the shell, so an
    // undocumented flag is the same as a missing one for a caller reading help.
    let repo = init_repo_with_head();
    let output =
        common::run_semantic_commit_output(repo.path(), &["local-default", "--help"], &[], None);

    assert_eq!(output.status.code(), Some(0));
    assert!(
        as_str(&output.stdout).contains("--repo <path>"),
        "stdout was: {}",
        as_str(&output.stdout)
    );
}

#[test]
fn local_default_records_a_stated_delivery_waiver_in_the_receipt() {
    let repo = init_repo_with_head();
    let _signing = configure_ssh_signing(repo.path());
    common::write_file(repo.path(), "change.txt", "change\n");
    common::git(repo.path(), &["add", "change.txt"]);
    let old_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let receipt_dir = tempfile::tempdir().expect("receipt dir");
    let receipt = receipt_dir.path().join("receipt.json");
    let reason = "maintainer authorized this cross-repo local completion";

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "local-default",
            "--message",
            "docs(policy): update contract",
            "--expect-head",
            &old_head,
            "--expected-branch",
            "main",
            "--receipt-out",
            receipt.to_str().expect("receipt path"),
            "--automation",
            "--format",
            "json",
        ],
        &[("AGENT_RUNTIME_DEFAULT_DELIVERY_WAIVER", reason)],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
    let parsed = read_strict(&receipt).expect("strict receipt");
    assert_eq!(parsed.data.delivery_waiver.as_deref(), Some(reason));
}

#[test]
fn local_default_omits_an_unstated_delivery_waiver() {
    // A value that states nothing is not evidence, so it is not recorded as if
    // it were. The receipt keeps the field absent instead.
    let repo = init_repo_with_head();
    let _signing = configure_ssh_signing(repo.path());
    common::write_file(repo.path(), "change.txt", "change\n");
    common::git(repo.path(), &["add", "change.txt"]);
    let old_head = git_trim(repo.path(), &["rev-parse", "HEAD"]);
    let receipt_dir = tempfile::tempdir().expect("receipt dir");
    let receipt = receipt_dir.path().join("receipt.json");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "local-default",
            "--message",
            "docs(policy): update contract",
            "--expect-head",
            &old_head,
            "--expected-branch",
            "main",
            "--receipt-out",
            receipt.to_str().expect("receipt path"),
            "--automation",
            "--format",
            "json",
        ],
        &[("AGENT_RUNTIME_DEFAULT_DELIVERY_WAIVER", "1")],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
    let parsed = read_strict(&receipt).expect("strict receipt");
    assert_eq!(parsed.data.delivery_waiver, None);
    let receipt_json: Value =
        serde_json::from_slice(&fs::read(&receipt).expect("receipt bytes")).expect("receipt json");
    assert!(receipt_json["data"].get("delivery_waiver").is_none());
}
