#![allow(dead_code)]

use nils_test_support::{StubBinDir, cmd};
use std::path::Path;

pub struct CmdOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_fzf_cli(
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    stdin: Option<&str>,
) -> CmdOutput {
    let mut options = cmd::options_in_dir_with_envs(dir, envs);
    if let Some(input) = stdin {
        options = options.with_stdin_str(input);
    }
    let output = cmd::run_resolved("fzf-cli", args, &options);
    CmdOutput {
        code: output.code,
        stdout: output.stdout_text(),
        stderr: output.stderr_text(),
    }
}

pub fn run_fzf_cli_with_stub_path(
    dir: &Path,
    stub_path: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    stdin: Option<&str>,
) -> CmdOutput {
    let mut options = cmd::options_in_dir_with_envs(dir, envs).with_path_prepend(stub_path);
    if let Some(input) = stdin {
        options = options.with_stdin_str(input);
    }
    let output = cmd::run_resolved("fzf-cli", args, &options);
    CmdOutput {
        code: output.code,
        stdout: output.stdout_text(),
        stderr: output.stderr_text(),
    }
}

pub fn run_fzf_cli_with_stub_only_path(
    dir: &Path,
    stub_path: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    stdin: Option<&str>,
) -> CmdOutput {
    let path = stub_path.to_string_lossy().to_string();
    let mut options = cmd::options_in_dir_with_envs(dir, envs).with_env("PATH", &path);
    if let Some(input) = stdin {
        options = options.with_stdin_str(input);
    }
    let output = cmd::run_resolved("fzf-cli", args, &options);
    CmdOutput {
        code: output.code,
        stdout: output.stdout_text(),
        stderr: output.stderr_text(),
    }
}

pub fn make_stub_dir() -> StubBinDir {
    StubBinDir::new()
}

pub fn fzf_stub_script() -> &'static str {
    nils_test_support::stubs::fzf_stub_script()
}

pub fn write_exe(dir: &Path, name: &str, content: &str) {
    nils_test_support::write_exe(dir, name, content);
}
