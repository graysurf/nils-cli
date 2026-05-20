//! Sprint 8 Task 8.1 — completion script generation and sync test.
//!
//! Asserts:
//! 1. `forge-cli completion bash|zsh` succeeds and emits a non-empty script
//!    whose preamble matches the shell-specific marker (`_forge-cli` for
//!    bash, `#compdef forge-cli` for zsh).
//! 2. The checked-in completion files at `completions/{bash,zsh}/` stay in
//!    byte-equality with the live binary output, so a future flag addition
//!    that forgets to regenerate the snapshots fails CI rather than landing
//!    silently.

use std::path::PathBuf;
use std::process::Command;

use pretty_assertions::assert_eq;

use super::support::forge_cli_bin;

fn run_completion(shell: &str) -> (i32, String, String) {
    let output = Command::new(forge_cli_bin())
        .args(["completion", shell])
        .output()
        .expect("spawn forge-cli");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn workspace_root() -> PathBuf {
    // tests live under crates/forge-cli/tests; the workspace root is two
    // ancestors above the crate dir. Walk up until we find `completions/`.
    std::env::current_dir()
        .expect("cwd")
        .ancestors()
        .find(|p| p.join("completions").is_dir() && p.join("Cargo.toml").is_file())
        .expect("locate workspace root")
        .to_path_buf()
}

#[test]
fn completion_bash_emits_non_empty_script_with_forge_cli_marker() {
    let (code, stdout, stderr) = run_completion("bash");
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        !stdout.is_empty(),
        "bash completion script must be non-empty"
    );
    assert!(
        stdout.contains("_forge-cli"),
        "bash completion must define _forge-cli, got first 200 bytes: {}",
        &stdout[..stdout.len().min(200)]
    );
}

#[test]
fn completion_zsh_emits_non_empty_script_with_compdef_marker() {
    let (code, stdout, stderr) = run_completion("zsh");
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        !stdout.is_empty(),
        "zsh completion script must be non-empty"
    );
    assert!(
        stdout.contains("#compdef forge-cli"),
        "zsh completion must start with #compdef forge-cli, got first 200 bytes: {}",
        &stdout[..stdout.len().min(200)]
    );
}

#[test]
fn checked_in_bash_completion_matches_live_binary_output() {
    let (code, stdout, _) = run_completion("bash");
    assert_eq!(code, 0);
    let path = workspace_root().join("completions/bash/forge-cli");
    let on_disk =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert_eq!(
        on_disk.trim_end(),
        stdout.trim_end(),
        "completions/bash/forge-cli is out of sync with the binary; regenerate via\n  forge-cli completion bash > completions/bash/forge-cli"
    );
}

#[test]
fn checked_in_zsh_completion_matches_live_binary_output() {
    let (code, stdout, _) = run_completion("zsh");
    assert_eq!(code, 0);
    let path = workspace_root().join("completions/zsh/_forge-cli");
    let on_disk =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert_eq!(
        on_disk.trim_end(),
        stdout.trim_end(),
        "completions/zsh/_forge-cli is out of sync with the binary; regenerate via\n  forge-cli completion zsh > completions/zsh/_forge-cli"
    );
}
