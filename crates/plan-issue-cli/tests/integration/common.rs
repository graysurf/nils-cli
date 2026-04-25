use std::fs;
use std::path::Path;

use nils_test_support::cmd::{CmdOptions, run_resolved};

pub struct CmdOut {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Build a deterministic baseline for plan-issue integration tests.
/// Tests can compose env/path overrides via `CmdOptions` instead of ad-hoc
/// shell-style setup in each test body.
pub fn plan_issue_cmd_options() -> CmdOptions {
    CmdOptions::new().with_cwd(Path::new(env!("CARGO_MANIFEST_DIR")))
}

fn run_bin_with_options(bin_name: &str, args: &[&str], options: CmdOptions) -> CmdOut {
    let output = run_resolved(bin_name, args, &options);

    CmdOut {
        code: output.code,
        stdout: output.stdout_text(),
        stderr: output.stderr_text(),
    }
}

#[allow(dead_code)]
pub fn run_plan_issue(args: &[&str]) -> CmdOut {
    run_bin_with_options("plan-issue", args, plan_issue_cmd_options())
}

#[allow(dead_code)]
pub fn run_plan_issue_with_options(args: &[&str], options: CmdOptions) -> CmdOut {
    run_bin_with_options("plan-issue", args, options)
}

#[allow(dead_code)]
pub fn run_plan_issue_local(args: &[&str]) -> CmdOut {
    run_bin_with_options("plan-issue-local", args, plan_issue_cmd_options())
}

#[allow(dead_code)]
pub fn run_plan_issue_local_with_env(args: &[&str], env: &[(&str, &str)]) -> CmdOut {
    run_bin_with_options(
        "plan-issue-local",
        args,
        plan_issue_cmd_options().with_envs(env),
    )
}

/// Materialize the agent-kit prompts the canonical runtime layout requires.
///
/// `start-plan` and `start-sprint` copy these files into the plan-issue
/// runtime tree, so every test that exercises those commands must pre-seed
/// them under `$AGENT_HOME/prompts/`.
#[allow(dead_code)]
pub fn seed_agent_home_prompts(agent_home: &Path) {
    let prompts = agent_home.join("prompts");
    fs::create_dir_all(&prompts).expect("create prompts dir");
    fs::write(
        prompts.join("plan-issue-delivery-main-agent-init.md"),
        "# Main Agent Init (test fixture)\n",
    )
    .expect("write main-agent init fixture");
    fs::write(
        prompts.join("plan-issue-delivery-subagent-init.md"),
        "# Subagent Init (test fixture)\n",
    )
    .expect("write subagent init fixture");
}
