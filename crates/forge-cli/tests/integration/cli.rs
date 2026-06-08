//! High-level CLI behaviour: help, version, and --dry-run plumbing for the
//! two read-only atoms.

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
