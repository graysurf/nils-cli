use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug)]
pub(super) struct Git {
    root: PathBuf,
}

impl Git {
    pub(super) fn discover(start: &Path) -> Result<Self, String> {
        let output = run_at(start, ["rev-parse", "--show-toplevel"])?;
        let root = required_stdout(output)?;
        let root = std::fs::canonicalize(root)
            .map_err(|error| format!("failed to canonicalize repository root: {error}"))?;
        Ok(Self { root })
    }

    pub(super) fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn run<I, S>(&self, args: I) -> Result<Output, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        run_at(&self.root, args)
    }

    pub(super) fn stdout<I, S>(&self, args: I) -> Result<String, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        required_stdout(self.run(args)?)
    }

    pub(super) fn stdout_allow_empty<I, S>(&self, args: I) -> Result<String, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Ok(String::from_utf8_lossy(&self.run(args)?.stdout)
            .trim()
            .to_string())
    }

    pub(super) fn config_optional(&self, key: &str) -> Result<Option<String>, String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["config", "--get", key])
            .env("GIT_PAGER", "cat")
            .env("PAGER", "cat")
            .output()
            .map_err(|error| format!("failed to launch git: {error}"))?;
        if output.status.success() {
            return Ok(Some(
                String::from_utf8_lossy(&output.stdout).trim().to_string(),
            ));
        }
        if output.status.code() == Some(1) {
            Ok(None)
        } else {
            Err(stderr_or_default(&output))
        }
    }
}

fn run_at<I, S>(root: &Path, args: I) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().collect();
    if args.first().is_some_and(|arg| {
        matches!(
            arg.as_ref().to_str(),
            Some("fetch" | "pull" | "push" | "ls-remote")
        )
    }) {
        return Err("network-capable Git operations are forbidden".to_string());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .output()
        .map_err(|error| format!("failed to launch git: {error}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(stderr_or_default(&output))
    }
}

fn required_stdout(output: Output) -> Result<String, String> {
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        Err("git command returned empty output".to_string())
    } else {
        Ok(value)
    }
}

fn stderr_or_default(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        "git command failed".to_string()
    } else {
        stderr
    }
}
