use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Output captured from a command invocation.
#[derive(Debug)]
pub struct CmdOutput {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CmdOutput {
    pub fn success(&self) -> bool {
        self.code == 0
    }

    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).to_string()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).to_string()
    }

    /// Parse stdout as JSON, panicking when it is not valid JSON.
    ///
    /// Replaces the per-crate `fn json_stdout(&CmdOutput)` test helpers; the
    /// panic message is kept identical to theirs.
    pub fn stdout_json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout_text()).expect("stdout should be json")
    }

    /// Parse stderr as JSON, panicking when it is not valid JSON. Used for
    /// CLIs whose error envelopes are written to stderr.
    pub fn stderr_json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stderr_text()).expect("stderr should be json")
    }

    /// Convert to `std::process::Output` for integration with assertion APIs
    /// that expect process output semantics.
    pub fn into_output(self) -> Output {
        Output {
            status: exit_status_from_code(self.code),
            stdout: self.stdout,
            stderr: self.stderr,
        }
    }
}

#[cfg(unix)]
fn exit_status_from_code(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    let raw = if code >= 0 { code << 8 } else { 1 << 8 };
    std::process::ExitStatus::from_raw(raw)
}

#[cfg(windows)]
fn exit_status_from_code(code: i32) -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    let raw = if code >= 0 { code as u32 } else { 1 };
    std::process::ExitStatus::from_raw(raw)
}

#[derive(Debug, Clone)]
pub struct CmdOptions {
    pub cwd: Option<PathBuf>,
    pub envs: Vec<(String, String)>,
    pub env_remove: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    pub stdin_null: bool,
}

impl Default for CmdOptions {
    fn default() -> Self {
        Self {
            cwd: None,
            envs: Vec::new(),
            env_remove: Vec::new(),
            stdin: None,
            stdin_null: true,
        }
    }
}

impl CmdOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_envs(mut self, envs: &[(&str, &str)]) -> Self {
        for (key, value) in envs {
            self = self.with_env(key, value);
        }
        self
    }

    pub fn with_env_remove_prefix(mut self, prefix: &str) -> Self {
        for (key, _) in std::env::vars_os() {
            let key = key.to_string_lossy();
            if key.starts_with(prefix) {
                self = self.with_env_remove(&key);
            }
        }
        self
    }

    pub fn with_env_remove_many(mut self, keys: &[&str]) -> Self {
        for key in keys {
            self = self.with_env_remove(key);
        }
        self
    }

    pub fn with_path_prepend(self, dir: &Path) -> Self {
        let base = self
            .envs
            .iter()
            .rev()
            .find(|(key, _)| key == "PATH")
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var_os("PATH").map(|value| value.to_string_lossy().to_string()))
            .unwrap_or_default();

        let mut paths: Vec<PathBuf> = std::env::split_paths(std::ffi::OsStr::new(&base)).collect();
        paths.insert(0, dir.to_path_buf());
        let joined = std::env::join_paths(paths).expect("join paths");
        let joined = joined.to_string_lossy().to_string();
        self.with_env("PATH", &joined)
    }

    pub fn with_cwd(mut self, dir: &Path) -> Self {
        self.cwd = Some(dir.to_path_buf());
        self
    }

    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.envs.push((key.to_string(), value.to_string()));
        self
    }

    pub fn with_env_remove(mut self, key: &str) -> Self {
        self.env_remove.push(key.to_string());
        self
    }

    pub fn with_stdin_bytes(mut self, bytes: &[u8]) -> Self {
        self.stdin = Some(bytes.to_vec());
        self
    }

    pub fn with_stdin_str(mut self, input: &str) -> Self {
        self.stdin = Some(input.as_bytes().to_vec());
        self
    }

    pub fn inherit_stdin(mut self) -> Self {
        self.stdin_null = false;
        self
    }
}

/// Build a `PATH` value with `prepend` inserted first and any directory that
/// contains `program` removed.
pub fn path_with_prepend_excluding_program(prepend: &Path, program: &str) -> String {
    let mut paths: Vec<PathBuf> =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .filter(|dir| !dir.join(program).is_file())
            .collect();
    paths.insert(0, prepend.to_path_buf());
    std::env::join_paths(paths)
        .expect("join PATH")
        .to_string_lossy()
        .to_string()
}

/// Run a binary with arguments, capturing `code`, `stdout`, and `stderr`.
///
/// - `envs` overrides or adds environment variables (existing vars are preserved).
/// - `stdin` (when `Some`) is piped into the process before waiting for output.
pub fn run(bin: &Path, args: &[&str], envs: &[(&str, &str)], stdin: Option<&[u8]>) -> CmdOutput {
    let mut options = CmdOptions::default().with_envs(envs);
    if let Some(input) = stdin {
        options = options.with_stdin_bytes(input);
    }
    run_with(bin, args, &options)
}

/// Run a binary in a specific directory.
pub fn run_in_dir(
    dir: &Path,
    bin: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    stdin: Option<&[u8]>,
) -> CmdOutput {
    let mut options = CmdOptions::default().with_cwd(dir).with_envs(envs);
    if let Some(input) = stdin {
        options = options.with_stdin_bytes(input);
    }
    run_with(bin, args, &options)
}

/// Build command options with cwd + env pairs for common integration test usage.
pub fn options_in_dir_with_envs(dir: &Path, envs: &[(&str, &str)]) -> CmdOptions {
    CmdOptions::default().with_cwd(dir).with_envs(envs)
}

/// Resolve a workspace binary by name and run it with explicit options.
pub fn run_resolved(bin_name: &str, args: &[&str], options: &CmdOptions) -> CmdOutput {
    let bin = crate::bin::resolve(bin_name);
    run_with(&bin, args, options)
}

/// Resolve and run a workspace binary in a specific directory.
pub fn run_resolved_in_dir(
    bin_name: &str,
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    stdin: Option<&[u8]>,
) -> CmdOutput {
    let mut options = options_in_dir_with_envs(dir, envs);
    if let Some(input) = stdin {
        options = options.with_stdin_bytes(input);
    }
    run_resolved(bin_name, args, &options)
}

/// Resolve and run a workspace binary in a specific directory with optional
/// UTF-8 stdin.
///
/// When `stdin` is `None`, this helper sends empty stdin bytes to keep test
/// command execution non-interactive.
pub fn run_resolved_in_dir_with_stdin_str(
    bin_name: &str,
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    stdin: Option<&str>,
) -> CmdOutput {
    let mut options = options_in_dir_with_envs(dir, envs);
    options = match stdin {
        Some(input) => options.with_stdin_str(input),
        None => options.with_stdin_bytes(&[]),
    };
    run_resolved(bin_name, args, &options)
}

pub fn run_with(bin: &Path, args: &[&str], options: &CmdOptions) -> CmdOutput {
    run_impl(bin, args, options, None)
}

pub fn run_in_dir_with(dir: &Path, bin: &Path, args: &[&str], options: &CmdOptions) -> CmdOutput {
    run_impl(bin, args, options, Some(dir))
}

fn run_impl(bin: &Path, args: &[&str], options: &CmdOptions, dir: Option<&Path>) -> CmdOutput {
    let mut cmd = Command::new(bin);
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    } else if let Some(cwd) = options.cwd.as_deref() {
        cmd.current_dir(cwd);
    }

    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

    for key in &options.env_remove {
        cmd.env_remove(key);
    }
    for (key, value) in &options.envs {
        cmd.env(key, value);
    }

    let output = match options.stdin.as_ref() {
        Some(input) => {
            cmd.stdin(Stdio::piped());
            let mut child = cmd.spawn().expect("spawn command");
            if let Some(mut writer) = child.stdin.take() {
                writer.write_all(input).expect("write stdin");
            }
            child.wait_with_output().expect("wait command")
        }
        None => {
            if options.stdin_null {
                cmd.stdin(Stdio::null());
            }
            cmd.output().expect("run command")
        }
    };

    CmdOutput {
        code: output.status.code().unwrap_or(-1),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}
