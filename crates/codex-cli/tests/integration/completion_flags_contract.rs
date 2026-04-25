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

fn stdout(output: &CmdOutput) -> String {
    output.stdout_text()
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(output.code, code, "stderr: {}", output.stderr_text());
}

fn parse_help_flag_tokens(help_text: &str) -> Vec<String> {
    let mut in_options = false;
    let mut flags: Vec<String> = Vec::new();

    for raw_line in help_text.lines() {
        let line = raw_line.trim();
        if line == "Options:" {
            in_options = true;
            continue;
        }
        if !in_options {
            continue;
        }
        if line.is_empty() {
            break;
        }

        let spec_end = line.find("  ").unwrap_or(line.len());
        let spec = &line[..spec_end];

        for token in spec
            .split(|ch: char| ch == ',' || ch.is_whitespace())
            .filter(|piece| !piece.is_empty())
        {
            if !token.starts_with('-') {
                continue;
            }
            if token == "-h" || token == "--help" {
                continue;
            }
            if !flags.iter().any(|seen| seen == token) {
                flags.push(token.to_string());
            }
        }
    }

    flags
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

fn bash_case_label(path: &[&str]) -> String {
    let mut parts = vec!["codex__cli".to_string()];
    parts.extend(path.iter().map(|segment| segment.replace('-', "__")));
    parts.join("__")
}

fn zsh_context_marker(parents: &[&str]) -> String {
    if parents.is_empty() {
        "curcontext=\"${curcontext%:*:*}:codex-cli-command-$line[1]:\"".to_string()
    } else {
        format!(
            "curcontext=\"${{curcontext%:*:*}}:codex-cli-{}-command-$line[1]:\"",
            parents.join("-")
        )
    }
}

fn zsh_leaf_arguments_block(script: &str, path: &[&str]) -> String {
    let (leaf, parents) = path
        .split_last()
        .unwrap_or_else(|| panic!("expected non-empty command path"));
    let marker = zsh_context_marker(parents);
    let marker_idx = script
        .find(&marker)
        .unwrap_or_else(|| panic!("missing zsh context marker: {marker}"));
    let from_marker = &script[marker_idx..];

    let leaf_marker = format!("({leaf})");
    let leaf_idx = from_marker
        .find(&leaf_marker)
        .unwrap_or_else(|| panic!("missing leaf marker {leaf_marker} after marker {marker}"));
    let from_leaf = &from_marker[leaf_idx..];

    let args_marker = "_arguments \"${_arguments_options[@]}\" : \\";
    let args_idx = from_leaf
        .find(args_marker)
        .unwrap_or_else(|| panic!("missing _arguments block for path {}", path.join(" ")));
    let from_args = &from_leaf[args_idx..];

    let end_idx = from_args
        .find("&& ret=0")
        .unwrap_or_else(|| panic!("missing _arguments terminator for path {}", path.join(" ")));

    from_args[..end_idx].to_string()
}

fn contains_option_token(haystack: &str, token: &str) -> bool {
    let mut search_start = 0;
    while let Some(relative_idx) = haystack[search_start..].find(token) {
        let start = search_start + relative_idx;
        let end = start + token.len();
        let before = haystack[..start].chars().next_back();
        let after = haystack[end..].chars().next();
        let before_ok = before.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '-');
        let after_ok = after.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '-');
        if before_ok && after_ok {
            return true;
        }
        search_start = end;
    }
    false
}

fn leaf_paths() -> Vec<Vec<&'static str>> {
    vec![
        vec!["agent", "prompt"],
        vec!["agent", "advice"],
        vec!["agent", "knowledge"],
        vec!["agent", "commit"],
        vec!["auth", "login"],
        vec!["auth", "use"],
        vec!["auth", "save"],
        vec!["auth", "remove"],
        vec!["auth", "refresh"],
        vec!["auth", "auto-refresh"],
        vec!["auth", "current"],
        vec!["auth", "sync"],
        vec!["diag", "rate-limits"],
        vec!["config", "show"],
        vec!["config", "set"],
        vec!["prompt-segment"],
        vec!["completion"],
    ]
}

#[test]
fn completion_flags_contract_leaf_help_matches_bash_and_zsh_candidates() {
    let bash_output = run(&["completion", "bash"]);
    assert_exit(&bash_output, 0);
    let bash_script = stdout(&bash_output);

    let zsh_output = run(&["completion", "zsh"]);
    assert_exit(&zsh_output, 0);
    let zsh_script = stdout(&zsh_output);

    for path in leaf_paths() {
        let mut help_args = path.clone();
        help_args.push("--help");
        let help_output = run(&help_args);
        assert_exit(&help_output, 0);
        let help_text = stdout(&help_output);
        let expected_flags = parse_help_flag_tokens(&help_text);
        if expected_flags.is_empty() {
            continue;
        }

        let label = bash_case_label(&path);
        let bash_opts = bash_case_opts(&bash_script, &label);
        let zsh_block = zsh_leaf_arguments_block(&zsh_script, &path);

        for flag in expected_flags {
            assert!(
                contains_option_token(&bash_opts, &flag),
                "missing bash completion flag for `{}`: {}",
                path.join(" "),
                flag
            );
            assert!(
                contains_option_token(&zsh_block, &flag),
                "missing zsh completion flag for `{}`: {}\nblock:\n{}",
                path.join(" "),
                flag,
                zsh_block
            );
        }
    }
}
