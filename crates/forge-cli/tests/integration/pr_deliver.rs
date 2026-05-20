//! Sprint 6 integration tests for the `pr deliver` macro. These pin the
//! dry-run plan envelope and the CLI surface — full-chain end-to-end with all
//! six atoms (auth_status / repo_view / create / wait_checks / ready / merge)
//! is exercised in Sprint 7's parity harness, where the fixture corpus
//! handles real-shaped responses for every atom.

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

const FORBIDDEN_STUB: &str = "#!/bin/sh\necho 'should not run during dry-run' >&2\nexit 99\n";

#[test]
fn pr_deliver_dry_run_lists_all_six_steps_in_order() {
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
            "create",
            "wait_checks",
            "ready",
            "merge",
        ]
    );
    assert_eq!(env["data"]["no_merge"], false);
    assert_eq!(env["data"]["kind"], "feature");
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
        vec!["auth_status", "repo_view", "create", "wait_checks"]
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
