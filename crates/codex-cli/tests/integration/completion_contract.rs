use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use std::sync::OnceLock;

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn run(args: &[&str]) -> CmdOutput {
    let bin = codex_cli_bin();
    cmd::run(&bin, args, &[], None)
}

fn completion_zsh() -> &'static str {
    static OUTPUT: OnceLock<String> = OnceLock::new();
    OUTPUT
        .get_or_init(|| {
            let output = run(&["completion", "zsh"]);
            assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
            output.stdout_text()
        })
        .as_str()
}

fn assert_contains_all(haystack: &str, needles: &[&str]) {
    for needle in needles {
        assert!(
            haystack.contains(needle),
            "missing completion contract token: {needle}"
        );
    }
}

#[test]
fn completion_contract_includes_top_level_command_families() {
    let script = completion_zsh();
    assert_contains_all(
        script,
        &[
            "'agent:Agent command group' \\",
            "'auth:Authentication command group' \\",
            "'diag:Diagnostics command group' \\",
            "'config:Configuration command group' \\",
            "'prompt-segment:Prompt-segment command group' \\",
        ],
    );
}

#[test]
fn completion_contract_is_context_aware_across_command_families() {
    let script = completion_zsh();
    assert_contains_all(
        script,
        &[
            "curcontext=\"${curcontext%:*:*}:codex-cli-agent-command-$line[1]:\"",
            "curcontext=\"${curcontext%:*:*}:codex-cli-auth-command-$line[1]:\"",
            "curcontext=\"${curcontext%:*:*}:codex-cli-diag-command-$line[1]:\"",
            "curcontext=\"${curcontext%:*:*}:codex-cli-config-command-$line[1]:\"",
            "'--api-key[Use API key login flow]' \\",
            "'--cached[Cached mode (no network)]' \\",
            "':key:_default' \\",
            "':value:_default' \\",
            "'--time-format=[Reset time format (local time)]:TIME_FORMAT:_default' \\",
            "'*::target:_default' \\",
            "'*::secret:_default' \\",
        ],
    );
}

#[test]
fn completion_contract_format_values_include_text_and_json() {
    let script = completion_zsh();
    let format_candidates = script.match_indices(":format:(text json)'").count();
    assert!(
        format_candidates >= 2,
        "expected at least 2 format candidate entries, got {format_candidates}"
    );
    assert_contains_all(
        script,
        &[
            "'--format=[Output format (\\`text\\` or \\`json\\`)]:format:(text json)' \\",
            "'(--format)--json[Output machine-readable JSON]' \\",
            "'(--format)--json[Output raw JSON]' \\",
        ],
    );
}
