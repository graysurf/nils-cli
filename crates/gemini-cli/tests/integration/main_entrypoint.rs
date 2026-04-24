use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use pretty_assertions::assert_eq;
use std::path::PathBuf;

fn gemini_cli_bin() -> PathBuf {
    bin::resolve("gemini-cli")
}

fn run(args: &[&str]) -> CmdOutput {
    let bin = gemini_cli_bin();
    cmd::run(&bin, args, &[], None)
}

fn stdout(output: &CmdOutput) -> String {
    output.stdout_text()
}

fn stderr(output: &CmdOutput) -> String {
    output.stderr_text()
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(output.code, code);
}

#[test]
fn main_no_args_prints_help_and_exits_zero() {
    let output = run(&[]);
    assert_exit(&output, 0);
    assert!(stdout(&output).contains("gemini-cli"));
}

#[test]
fn main_help_subcommand_exits_zero() {
    let output = run(&["help"]);
    assert_exit(&output, 0);
    assert!(stdout(&output).contains("gemini-cli"));
}

#[test]
fn main_agent_and_config_without_subcommand_print_help() {
    let output = run(&["agent"]);
    assert_exit(&output, 0);
    assert!(stdout(&output).contains("Agent command group"));

    let output = run(&["config"]);
    assert_exit(&output, 0);
    assert!(stdout(&output).contains("Configuration command group"));
}

#[test]
fn main_agent_prompt_is_gated_and_config_show_exits_zero() {
    let options = CmdOptions::default().with_env("GEMINI_ALLOW_DANGEROUS_ENABLED", "false");
    let bin = gemini_cli_bin();
    let output = cmd::run_with(&bin, &["agent", "prompt", "hello"], &options);
    assert_exit(&output, 1);
    assert!(stderr(&output).contains("disabled (set GEMINI_ALLOW_DANGEROUS_ENABLED=true)"));

    let output = run(&["config", "show"]);
    assert_exit(&output, 0);
}

#[test]
fn main_unknown_command_exits_64() {
    let output = run(&["not-a-real-command"]);
    assert_exit(&output, 64);
    assert!(!stderr(&output).trim().is_empty());
}

#[test]
fn main_removed_provider_neutral_groups_use_clap_parse_errors() {
    for group in ["provider", "debug", "workflow", "automation"] {
        let output = run(&[group]);
        assert_exit(&output, 64);
        let err = stderr(&output);
        assert!(
            err.contains("unrecognized subcommand"),
            "stderr should include clap parse error: {err}"
        );
        assert!(
            err.contains(group),
            "stderr should include rejected command token: {err}"
        );
    }
}

#[test]
fn main_help_excludes_provider_neutral_groups() {
    let output = run(&["--help"]);
    assert_exit(&output, 0);
    let help = stdout(&output);
    for group in ["provider", "debug", "workflow", "automation"] {
        assert!(
            !help.contains(group),
            "unexpected provider-neutral group in help: {group}\n{help}"
        );
    }
}

#[test]
fn main_help_includes_json_output_modes_for_diag_and_auth() {
    let diag_help = run(&["diag", "rate-limits", "--help"]);
    assert_exit(&diag_help, 0);
    let diag_text = stdout(&diag_help);
    assert!(diag_text.contains("--json"));
    assert!(diag_text.contains("--format"));

    let auth_help = run(&["auth", "current", "--help"]);
    assert_exit(&auth_help, 0);
    let auth_text = stdout(&auth_help);
    assert!(auth_text.contains("--json"));
    assert!(auth_text.contains("--format"));
}

#[test]
fn main_help_includes_completion_export_entrypoint() {
    let output = run(&["--help"]);
    assert_exit(&output, 0);
    let help = stdout(&output);
    assert!(help.contains("completion"));
    assert!(help.contains("Export shell completion script"));
}

#[test]
fn main_completion_exports_bash_and_zsh_scripts() {
    let zsh = run(&["completion", "zsh"]);
    assert_exit(&zsh, 0);
    let zsh_text = stdout(&zsh);
    assert!(zsh_text.contains("#compdef gemini-cli"));
    assert!(zsh_text.contains("completion:Export shell completion script"));
    assert!(zsh_text.contains(":shell -- Shell to generate completion script for:(bash zsh)"));

    let bash = run(&["completion", "bash"]);
    assert_exit(&bash, 0);
    let bash_text = stdout(&bash);
    assert!(bash_text.contains("_gemini-cli()"));
    assert!(bash_text.contains("complete -F _gemini-cli"));
    assert!(bash_text.contains("opts=\"-h --help bash zsh\""));
}

#[test]
fn main_completion_rejects_unknown_shell_with_usage_error() {
    let output = run(&["completion", "fish"]);
    assert_exit(&output, 64);
    let err = stderr(&output);
    assert!(err.contains("invalid value"));
    assert!(err.contains("bash"));
    assert!(err.contains("zsh"));
}
