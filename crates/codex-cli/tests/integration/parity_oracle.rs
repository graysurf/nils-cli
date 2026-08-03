use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

/// `gemini-cli` belongs to another package, so a package-scoped run has no reason
/// to have built it, and an artifact left behind by an earlier build would
/// compare this CLI against a different release. Either way the answer says
/// nothing about parity, so skip with a reason instead of asserting.
/// See `sympoies/nils-cli#1413`.
fn gemini_cli_bin() -> Option<PathBuf> {
    bin::sibling_or_skip("gemini-cli", "nils-gemini-cli")
}

fn run_codex(args: &[&str]) -> CmdOutput {
    let bin = codex_cli_bin();
    cmd::run(&bin, args, &[], None)
}

fn run_gemini(bin: &Path, args: &[&str]) -> CmdOutput {
    cmd::run(bin, args, &[], None)
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
    let Some(gemini_bin) = gemini_cli_bin() else {
        return;
    };
    let codex = run_codex(&["--help"]);
    let gemini = run_gemini(&gemini_bin, &["--help"]);
    assert_eq!(codex.code, 0, "stderr={}", codex.stderr_text());
    assert_eq!(gemini.code, 0, "stderr={}", gemini.stderr_text());

    let codex_commands = extract_commands(&codex.stdout_text());
    let gemini_commands = extract_commands(&gemini.stdout_text());
    assert_eq!(codex_commands, gemini_commands);
}

#[test]
fn parity_oracle_format_flag_visibility_matches_gemini_for_auth_and_diag_help() {
    let Some(gemini_bin) = gemini_cli_bin() else {
        return;
    };
    let codex_auth = run_codex(&["auth", "current", "--help"]);
    let gemini_auth = run_gemini(&gemini_bin, &["auth", "current", "--help"]);
    assert_eq!(codex_auth.code, 0);
    assert_eq!(gemini_auth.code, 0);
    let codex_auth_text = codex_auth.stdout_text();
    let gemini_auth_text = gemini_auth.stdout_text();
    assert!(codex_auth_text.contains("--format"));
    assert!(gemini_auth_text.contains("--format"));
    assert!(!codex_auth_text.contains("--json"));
    assert!(!gemini_auth_text.contains("--json"));

    let codex_diag = run_codex(&["diag", "rate-limits", "--help"]);
    let gemini_diag = run_gemini(&gemini_bin, &["diag", "rate-limits", "--help"]);
    assert_eq!(codex_diag.code, 0);
    assert_eq!(gemini_diag.code, 0);
    let codex_diag_text = codex_diag.stdout_text();
    let gemini_diag_text = gemini_diag.stdout_text();
    for token in ["--format", "--cached", "--async"] {
        assert!(codex_diag_text.contains(token));
        assert!(gemini_diag_text.contains(token));
    }
    assert!(!codex_diag_text.contains("--json"));
    assert!(!gemini_diag_text.contains("--json"));
}

#[test]
fn parity_oracle_auth_json_schema_ids_are_provider_specific() {
    let Some(gemini_bin) = gemini_cli_bin() else {
        return;
    };
    let codex = run_codex(&["auth", "current", "--json"]);
    let gemini = run_gemini(&gemini_bin, &["auth", "current", "--json"]);

    let codex_json: Value = serde_json::from_str(&codex.stdout_text()).expect("codex auth json");
    let gemini_json: Value = serde_json::from_str(&gemini.stdout_text()).expect("gemini auth json");

    assert_eq!(codex_json["command"], "auth current");
    assert_eq!(gemini_json["command"], "auth current");
    assert_eq!(codex_json["schema_version"], "codex-cli.auth.v1");
    assert_eq!(gemini_json["schema_version"], "gemini-cli.auth.v1");
}
