//! Shared test helpers: locate the `forge-cli` binary, write executable stub
//! scripts under a tempdir, and run the binary with controlled env vars.
//!
//! Stub scripts replace `gh` / `glab` during tests so the parser / envelope
//! code under test never reaches the real internet.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Resolve the compiled `forge-cli` binary. Cargo exposes this via
/// `CARGO_BIN_EXE_forge-cli` to integration tests.
pub fn forge_cli_bin() -> PathBuf {
    let env_name = "CARGO_BIN_EXE_forge-cli";
    if let Ok(path) = std::env::var(env_name) {
        return PathBuf::from(path);
    }
    panic!("{env_name} not set by cargo; run via `cargo test -p nils-forge-cli`");
}

/// Build context for an integration call.
pub struct StubEnv {
    pub tempdir: TempDir,
    pub envs: Vec<(String, String)>,
}

impl StubEnv {
    pub fn new() -> Self {
        let tempdir = TempDir::new().expect("tempdir");
        Self {
            tempdir,
            envs: Vec::new(),
        }
    }

    /// Write an executable shell stub under the tempdir. The script body is
    /// written as-is; the file is `chmod 0o755`.
    pub fn write_stub(&self, name: &str, body: &str) -> PathBuf {
        let path = self.tempdir.path().join(name);
        fs::write(&path, body).expect("write stub");
        let mut perm = fs::metadata(&path).expect("metadata").permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&path, perm).expect("chmod");
        path
    }

    /// Set an env var on this stub environment.
    pub fn env(mut self, key: &str, value: impl Into<String>) -> Self {
        self.envs.push((key.to_string(), value.into()));
        self
    }

    /// Set FORGE_CLI_GH_BIN to point at a stub with the given body.
    pub fn gh_stub(self, body: &str) -> Self {
        let path = self.write_stub("gh", body);
        self.env("FORGE_CLI_GH_BIN", path.to_string_lossy())
    }

    /// Set FORGE_CLI_GLAB_BIN to point at a stub with the given body.
    pub fn glab_stub(self, body: &str) -> Self {
        let path = self.write_stub("glab", body);
        self.env("FORGE_CLI_GLAB_BIN", path.to_string_lossy())
    }
}

/// Captured output from a binary invocation.
#[derive(Debug)]
pub struct CmdOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run `forge-cli <args>` with the stub env applied.
pub fn run_forge_cli(stub: &StubEnv, args: &[&str]) -> CmdOutput {
    run_forge_cli_in(stub, args, None)
}

/// Run `forge-cli` from inside `cwd`. When `cwd` is `None`, the binary inherits
/// the parent test's working directory.
pub fn run_forge_cli_in(stub: &StubEnv, args: &[&str], cwd: Option<&Path>) -> CmdOutput {
    let mut cmd = Command::new(forge_cli_bin());
    cmd.args(args);
    for (k, v) in &stub.envs {
        cmd.env(k, v);
    }
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let output = cmd.output().expect("spawn forge-cli");
    CmdOutput {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Parse a single JSON envelope from stdout.
pub fn parse_envelope(stdout: &str) -> serde_json::Value {
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON envelope in stdout: {stdout:?}"));
    serde_json::from_str(line).unwrap_or_else(|e| panic!("invalid JSON: {e}; stdout={stdout:?}"))
}
