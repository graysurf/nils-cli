#![allow(dead_code)]

use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use nils_test_support::StubBinDir;
use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_with};
pub use nils_test_support::git::git;
use nils_test_support::git::init_repo_main_with_initial_commit;
use tempfile::TempDir;

pub struct GitCliHarness {
    home_dir: TempDir,
    xdg_config_home: PathBuf,
    stub_bin_dir: StubBinDir,
    git_cli_path: PathBuf,
}

fn invalid_binary(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn same_file_metadata(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.mode() == right.mode()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
        && left.size() == right.size()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

struct TrustedGitCliBinary {
    _root: TempDir,
    path: PathBuf,
}

fn copy_trusted_git_cli_binary(source_path: &Path, destination_dir: &Path) -> io::Result<PathBuf> {
    let source = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(source_path)?;
    let source_metadata = source.metadata()?;
    let euid = unsafe { libc::geteuid() };
    if !source_metadata.file_type().is_file()
        || source_metadata.permissions().mode() & 0o111 == 0
        || (source_metadata.uid() != 0 && source_metadata.uid() != euid)
    {
        return Err(invalid_binary("Cargo-resolved git-cli source is untrusted"));
    }

    let destination_path = destination_dir.join("git-cli");
    let mut destination = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&destination_path)?;
    io::copy(&mut &source, &mut destination)?;
    destination.sync_all()?;
    destination.set_permissions(std::fs::Permissions::from_mode(0o500))?;

    let current_source_metadata = source.metadata()?;
    let source_path_metadata = std::fs::metadata(source_path)?;
    if !same_file_metadata(&source_metadata, &current_source_metadata)
        || !same_file_metadata(&current_source_metadata, &source_path_metadata)
    {
        return Err(invalid_binary(
            "Cargo-resolved git-cli source changed during private installation",
        ));
    }
    let destination_metadata = destination.metadata()?;
    let destination_path_metadata = std::fs::metadata(&destination_path)?;
    if !destination_metadata.file_type().is_file()
        || destination_metadata.uid() != euid
        || destination_metadata.permissions().mode() & 0o111 == 0
        || destination_metadata.permissions().mode() & 0o022 != 0
        || !same_file_metadata(&destination_metadata, &destination_path_metadata)
    {
        return Err(invalid_binary("private git-cli test binary is untrusted"));
    }
    Ok(destination_path)
}

fn trusted_git_cli_binary() -> PathBuf {
    static BINARY: OnceLock<TrustedGitCliBinary> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            let root = TempDir::new().expect("private git-cli binary root");
            let path = copy_trusted_git_cli_binary(&resolve("git-cli"), root.path())
                .expect("install private trusted git-cli test binary");
            TrustedGitCliBinary { _root: root, path }
        })
        .path
        .clone()
}

impl GitCliHarness {
    pub fn new() -> Self {
        let home_dir = TempDir::new().expect("tempdir");
        let xdg_config_home = home_dir.path().join(".config");
        std::fs::create_dir_all(&xdg_config_home).expect("create XDG_CONFIG_HOME");

        let stub_bin_dir = StubBinDir::new();
        nils_test_support::stubs::install_git_cli_runtime_stubs(&stub_bin_dir);
        let git_cli_path = trusted_git_cli_binary();

        Self {
            home_dir,
            xdg_config_home,
            stub_bin_dir,
            git_cli_path,
        }
    }

    pub fn git_cli_bin(&self) -> PathBuf {
        self.git_cli_path.clone()
    }

    pub fn cmd_options(&self, cwd: &Path) -> CmdOptions {
        let home = self.home_dir.path().to_string_lossy().to_string();
        let xdg_config_home = self.xdg_config_home.to_string_lossy().to_string();
        CmdOptions::new()
            .with_cwd(cwd)
            .with_path_prepend(self.stub_bin_dir.path())
            .with_env("HOME", &home)
            .with_env("XDG_CONFIG_HOME", &xdg_config_home)
            .with_env("GIT_CONFIG_NOSYSTEM", "1")
            .with_env("GIT_CONFIG_GLOBAL", "/dev/null")
            .with_env("GIT_PAGER", "cat")
            .with_env("PAGER", "cat")
            .with_env("TERM", "dumb")
            .with_env("TZ", "UTC")
            .with_env("LC_ALL", "C")
            .with_env_remove_prefix("GIT_TRACE")
    }

    pub fn run(&self, cwd: &Path, args: &[&str]) -> CmdOutput {
        run_with(&self.git_cli_bin(), args, &self.cmd_options(cwd))
    }
}

impl Default for GitCliHarness {
    fn default() -> Self {
        Self::new()
    }
}

pub fn init_repo() -> tempfile::TempDir {
    init_repo_main_with_initial_commit()
}

pub fn init_bare_remote() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    git(dir.path(), &["init", "--bare", "-q"]);
    dir
}

pub fn write_context_json_git_stub(stubs: &StubBinDir) {
    stubs.write_exe(
        "git",
        r#"#!/bin/bash
set -euo pipefail

args=("$@")

if [[ ${#args[@]} -ge 2 && "${args[0]}" == "rev-parse" && "${args[1]}" == "--is-inside-work-tree" ]]; then
  exit 0
fi

if [[ ${#args[@]} -ge 4 && "${args[0]}" == "diff" && "${args[1]}" == "--cached" && "${args[2]}" == "--quiet" && "${args[3]}" == "--exit-code" ]]; then
  exit 1
fi

if [[ ${#args[@]} -ge 2 && "${args[0]}" == "symbolic-ref" && "${args[1]}" == "--quiet" ]]; then
  echo "main"
  exit 0
fi

if [[ ${#args[@]} -ge 2 && "${args[0]}" == "rev-parse" && "${args[1]}" == "--short" ]]; then
  echo "abc123"
  exit 0
fi

if [[ ${#args[@]} -ge 2 && "${args[0]}" == "rev-parse" && "${args[1]}" == "--show-toplevel" ]]; then
  pwd
  exit 0
fi

if [[ ${#args[@]} -ge 5 && "${args[0]}" == "-c" && "${args[1]}" == "core.quotepath=false" && "${args[2]}" == "diff" && "${args[3]}" == "--cached" && "${args[4]}" == "--no-color" ]]; then
  echo "diff --git a/hello.txt b/hello.txt"
  exit 0
fi

if [[ ${#args[@]} -ge 6 && "${args[0]}" == "-c" && "${args[1]}" == "core.quotepath=false" && "${args[2]}" == "diff" && "${args[3]}" == "--cached" && "${args[4]}" == "--name-status" && "${args[5]}" == "-z" ]]; then
  printf "A\0hello.txt\0"
  exit 0
fi

if [[ ${#args[@]} -ge 6 && "${args[0]}" == "-c" && "${args[1]}" == "core.quotepath=false" && "${args[2]}" == "diff" && "${args[3]}" == "--cached" && "${args[4]}" == "--numstat" ]]; then
  last_index=$((${#args[@]} - 1))
  path="${args[$last_index]}"
  printf "1\t0\t%s\n" "$path"
  exit 0
fi

exit 0
"#,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_cli_test_binary_uses_a_private_validated_copy() {
        let resolved = resolve("git-cli");
        let source_mode = std::fs::metadata(&resolved)
            .expect("Cargo-resolved binary metadata")
            .permissions()
            .mode();
        let harness = GitCliHarness::new();

        assert_ne!(
            std::fs::canonicalize(harness.git_cli_bin()).expect("canonical harness binary"),
            std::fs::canonicalize(&resolved).expect("canonical Cargo-resolved binary"),
            "parallel tests must not chmod the shared Cargo executable"
        );
        assert_eq!(
            std::fs::metadata(&resolved)
                .expect("unchanged Cargo binary metadata")
                .permissions()
                .mode(),
            source_mode,
            "test setup must not mutate shared Cargo output permissions"
        );
        assert_eq!(
            std::fs::metadata(harness.git_cli_bin())
                .expect("private harness binary metadata")
                .permissions()
                .mode()
                & 0o022,
            0,
            "the private executable must satisfy production trust checks"
        );
    }

    #[test]
    fn git_cli_source_validation_copies_without_mutating_source() {
        let source_root = tempfile::TempDir::new().expect("writable source root");
        let destination_root = tempfile::TempDir::new().expect("private destination root");
        let source = source_root.path().join("git-cli");
        std::fs::write(&source, b"test executable").expect("write source fixture");
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o777))
            .expect("make source fixture writable");

        let validated = copy_trusted_git_cli_binary(&source, destination_root.path())
            .expect("install private executable copy");
        assert_ne!(
            std::fs::canonicalize(&validated).expect("canonical validated fixture"),
            std::fs::canonicalize(&source).expect("canonical source fixture")
        );
        assert_eq!(
            std::fs::metadata(&source)
                .expect("unchanged source metadata")
                .permissions()
                .mode()
                & 0o777,
            0o777
        );
        assert_eq!(
            std::fs::metadata(&validated)
                .expect("private copy metadata")
                .permissions()
                .mode()
                & 0o777,
            0o500
        );
        assert_eq!(
            std::fs::read(&validated).expect("read private fixture"),
            b"test executable"
        );
    }
}
