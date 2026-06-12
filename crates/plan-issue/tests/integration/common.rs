use std::path::Path;
use std::sync::LazyLock;

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};

/// Hermetic per-process `$PLAN_ISSUE_HOME` for every baseline invocation.
///
/// Without this, parallel tests share the host-default state dir
/// (`$XDG_STATE_HOME/plan-issue`) and race on lifecycle locks whose key is
/// the fixture repo/issue/profile triple, so a victim test exits 1 with an
/// empty stderr (issue: plan-tracking-testbed#61). nextest runs one process
/// per test, so a process-wide dir isolates each test while multi-invocation
/// tests keep state continuity within their own process.
static HERMETIC_STATE_DIR: LazyLock<tempfile::TempDir> = LazyLock::new(|| {
    tempfile::Builder::new()
        .prefix("plan-issue-state-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("create hermetic plan-issue state dir")
});

/// Build a deterministic baseline for plan-issue integration tests.
/// Tests can compose env/path overrides via `CmdOptions` instead of ad-hoc
/// shell-style setup in each test body; a later `with_env` push or an
/// explicit `--state-dir` flag still overrides the hermetic baseline.
pub fn plan_issue_cmd_options() -> CmdOptions {
    CmdOptions::new()
        .with_cwd(Path::new(env!("CARGO_MANIFEST_DIR")))
        .with_env(
            "PLAN_ISSUE_HOME",
            HERMETIC_STATE_DIR
                .path()
                .to_str()
                .expect("hermetic state dir path is utf-8"),
        )
}

#[allow(dead_code)]
pub fn run_plan_issue(args: &[&str]) -> CmdOutput {
    run_resolved("plan-issue", args, &plan_issue_cmd_options())
}

#[allow(dead_code)]
pub fn run_plan_issue_with_options(args: &[&str], options: CmdOptions) -> CmdOutput {
    run_resolved("plan-issue", args, &options)
}

#[allow(dead_code)]
pub fn run_plan_issue_local(args: &[&str]) -> CmdOutput {
    run_resolved("plan-issue-local", args, &plan_issue_cmd_options())
}

#[allow(dead_code)]
pub fn run_plan_issue_local_with_env(args: &[&str], env: &[(&str, &str)]) -> CmdOutput {
    run_resolved(
        "plan-issue-local",
        args,
        &plan_issue_cmd_options().with_envs(env),
    )
}

/// Reserved hook for tests that historically pre-seeded
/// `$PLAN_ISSUE_HOME/prompts/` before plan-issue copied init snapshots into
/// each runtime workspace. The init-snapshot copy was removed in the
/// 0.8 cut, so callers no longer need fixture content — the helper now
/// only verifies the workspace path is a directory.
#[allow(dead_code)]
pub fn ensure_state_dir(state_dir: &Path) {
    std::fs::create_dir_all(state_dir).expect("create plan-issue state-dir");
}
