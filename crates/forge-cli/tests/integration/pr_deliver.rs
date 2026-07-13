//! Sprint 6 integration tests for the `pr deliver` macro. These pin the
//! dry-run plan envelope and the CLI surface — full-chain end-to-end with all
//! six atoms (auth_status / repo_view / create / wait_checks / ready / merge)
//! is exercised in Sprint 7's parity harness, where the fixture corpus
//! handles real-shaped responses for every atom.

use std::fs;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::support::{
    StubEnv, parse_envelope, run_forge_cli, run_forge_cli_in, write_label_catalog,
};

const FORBIDDEN_STUB: &str = "#!/bin/sh\necho 'should not run during dry-run' >&2\nexit 99\n";

#[test]
fn pr_deliver_strict_labels_dry_run_rejects_missing_catalog_before_provider() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "demo",
            "--body",
            "## Summary\nx\n\n## Test plan\ny\n",
            "--label",
            "type::feature",
            "--strict-labels",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "label_catalog_missing");
}

#[test]
fn pr_deliver_strict_labels_live_rejects_missing_catalog_before_provider() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "demo",
            "--body",
            "## Summary\nx\n\n## Test plan\ny\n",
            "--label",
            "type::feature",
            "--strict-labels",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "label_catalog_missing");
}

#[test]
fn pr_deliver_strict_labels_valid_catalog_passes_dry_run_for_supported_providers() {
    let (_catalog_tempdir, catalog) = write_label_catalog();

    for provider in ["github", "gitlab"] {
        let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
        let out = run_forge_cli(
            &stub,
            &[
                "--provider",
                provider,
                "--dry-run",
                "--format",
                "json",
                "pr",
                "deliver",
                "--kind",
                "feature",
                "--title",
                "demo",
                "--body",
                "## Summary\nx\n\n## Test plan\ny\n",
                "--label",
                "type::feature",
                "--label-catalog",
                &catalog,
                "--strict-labels",
            ],
        );

        assert_eq!(
            out.code, 0,
            "provider={provider} stdout={} stderr={}",
            out.stdout, out.stderr
        );
    }
}

#[test]
fn pr_deliver_dry_run_lists_all_eight_steps_in_order() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "demo",
            "--body",
            "## Summary\nx\n\n## Test plan\ny\n",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.deliver.v1");
    let steps: Vec<&str> = env["data"]["plan_steps"]
        .as_array()
        .expect("plan_steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        steps,
        vec![
            "auth_status",
            "repo_view",
            "lookup",
            "create",
            "wait_checks",
            "ready",
            "merge",
            "issue_closeout",
        ]
    );
    assert_eq!(env["data"]["no_merge"], false);
    assert_eq!(env["data"]["kind"], "feature");
    let wait_plan = env["data"]["plan_steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["step"] == "wait_checks")
        .expect("wait_checks step present")["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect::<Vec<_>>();
    assert!(wait_plan.iter().any(|s| s == "--required"), "{wait_plan:?}");
    let json_idx = wait_plan
        .iter()
        .position(|s| s == "--json")
        .expect("--json present");
    assert!(wait_plan[json_idx + 1].contains("bucket"), "{wait_plan:?}");
    assert!(
        !wait_plan[json_idx + 1].contains("conclusion"),
        "{wait_plan:?}"
    );
    assert!(
        !wait_plan[json_idx + 1].contains("isRequired"),
        "{wait_plan:?}"
    );
}

#[test]
fn pr_deliver_dry_run_rejects_invalid_enabled_review_convergence_config() {
    let repo = TempDir::new().expect("tempdir");
    fs::write(
        repo.path().join(".forge-cli.toml"),
        "[review_convergence]\nrequire = true\nquiet_period = \"3601s\"\n",
    )
    .expect("write config");
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
            "deliver",
            "--kind",
            "feature",
            "--title",
            "demo",
            "--body",
            "## Summary\nx\n\n## Test plan\ny\n",
        ],
        Some(repo.path()),
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "invalid_review_convergence_config");
}

#[test]
fn pr_deliver_github_dry_run_exposes_enabled_review_convergence() {
    let repo = TempDir::new().expect("tempdir");
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
            "deliver",
            "--kind",
            "feature",
            "--title",
            "demo",
            "--body",
            "## Summary\nx\n\n## Test plan\ny\n",
            "--review-convergence",
        ],
        Some(repo.path()),
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["review_convergence"]["require"], true);
}

#[test]
fn pr_deliver_gitlab_dry_run_rejects_enabled_review_convergence() {
    let repo = TempDir::new().expect("tempdir");
    let stub = StubEnv::new().glab_stub(FORBIDDEN_STUB);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "demo",
            "--body",
            "## Summary\nx\n\n## Test plan\ny\n",
            "--review-convergence",
        ],
        Some(repo.path()),
    );

    assert_eq!(out.code, 64, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "provider_unsupported");
}

#[test]
fn pr_deliver_gitlab_dry_run_no_merge_exempts_review_convergence() {
    let repo = TempDir::new().expect("tempdir");
    let stub = StubEnv::new().glab_stub(FORBIDDEN_STUB);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "demo",
            "--body",
            "## Summary\nx\n\n## Test plan\ny\n",
            "--review-convergence",
            "--no-merge",
        ],
        Some(repo.path()),
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["no_merge"], true);
    assert!(env["data"].get("review_convergence").is_none());
}

#[test]
fn pr_deliver_dry_run_no_merge_ignores_review_convergence_config() {
    let repo = TempDir::new().expect("tempdir");
    fs::write(
        repo.path().join(".forge-cli.toml"),
        "[review_convergence]\nrequire = true\nquiet_period = \"3601s\"\n",
    )
    .expect("write config");
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
            "deliver",
            "--kind",
            "feature",
            "--title",
            "demo",
            "--body",
            "## Summary\nx\n\n## Test plan\ny\n",
            "--no-merge",
        ],
        Some(repo.path()),
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["no_merge"], true);
}

#[test]
fn pr_deliver_dry_run_no_merge_excludes_ready_and_merge() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "bug",
            "--title",
            "demo",
            "--body",
            "## Summary\nx\n\n## Test plan\ny\n",
            "--no-merge",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let steps: Vec<&str> = env["data"]["plan_steps"]
        .as_array()
        .expect("plan_steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        steps,
        vec![
            "auth_status",
            "repo_view",
            "lookup",
            "create",
            "wait_checks"
        ]
    );
    assert_eq!(env["data"]["no_merge"], true);
    assert_eq!(env["data"]["kind"], "bug");
}

#[test]
fn pr_deliver_dry_run_method_override_threads_through_merge_plan() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "demo",
            "--body",
            "## Summary\nx\n\n## Test plan\ny\n",
            "--method",
            "rebase",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let merge_plan = env["data"]["plan_steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["step"] == "merge")
        .expect("merge step present")["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect::<Vec<_>>();
    assert!(
        merge_plan.iter().any(|s| s == "--rebase"),
        "expected --rebase in merge plan, got {merge_plan:?}"
    );
}

#[test]
fn pr_deliver_dry_run_reports_local_preflight_without_backend() {
    // FORBIDDEN_STUB exits 99 if the gh backend is ever invoked. A bad body
    // must surface in data.local_preflight without aborting (dry-run exits 0)
    // and without calling the provider.
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "demo",
            "--head",
            "feat/demo",
            "--body",
            "no required sections here",
        ],
    );
    assert_eq!(out.code, 0, "dry-run must not abort; stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let preflight = env["data"]["local_preflight"]
        .as_array()
        .expect("local_preflight array present");

    let lookup = |rule: &str| {
        preflight
            .iter()
            .find(|v| v["rule"] == rule)
            .unwrap_or_else(|| panic!("missing verdict for {rule}: {preflight:?}"))
    };

    // Deterministic, git-independent rules.
    assert_eq!(lookup("branch_name")["ok"], true);
    assert_eq!(lookup("branch_kind")["ok"], true);
    assert_eq!(lookup("title_length")["ok"], true);
    // The bad body surfaces both section failures in one sweep.
    assert_eq!(lookup("body_summary")["ok"], false);
    assert_eq!(lookup("body_summary")["code"], "body_missing_summary");
    assert_eq!(lookup("body_test_plan")["ok"], false);
    assert_eq!(lookup("body_test_plan")["code"], "body_missing_test_plan");
    // Rule 11 — the local-path verdicts are present and pass for portable text.
    assert_eq!(lookup("title_local_path")["ok"], true);
    assert_eq!(lookup("body_local_path")["ok"], true);
    // The worktree/head rules are present too (their verdict depends on the
    // local git state, so only presence is asserted here).
    assert!(preflight.iter().any(|v| v["rule"] == "worktree_clean"));
    assert!(preflight.iter().any(|v| v["rule"] == "head_pushed"));
}

#[test]
fn pr_deliver_help_lists_every_documented_flag() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["pr", "deliver", "--help"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    for flag in [
        "--kind",
        "--title",
        "--body",
        "--body-file",
        "--head",
        "--base",
        "--method",
        "--reviewer",
        "--label",
        "--timeout",
        "--no-merge",
        "--allow-non-default-base",
    ] {
        assert!(
            out.stdout.contains(flag),
            "expected `{flag}` in help, got: {}",
            out.stdout
        );
    }
}

#[test]
fn pr_deliver_dry_run_threads_labels_into_create_step() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "demo",
            "--body",
            "## Summary\nx\n\n## Test plan\ny\n",
            "--label",
            "type::feature",
            "--label",
            "area::cli",
            "--label",
            "size::m",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let create_plan = env["data"]["plan_steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["step"] == "create")
        .expect("create step present")["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect::<Vec<_>>();
    let label_count = create_plan
        .iter()
        .filter(|s| s.as_str() == "--label")
        .count();
    assert_eq!(label_count, 3, "{create_plan:?}");
    assert!(
        create_plan.iter().any(|s| s == "type::feature"),
        "{create_plan:?}"
    );
    assert!(
        create_plan.iter().any(|s| s == "area::cli"),
        "{create_plan:?}"
    );
    assert!(
        create_plan.iter().any(|s| s == "size::m"),
        "{create_plan:?}"
    );
}
