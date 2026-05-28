use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn absolutize(path: impl AsRef<Path>) -> Option<PathBuf> {
    let path = path.as_ref();
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }

    Some(env::current_dir().ok()?.join(path))
}

fn git_dir(flag: &str) -> Option<PathBuf> {
    command_stdout("git", &["rev-parse", "--path-format=absolute", flag])
        .map(PathBuf::from)
        .or_else(|| command_stdout("git", &["rev-parse", flag]).and_then(absolutize))
}

fn emit_git_dir_rerun_paths(git_dir: &Path) {
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    println!("cargo:rerun-if-changed={}", git_dir.join("refs").display());
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join("packed-refs").display()
    );
}

fn emit_git_rerun_paths() {
    let Some(worktree_git_dir) = git_dir("--git-dir") else {
        return;
    };
    emit_git_dir_rerun_paths(&worktree_git_dir);

    let Some(common_dir) = git_dir("--git-common-dir") else {
        return;
    };
    if common_dir != worktree_git_dir {
        emit_git_dir_rerun_paths(&common_dir);
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=RUSTC");

    let describe = command_stdout("git", &["describe", "--tags", "--always", "--dirty"])
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=NILS_GIT_DESCRIBE={describe}");

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rustc_version = command_stdout(&rustc, &["--version"])
        .map(|value| value.strip_prefix("rustc ").unwrap_or(&value).to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=NILS_RUSTC_VERSION={rustc_version}");

    emit_git_rerun_paths();
}
