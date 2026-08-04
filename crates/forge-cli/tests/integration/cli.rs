//! High-level CLI behaviour: help, version, and --dry-run plumbing for the
//! two read-only atoms.

use std::process::Command;

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

#[test]
fn help_lists_every_top_level_subcommand() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["--help"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    for sub in [
        "pr",
        "issue",
        "activity",
        "label",
        "inbox",
        "repo",
        "auth",
        "completion",
    ] {
        assert!(
            out.stdout.contains(sub),
            "--help missing {sub}: stdout={}",
            out.stdout
        );
    }
    assert!(
        !out.stdout.contains("--json"),
        "must not surface --json flag"
    );
}

#[test]
fn root_repo_help_is_provider_neutral() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["--help"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    for expected in [
        "--repo <REPOSITORY>",
        "Override the remote-derived repository path",
        "GitHub `owner/name`",
        "GitLab `group[/subgroup...]/project`",
        "Local `local:<slug>` or `<slug>`",
    ] {
        assert!(
            out.stdout.contains(expected),
            "root --repo help missing {expected}: stdout={}",
            out.stdout
        );
    }
    assert!(
        !out.stdout.contains("--repo <owner/name>"),
        "root --repo value name must not imply a GitHub-only shape: stdout={}",
        out.stdout
    );
}

#[test]
fn activity_cli_help_lists_every_v1_subcommand() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["activity", "--help"]);
    assert_eq!(out.code, 0);
    for sub in ["commits", "events", "feed", "summary"] {
        assert!(
            out.stdout.contains(sub),
            "activity --help missing {sub}: stdout={}",
            out.stdout
        );
    }
}

#[test]
fn activity_commits_help_describes_contract_inputs() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["activity", "commits", "--help"]);
    assert_eq!(out.code, 0);
    for expected in [
        "Search recent GitHub commits authored by a user",
        "GitHub login to inspect",
        "DATE_OR_DATETIME",
        "Only include commits authored at or after this date/datetime",
    ] {
        assert!(
            out.stdout.contains(expected),
            "activity commits --help missing {expected}: stdout={}",
            out.stdout
        );
    }
}

#[test]
fn inbox_cli_help_lists_every_v1_subcommand() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["inbox", "--help"]);
    assert_eq!(out.code, 0);
    for sub in ["status", "list", "next"] {
        assert!(
            out.stdout.contains(sub),
            "inbox --help missing {sub}: stdout={}",
            out.stdout
        );
    }
}

#[test]
fn label_help_lists_every_v1_subcommand() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["label", "--help"]);
    assert_eq!(out.code, 0);
    for sub in ["list", "audit", "ensure"] {
        assert!(
            out.stdout.contains(sub),
            "label --help missing {sub}: stdout={}",
            out.stdout
        );
    }
}

#[test]
fn repo_help_lists_governed_default_branch_delivery() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["repo", "--help"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    for sub in ["view", "push-default"] {
        assert!(
            out.stdout.contains(sub),
            "repo --help missing {sub}: stdout={}",
            out.stdout
        );
    }
}

#[test]
fn repo_push_default_help_exposes_guarded_contract_inputs() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["repo", "push-default", "--help"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    for expected in [
        "--head",
        "--expected-base",
        "--reason-file",
        "--default-branch-receipt",
        "normal fast-forward push",
    ] {
        assert!(
            out.stdout.contains(expected),
            "repo push-default --help missing {expected}: stdout={}",
            out.stdout
        );
    }
    assert!(
        !out.stdout.contains("--local-default-receipt"),
        "removed receipt option remains in help: stdout={}",
        out.stdout
    );
}

#[test]
fn repo_push_default_rejects_the_removed_receipt_option() {
    let stub = StubEnv::new();
    let out = run_forge_cli(
        &stub,
        &[
            "repo",
            "push-default",
            "--expected-base",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--reason-file",
            "reason.md",
            "--local-default-receipt",
            "receipt.json",
        ],
    );

    assert_eq!(out.code, 64, "stdout={} stderr={}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("--local-default-receipt"),
        "stderr={}",
        out.stderr
    );
}

#[test]
fn pr_help_lists_every_v1_subcommand() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["pr", "--help"]);
    assert_eq!(out.code, 0);
    for sub in [
        "create",
        "view",
        "list",
        "edit",
        "comment",
        "review",
        "review-threads",
        "reviews",
        "pending-review",
        "review-loop",
        "ready",
        "merge",
        "close",
        "checks",
        "wait-checks",
        "deliver",
    ] {
        assert!(
            out.stdout.contains(sub),
            "pr --help missing {sub}: stdout={}",
            out.stdout
        );
    }
}

#[test]
fn pr_review_loop_dry_run_uses_each_command_specific_schema_without_backend_calls() {
    let stub = StubEnv::new();
    let cases = [
        (
            vec!["pr", "review-loop", "inspect", "7"],
            "cli.forge-cli.pr.review-loop.inspect.v1",
        ),
        (
            vec![
                "pr",
                "review-loop",
                "observe",
                "7",
                "--expected-head",
                "abc123",
                "--findings-file",
                "findings.json",
            ],
            "cli.forge-cli.pr.review-loop.observe.v1",
        ),
        (
            vec![
                "pr",
                "review-loop",
                "extend",
                "7",
                "--expected-head",
                "abc123",
                "--expected-state",
                "sha256:tip",
                "--stop-code",
                "review_no_progress",
                "--budget-field",
                "max_no_progress_rounds",
                "--proposal-digest",
                "sha256:proposal",
                "--approval-comment",
                "42",
            ],
            "cli.forge-cli.pr.review-loop.extend.v1",
        ),
    ];

    for (command, schema) in cases {
        let mut args = vec![
            "--format",
            "json",
            "--dry-run",
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
        ];
        args.extend(command);
        let out = run_forge_cli(&stub, &args);
        assert_eq!(out.code, 0, "stderr={}", out.stderr);
        let envelope = parse_envelope(&out.stdout);
        assert_eq!(envelope["schema_version"], schema);
    }
}

#[test]
fn pr_review_help_distinguishes_thread_file_posting_and_validate() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["pr", "review", "--help"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    for expected in [
        "Requires --submit-review when posting",
        "may be inherited by `validate` without posting",
    ] {
        assert!(
            out.stdout.contains(expected),
            "pr review --help missing {expected}: stdout={}",
            out.stdout
        );
    }
}

#[test]
fn pr_review_threads_missing_subcommand_preserves_json_error_envelope() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["--format", "json", "pr", "review-threads"]);
    assert_eq!(out.code, 64, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "parse-error");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("subcommand"),
        "message should explain the missing subcommand: {envelope}"
    );
}

#[test]
fn issue_help_lists_every_v1_subcommand() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["issue", "--help"]);
    assert_eq!(out.code, 0);
    for sub in ["create", "view", "edit", "comment", "close", "reopen"] {
        assert!(
            out.stdout.contains(sub),
            "issue --help missing {sub}: stdout={}",
            out.stdout
        );
    }
}

#[test]
fn forced_provider_and_repo_require_host_for_unclassified_remote_authority() {
    let stub = StubEnv::new();
    let init = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(stub.tempdir.path())
        .status()
        .expect("initialize Git repository");
    assert!(init.success(), "git init failed: {init}");
    let add_remote = Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://gitlab.corp.example/group/project.git",
        ])
        .current_dir(stub.tempdir.path())
        .status()
        .expect("add custom-authority Git remote");
    assert!(add_remote.success(), "git remote add failed: {add_remote}");

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--repo",
            "group/project",
            "--dry-run",
            "--format",
            "json",
            "repo",
            "view",
        ],
    );

    assert_eq!(out.code, 64, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "provider_unsupported");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("pass --host"),
        "error must require an explicit host: {envelope}"
    );
}

#[test]
fn dry_run_auth_status_renders_plan_envelope() {
    let stub = StubEnv::new();
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "auth",
            "status",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(
        envelope["schema_version"], "cli.forge-cli.auth.status.v1",
        "schema_version mismatch: {envelope}"
    );
    assert_eq!(envelope["ok"], true);
    let plan = envelope["data"]["plan"].as_array().expect("plan array");
    assert_eq!(plan[1], "auth");
    assert_eq!(plan[2], "status");
    assert_eq!(envelope["data"]["provider"], "github");
}

#[test]
fn dry_run_repo_view_renders_plan_envelope_for_gitlab() {
    let stub = StubEnv::new();
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--dry-run",
            "--format",
            "json",
            "repo",
            "view",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(
        envelope["schema_version"], "cli.forge-cli.repo.view.v1",
        "schema_version mismatch: {envelope}"
    );
    let plan = envelope["data"]["plan"].as_array().expect("plan array");
    assert_eq!(plan[1], "repo");
    assert_eq!(plan[2], "view");
    assert!(plan.iter().any(|v| v == "-F"));
    assert_eq!(envelope["data"]["provider"], "gitlab");
}

/// `pr review-loop validate` end to end: the schema literal, the full payload
/// shape, and the claim the command is built on — that it reaches no backend.
/// The stub exits 99 on any invocation, so a single provider call fails this.
#[test]
fn pr_review_loop_validate_emits_its_schema_without_touching_a_backend() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho 'no backend expected' >&2\nexit 99\n");
    let findings = stub.tempdir.path().join("findings.json");
    std::fs::write(
        &findings,
        r#"[
          {"lifecycle_fingerprint":"correctness:a:one","disposition":"open"},
          {"lifecycle_fingerprint":"maintainability:b:two","disposition":"fixed"}
        ]"#,
    )
    .expect("write findings");

    let out = run_forge_cli(
        &stub,
        &[
            "--format",
            "json",
            "pr",
            "review-loop",
            "validate",
            "--findings-file",
            &findings.to_string_lossy(),
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(
        envelope["schema_version"],
        "cli.forge-cli.pr.review-loop.validate.v1"
    );
    let data = &envelope["data"];
    assert_eq!(data["shape"], "observation-array");
    assert_eq!(data["row_count"], 2);
    assert_eq!(data["identity_count"], 2);
    assert_eq!(data["blocking_count"], 1);
    assert_eq!(
        data["dispositions"],
        serde_json::json!([
            {"disposition": "open", "count": 1},
            {"disposition": "fixed", "count": 1},
            {"disposition": "accepted", "count": 0},
            {"disposition": "preference", "count": 0},
            {"disposition": "follow-up", "count": 0},
        ])
    );
    assert!(
        data.get("duplicate_identities").is_none(),
        "omitted when empty: {}",
        out.stdout
    );
    assert!(
        envelope.get("warnings").is_none()
            || envelope["warnings"].as_array().is_none_or(|w| w.is_empty()),
        "a clean payload warns about nothing: {}",
        out.stdout
    );
}

/// Provider globals are inert here by design; the command must not start
/// rejecting payloads because a provider was named.
#[test]
fn pr_review_loop_validate_ignores_provider_globals() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\nexit 99\n");
    let findings = stub.tempdir.path().join("findings.json");
    std::fs::write(
        &findings,
        r#"[{"lifecycle_fingerprint":"correctness:a:one","disposition":"open"}]"#,
    )
    .expect("write findings");

    let out = run_forge_cli(
        &stub,
        &[
            "--format",
            "json",
            "--provider",
            "gitlab",
            "pr",
            "review-loop",
            "validate",
            "--findings-file",
            &findings.to_string_lossy(),
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
}
