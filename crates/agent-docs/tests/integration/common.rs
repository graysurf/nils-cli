#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use nils_test_support::cmd;
use serde_json::Value;
use tempfile::TempDir;

/// A hermetic docs-home + project fixture built in a temp directory. The binary
/// is invoked with explicit `--docs-home` / `--project-path` so ambient
/// `AGENT_DOCS_HOME` / install symlinks never leak into the assertion.
pub struct TestEnv {
    _temp: TempDir,
    pub docs_home: PathBuf,
    pub project: PathBuf,
    home: PathBuf,
    xdg: PathBuf,
}

#[derive(Debug)]
pub struct CliOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CliOutput {
    pub fn success(&self) -> bool {
        self.code == 0
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|err| {
            panic!(
                "expected JSON stdout (err={err}); code={} stdout=\n{}\nstderr=\n{}",
                self.code, self.stdout, self.stderr
            )
        })
    }
}

impl TestEnv {
    pub fn new() -> Self {
        let temp = TempDir::new().expect("create temp dir");
        let docs_home = temp.path().join("docs-home");
        let project = temp.path().join("project");
        let home = temp.path().join("home");
        let xdg = temp.path().join("xdg");
        fs::create_dir_all(&docs_home).expect("create docs-home");
        fs::create_dir_all(&project).expect("create project");
        fs::create_dir_all(&home).expect("create home");
        fs::create_dir_all(&xdg).expect("create xdg");
        Self {
            _temp: temp,
            docs_home,
            project,
            home,
            xdg,
        }
    }

    pub fn write_home_catalog(&self, toml: &str) -> &Self {
        write(&self.docs_home.join("AGENT_DOCS.toml"), toml);
        self
    }

    pub fn write_project_catalog(&self, toml: &str) -> &Self {
        write(&self.project.join("AGENT_DOCS.toml"), toml);
        self
    }

    pub fn write_home_doc(&self, rel: &str, body: &str) -> &Self {
        write(&self.docs_home.join(rel), body);
        self
    }

    pub fn write_project_doc(&self, rel: &str, body: &str) -> &Self {
        write(&self.project.join(rel), body);
        self
    }

    pub fn project_path(&self, rel: &str) -> PathBuf {
        self.project.join(rel)
    }

    pub fn home_path(&self, rel: &str) -> PathBuf {
        self.docs_home.join(rel)
    }

    /// Run `agent-docs <args>` with explicit `--docs-home` / `--project-path`.
    pub fn run(&self, args: &[&str]) -> CliOutput {
        self.run_for_project(&self.project, args)
    }

    pub fn run_for_project(&self, project: &Path, args: &[&str]) -> CliOutput {
        let docs_home = self.docs_home.to_str().expect("utf-8 docs_home");
        let project_arg = project.to_str().expect("utf-8 project");
        let mut full: Vec<&str> = vec!["--docs-home", docs_home, "--project-path", project_arg];
        full.extend_from_slice(args);
        let options = cmd::CmdOptions::default()
            .with_cwd(project)
            .with_env("HOME", self.home.to_str().expect("utf-8 home"))
            .with_env(
                "XDG_CONFIG_HOME",
                self.xdg.to_str().expect("utf-8 xdg config home"),
            )
            .with_env_remove("AGENT_DOCS_HOME")
            .with_env_remove("PROJECT_PATH");
        run_cli(&full, &options)
    }
}

impl Default for TestEnv {
    fn default() -> Self {
        Self::new()
    }
}

pub fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dir");
    }
    fs::write(path, body).expect("write fixture file");
}

/// Low-level invocation with full control over args / cwd / env (no auto flags).
pub fn run_cli(args: &[&str], options: &cmd::CmdOptions) -> CliOutput {
    let output = cmd::run_resolved("agent-docs", args, options);
    CliOutput {
        code: output.code,
        stdout: output.stdout_text(),
        stderr: output.stderr_text(),
    }
}

/// Low-level invocation with platform-native args for non-UTF path coverage.
pub fn run_cli_os(args: &[&OsStr], options: &cmd::CmdOptions) -> CliOutput {
    let output = cmd::run_resolved_os("agent-docs", args, options);
    CliOutput {
        code: output.code,
        stdout: output.stdout_text(),
        stderr: output.stderr_text(),
    }
}
