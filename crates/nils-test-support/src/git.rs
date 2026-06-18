use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use tempfile::TempDir;

static GIT_PATH: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct InitRepoOptions {
    pub branch: Option<String>,
    pub initial_commit: bool,
    pub initial_commit_name: String,
    pub initial_commit_contents: String,
    pub initial_commit_message: String,
}

impl InitRepoOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_branch(mut self, branch: impl Into<String>) -> Self {
        self.branch = Some(branch.into());
        self
    }

    pub fn without_branch(mut self) -> Self {
        self.branch = None;
        self
    }

    pub fn with_initial_commit(mut self) -> Self {
        self.initial_commit = true;
        self
    }
}

impl Default for InitRepoOptions {
    fn default() -> Self {
        Self {
            branch: Some("main".to_string()),
            initial_commit: false,
            initial_commit_name: "README.md".to_string(),
            initial_commit_contents: "init".to_string(),
            initial_commit_message: "init".to_string(),
        }
    }
}

pub fn git_output(dir: &Path, args: &[&str]) -> Output {
    Command::new(git_command())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git command failed to spawn")
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = git_output(dir, args);
    if !output.status.success() {
        panic!(
            "git {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    }
    String::from_utf8_lossy(&output.stdout).to_string()
}

pub fn git_with_env(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> String {
    let output = git_output_with_env(dir, args, envs);
    if !output.status.success() {
        panic!(
            "git {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    }
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn git_output_with_env(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(git_command());
    cmd.args(args).current_dir(dir);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.output().expect("git command failed to spawn")
}

fn git_command() -> &'static Path {
    GIT_PATH.get_or_init(resolve_git_path).as_path()
}

fn resolve_git_path() -> PathBuf {
    if let Some(path) =
        env::var_os("NILS_TEST_SUPPORT_GIT").and_then(|value| executable_path(PathBuf::from(value)))
    {
        return path;
    }

    if let Some(path) =
        option_env!("PATH").and_then(|value| find_in_path_value(OsStr::new(value), "git"))
    {
        return path;
    }

    if let Some(path) = env::var_os("PATH").and_then(|value| find_in_path_value(&value, "git")) {
        return path;
    }

    for candidate in common_git_candidates() {
        if let Some(path) = executable_path(PathBuf::from(candidate)) {
            return path;
        }
    }

    panic!("git command failed to resolve; set NILS_TEST_SUPPORT_GIT to the git executable");
}

fn find_in_path_value(paths: &OsStr, program: &str) -> Option<PathBuf> {
    for dir in env::split_paths(paths) {
        for candidate in path_lookup_candidates(&dir, program) {
            if let Some(path) = executable_path(candidate) {
                return Some(path);
            }
        }
    }
    None
}

fn path_lookup_candidates(dir: &Path, program: &str) -> Vec<PathBuf> {
    #[cfg(not(windows))]
    {
        vec![dir.join(program)]
    }

    #[cfg(windows)]
    {
        let mut candidates = vec![dir.join(program)];

        if Path::new(program).extension().is_none() {
            for extension in windows_pathext_extensions() {
                let mut file_name = std::ffi::OsString::from(program);
                file_name.push(extension);
                candidates.push(dir.join(file_name));
            }
        }

        candidates
    }
}

fn executable_path(path: PathBuf) -> Option<PathBuf> {
    is_executable_file(&path).then_some(path)
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

#[cfg(unix)]
fn common_git_candidates() -> &'static [&'static str] {
    &[
        "/usr/bin/git",
        "/usr/local/bin/git",
        "/opt/homebrew/bin/git",
        "/home/linuxbrew/.linuxbrew/bin/git",
    ]
}

#[cfg(windows)]
fn common_git_candidates() -> &'static [&'static str] {
    &[]
}

#[cfg(windows)]
fn windows_pathext_extensions() -> Vec<std::ffi::OsString> {
    let pathext =
        env::var_os("PATHEXT").unwrap_or_else(|| std::ffi::OsString::from(".COM;.EXE;.BAT;.CMD"));
    pathext
        .to_string_lossy()
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(std::ffi::OsString::from)
        .collect()
}

pub fn init_repo_at_with(dir: &Path, options: InitRepoOptions) {
    git(dir, &["init", "-q"]);

    if let Some(branch) = options.branch.as_deref() {
        // Make the initial branch deterministic across environments.
        git(dir, &["checkout", "-q", "-B", branch]);
    }

    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["config", "tag.gpgSign", "false"]);

    if options.initial_commit {
        let file_path = dir.join(&options.initial_commit_name);
        fs::write(&file_path, &options.initial_commit_contents).expect("write initial commit");
        git(dir, &["add", &options.initial_commit_name]);
        git(dir, &["commit", "-m", &options.initial_commit_message]);
    }
}

pub fn init_repo_with(options: InitRepoOptions) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    init_repo_at_with(dir.path(), options);
    dir
}

pub fn init_repo_main() -> TempDir {
    init_repo_with(InitRepoOptions::new().with_branch("main"))
}

pub fn init_repo_main_with_initial_commit() -> TempDir {
    init_repo_with(
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    )
}

pub fn worktree_add_branch(repo: &Path, worktree_path: &Path, branch: &str) {
    let worktree_path = worktree_path.to_string_lossy().to_string();
    git(repo, &["worktree", "add", &worktree_path, "-b", branch]);
}

pub fn commit_file(dir: &Path, name: &str, contents: &str, message: &str) -> String {
    let path = dir.join(name);
    fs::write(&path, contents).expect("write file");
    git(dir, &["add", name]);
    git(dir, &["commit", "-m", message]);
    git(dir, &["rev-parse", "HEAD"]).trim().to_string()
}

pub fn repo_id(dir: &Path) -> String {
    dir.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_string()
}
