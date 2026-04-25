use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::path::PathBuf;

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn gemini_cli_bin() -> PathBuf {
    bin::resolve("gemini-cli")
}

fn run_codex(args: &[&str]) -> CmdOutput {
    let bin = codex_cli_bin();
    cmd::run(&bin, args, &[], None)
}

fn run_gemini(args: &[&str]) -> CmdOutput {
    let bin = gemini_cli_bin();
    cmd::run(&bin, args, &[], None)
}

fn extract_commands(help_text: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut in_commands = false;

    for line in help_text.lines() {
        if line.trim() == "Commands:" {
            in_commands = true;
            continue;
        }
        if !in_commands {
            continue;
        }
        if line.trim().is_empty() {
            break;
        }

        let trimmed = line.trim_start();
        if trimmed.starts_with('-') {
            continue;
        }

        if let Some(command) = trimmed.split_whitespace().next() {
            commands.push(command.to_string());
        }
    }

    commands
}

#[test]
fn parity_oracle_topology_matches_gemini() {
    let codex = run_codex(&["--help"]);
    let gemini = run_gemini(&["--help"]);
    assert_eq!(codex.code, 0, "stderr={}", codex.stderr_text());
    assert_eq!(gemini.code, 0, "stderr={}", gemini.stderr_text());

    let codex_commands = extract_commands(&codex.stdout_text());
    let gemini_commands = extract_commands(&gemini.stdout_text());
    assert_eq!(codex_commands, gemini_commands);
}

#[test]
fn parity_oracle_json_flags_match_gemini_for_auth_and_diag_help() {
    let codex_auth = run_codex(&["auth", "current", "--help"]);
    let gemini_auth = run_gemini(&["auth", "current", "--help"]);
    assert_eq!(codex_auth.code, 0);
    assert_eq!(gemini_auth.code, 0);
    let codex_auth_text = codex_auth.stdout_text();
    let gemini_auth_text = gemini_auth.stdout_text();
    for token in ["--format", "--json"] {
        assert!(codex_auth_text.contains(token));
        assert!(gemini_auth_text.contains(token));
    }

    let codex_diag = run_codex(&["diag", "rate-limits", "--help"]);
    let gemini_diag = run_gemini(&["diag", "rate-limits", "--help"]);
    assert_eq!(codex_diag.code, 0);
    assert_eq!(gemini_diag.code, 0);
    let codex_diag_text = codex_diag.stdout_text();
    let gemini_diag_text = gemini_diag.stdout_text();
    for token in ["--format", "--json", "--cached", "--async"] {
        assert!(codex_diag_text.contains(token));
        assert!(gemini_diag_text.contains(token));
    }
}

#[test]
fn parity_oracle_auth_json_schema_ids_are_provider_specific() {
    let codex = run_codex(&["auth", "current", "--json"]);
    let gemini = run_gemini(&["auth", "current", "--json"]);

    let codex_json: Value = serde_json::from_str(&codex.stdout_text()).expect("codex auth json");
    let gemini_json: Value = serde_json::from_str(&gemini.stdout_text()).expect("gemini auth json");

    assert_eq!(codex_json["command"], "auth current");
    assert_eq!(gemini_json["command"], "auth current");
    assert_eq!(codex_json["schema_version"], "codex-cli.auth.v1");
    assert_eq!(gemini_json["schema_version"], "gemini-cli.auth.v1");
}
