use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
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

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(output.code, code, "stderr: {}", output.stderr_text());
}

fn contains_token(space_delimited: &str, token: &str) -> bool {
    space_delimited
        .split_whitespace()
        .any(|candidate| candidate == token)
}

fn bash_case_opts(script: &str, label: &str) -> String {
    let case_marker = format!("{label})");
    let mut in_case = false;

    for line in script.lines() {
        let trimmed = line.trim();
        if trimmed == case_marker {
            in_case = true;
            continue;
        }

        if !in_case {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("opts=\"") {
            let end = rest
                .find('"')
                .unwrap_or_else(|| panic!("missing closing quote for opts in case {label}"));
            return rest[..end].to_string();
        }

        if trimmed.ends_with(")") {
            break;
        }
    }

    panic!("missing bash opts case: {label}");
}

#[test]
fn completion_export_is_stable_across_repeated_runs() {
    let zsh_first = run(&["completion", "zsh"]);
    assert_exit(&zsh_first, 0);
    let zsh_second = run(&["completion", "zsh"]);
    assert_exit(&zsh_second, 0);
    assert_eq!(zsh_first.stdout, zsh_second.stdout);
    assert_eq!(zsh_first.stderr, zsh_second.stderr);

    let bash_first = run(&["completion", "bash"]);
    assert_exit(&bash_first, 0);
    let bash_second = run(&["completion", "bash"]);
    assert_exit(&bash_second, 0);
    assert_eq!(bash_first.stdout, bash_second.stdout);
    assert_eq!(bash_first.stderr, bash_second.stderr);
}

#[test]
fn completion_bash_candidates_remain_context_aware() {
    let output = run(&["completion", "bash"]);
    assert_exit(&output, 0);
    let script = stdout(&output);

    let root_opts = bash_case_opts(&script, "gemini__cli");
    for token in [
        "agent",
        "auth",
        "diag",
        "config",
        "prompt-segment",
        "completion",
        "help",
    ] {
        assert!(
            contains_token(&root_opts, token),
            "missing root token: {token}"
        );
    }
    for token in [
        "login",
        "rate-limits",
        "--api-key",
        "--cached",
        "provider",
        "workflow",
    ] {
        assert!(
            !contains_token(&root_opts, token),
            "unexpected root token: {token}"
        );
    }

    let completion_opts = bash_case_opts(&script, "gemini__cli__completion");
    assert_eq!(completion_opts, "-h --help bash zsh");
    for token in ["agent", "auth", "--api-key", "--cached"] {
        assert!(
            !contains_token(&completion_opts, token),
            "completion command should not include token: {token}"
        );
    }

    let auth_login_opts = bash_case_opts(&script, "gemini__cli__auth__login");
    for token in ["--format", "--json", "--api-key", "--device-code"] {
        assert!(
            contains_token(&auth_login_opts, token),
            "missing auth login token: {token}"
        );
    }
    for token in ["--cached", "--jobs", "--all"] {
        assert!(
            !contains_token(&auth_login_opts, token),
            "unexpected auth login token: {token}"
        );
    }

    let diag_rate_limits_opts = bash_case_opts(&script, "gemini__cli__diag__rate__limits");
    for token in ["--cached", "--async", "--jobs", "--format", "--json"] {
        assert!(
            contains_token(&diag_rate_limits_opts, token),
            "missing diag token: {token}"
        );
    }
    for token in ["--api-key", "--device-code"] {
        assert!(
            !contains_token(&diag_rate_limits_opts, token),
            "unexpected diag token: {token}"
        );
    }
}
