use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use pretty_assertions::assert_eq;
use std::path::PathBuf;

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn run(args: &[&str]) -> CmdOutput {
    let bin = codex_cli_bin();
    cmd::run(&bin, args, &[], None)
}

fn stderr_string(output: &CmdOutput) -> String {
    output.stderr_text()
}

fn assert_exit_code(output: &CmdOutput, expected: i32) {
    assert_eq!(output.code, expected);
}

#[test]
fn dispatch_removed_redirect_commands_use_clap_parse_errors() {
    for command in [
        "list",
        "prompt",
        "advice",
        "knowledge",
        "commit",
        "auto-refresh",
        "rate-limits",
        "provider",
        "debug",
        "workflow",
        "automation",
    ] {
        let output = run(&[command]);
        assert_exit_code(&output, 64);
        let stderr = stderr_string(&output);
        assert!(
            stderr.contains("unrecognized subcommand"),
            "missing clap parse error for {command}: {stderr}"
        );
        assert!(
            stderr.contains(command),
            "stderr should include command token {command}: {stderr}"
        );
    }
}
