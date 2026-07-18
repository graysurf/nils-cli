#![allow(dead_code)]

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
}

struct PrivateTestBinary {
    _directory: TempDir,
    path: PathBuf,
}

fn nearest_trusted_copy_parent(binary: &Path) -> PathBuf {
    let euid = unsafe { libc::geteuid() };
    binary
        .parent()
        .expect("git-cli Cargo output parent")
        .ancestors()
        .find(|candidate| {
            candidate.ancestors().all(|ancestor| {
                std::fs::symlink_metadata(ancestor).is_ok_and(|metadata| {
                    metadata.file_type().is_dir()
                        && (metadata.uid() == 0 || metadata.uid() == euid)
                        && metadata.mode() & 0o022 == 0
                })
            })
        })
        .expect("trusted git-cli test binary parent")
        .to_path_buf()
}

impl GitCliHarness {
    pub fn new() -> Self {
        let home_dir = TempDir::new().expect("tempdir");
        let xdg_config_home = home_dir.path().join(".config");
        std::fs::create_dir_all(&xdg_config_home).expect("create XDG_CONFIG_HOME");

        let stub_bin_dir = StubBinDir::new();
        nils_test_support::stubs::install_git_cli_runtime_stubs(&stub_bin_dir);

        Self {
            home_dir,
            xdg_config_home,
            stub_bin_dir,
        }
    }

    pub fn git_cli_bin(&self) -> PathBuf {
        static TEST_BINARY: OnceLock<PrivateTestBinary> = OnceLock::new();
        TEST_BINARY
            .get_or_init(|| {
                let shared = resolve("git-cli");
                let parent = nearest_trusted_copy_parent(&shared);
                let directory = tempfile::Builder::new()
                    .prefix(".git-cli-integration-")
                    .tempdir_in(&parent)
                    .expect("create private git-cli test binary directory");
                std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                    .expect("secure git-cli test binary directory");
                let path = directory.path().join("git-cli");
                let staged = directory.path().join(".git-cli.staged");
                let mut source =
                    std::fs::File::open(&shared).expect("open shared git-cli test binary");
                let mut destination = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o700)
                    .open(&staged)
                    .expect("create staged git-cli test binary");
                std::io::copy(&mut source, &mut destination).expect("copy git-cli test binary");
                destination
                    .sync_all()
                    .expect("sync staged git-cli test binary");
                drop(destination);
                std::fs::rename(&staged, &path).expect("publish git-cli test binary copy");
                PrivateTestBinary {
                    _directory: directory,
                    path,
                }
            })
            .path
            .clone()
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
    fn git_cli_test_binary_is_an_isolated_private_copy() {
        let shared = resolve("git-cli");
        let shared_mode = std::fs::metadata(&shared)
            .expect("shared git-cli metadata")
            .permissions()
            .mode();
        let harness = GitCliHarness::new();
        let isolated = harness.git_cli_bin();

        assert_ne!(
            std::fs::canonicalize(&isolated).expect("canonical isolated binary"),
            std::fs::canonicalize(&shared).expect("canonical shared binary"),
            "integration harness must not execute or chmod the shared Cargo output"
        );
        assert_eq!(
            std::fs::metadata(&shared)
                .expect("shared git-cli metadata after harness setup")
                .permissions()
                .mode(),
            shared_mode,
            "harness setup must not mutate the shared Cargo output"
        );
        assert_eq!(
            std::fs::metadata(&isolated)
                .expect("isolated git-cli metadata")
                .permissions()
                .mode()
                & 0o077,
            0,
            "isolated test binary must be private"
        );
        assert_eq!(
            std::fs::metadata(isolated.parent().expect("isolated binary parent"))
                .expect("isolated binary parent metadata")
                .permissions()
                .mode()
                & 0o077,
            0,
            "isolated test binary parent must be private"
        );
    }
}
