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

fn copy_trusted_git_cli_binary(source_path: &Path, destination_path: &Path) -> io::Result<PathBuf> {
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

    let mut destination = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(destination_path)?;
    io::copy(&mut &source, &mut destination)?;
    destination.sync_all()?;
    // tempdir-leak-audit: allow — permanent hardening of the installed private binary.
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
    let destination_path_metadata = std::fs::metadata(destination_path)?;
    if !destination_metadata.file_type().is_file()
        || destination_metadata.uid() != euid
        || destination_metadata.permissions().mode() & 0o111 == 0
        || destination_metadata.permissions().mode() & 0o022 != 0
        || !same_file_metadata(&destination_metadata, &destination_path_metadata)
    {
        return Err(invalid_binary("private git-cli test binary is untrusted"));
    }
    Ok(destination_path.to_path_buf())
}

/// Deterministic home for the trusted `git-cli` copy shared by every test
/// process.
///
/// This lives under `CARGO_TARGET_TMPDIR` — inside the build tree — so
/// `cargo clean` reclaims it and no copy can outlive the binary it mirrors.
/// The previous implementation put the copy in a `TempDir` owned by a `static`.
/// Statics are never dropped, so that directory was never removed; under
/// `cargo nextest`, which runs one process per test, it leaked a ~19 MB copy
/// per test rather than per test binary.
fn trusted_binary_cache_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("trusted-git-cli")
}

/// Cache key over the source binary's identity. Any rebuild changes its size or
/// mtime, so a rebuilt `git-cli` is never served from a stale copy.
fn trusted_binary_cache_key(metadata: &std::fs::Metadata) -> String {
    format!(
        "{:016x}-{:016x}-{:08x}",
        metadata.size(),
        metadata.mtime(),
        metadata.mtime_nsec()
    )
}

/// Re-check the trust contract against an already-cached copy, so a tampered or
/// truncated entry is replaced instead of executed.
fn cached_binary_is_trusted(path: &Path, expected_size: u64) -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    let mode = metadata.permissions().mode();
    metadata.file_type().is_file()
        && metadata.uid() == unsafe { libc::geteuid() }
        && metadata.size() == expected_size
        && mode & 0o111 != 0
        && mode & 0o022 == 0
}

/// How long a superseded cache entry, or a staging file abandoned by a process
/// that died mid-install, may linger before a later run sweeps it. Comfortably
/// longer than any test run, so a live entry is never a candidate.
const STALE_CACHE_ENTRY_AFTER: std::time::Duration = std::time::Duration::from_secs(60 * 60);

fn is_stale(metadata: &std::fs::Metadata) -> bool {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= STALE_CACHE_ENTRY_AFTER)
}

/// Sweep cache entries for superseded builds plus staging files abandoned by a
/// process that died mid-install.
///
/// A rebuilt `git-cli` gets a new cache key, so without this the cache would
/// grow by one ~19 MB copy per rebuild. `current_key` is never swept, and
/// neither is anything younger than [`STALE_CACHE_ENTRY_AFTER`], so a
/// concurrent test run cannot have its binary removed from under it.
fn sweep_stale_cache_entries(cache_root: &Path, current_key: &str) {
    let Ok(entries) = std::fs::read_dir(cache_root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let is_current = entry.file_name().to_string_lossy() == current_key;

        if !is_current && is_stale(&metadata) {
            if metadata.is_dir() {
                let _ = std::fs::remove_dir_all(entry.path());
            } else {
                let _ = std::fs::remove_file(entry.path());
            }
            continue;
        }

        if metadata.is_dir() {
            sweep_stale_staging_files(&entry.path());
        }
    }
}

fn sweep_stale_staging_files(cache_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        if !entry.file_name().to_string_lossy().starts_with(".staging-") {
            continue;
        }
        if entry.metadata().is_ok_and(|metadata| is_stale(&metadata)) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Install, or reuse, the private trusted `git-cli` copy shared by every test
/// process.
pub fn install_trusted_git_cli_binary() -> io::Result<PathBuf> {
    let source_path = resolve("git-cli");
    let source_metadata = std::fs::metadata(&source_path)?;
    let cache_root = trusted_binary_cache_dir();
    std::fs::create_dir_all(&cache_root)?;
    std::fs::set_permissions(&cache_root, std::fs::Permissions::from_mode(0o700))?;

    // The cache key goes in the directory name, never the file name: the binary
    // reports its own `argv[0]` in usage output and checks
    // `current_exe().file_name() == "git-cli"` when deciding whether to trust
    // itself, so the executable must keep its real name.
    let cache_key = trusted_binary_cache_key(&source_metadata);
    sweep_stale_cache_entries(&cache_root, &cache_key);

    let cache_dir = cache_root.join(&cache_key);
    std::fs::create_dir_all(&cache_dir)?;
    std::fs::set_permissions(&cache_dir, std::fs::Permissions::from_mode(0o700))?;

    let cached_path = cache_dir.join("git-cli");
    if cached_binary_is_trusted(&cached_path, source_metadata.size()) {
        return Ok(cached_path);
    }

    // Stage under a process-unique name in the same directory, then rename into
    // place. Writing the final path directly would race a concurrent test
    // process executing it (ETXTBSY); rename swaps the directory entry
    // atomically while any in-flight exec keeps the inode it already opened.
    let staging_path = cache_dir.join(format!(".staging-{}", std::process::id()));
    let _ = std::fs::remove_file(&staging_path);
    if let Err(err) = copy_trusted_git_cli_binary(&source_path, &staging_path) {
        let _ = std::fs::remove_file(&staging_path);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&staging_path, &cached_path) {
        let _ = std::fs::remove_file(&staging_path);
        return Err(err);
    }

    // A concurrent installer may have renamed its own byte-identical copy over
    // ours; re-validate so the losing racer still returns a trusted path.
    if !cached_binary_is_trusted(&cached_path, source_metadata.size()) {
        return Err(invalid_binary(
            "cached private git-cli test binary is untrusted",
        ));
    }
    Ok(cached_path)
}

pub fn trusted_git_cli_binary() -> PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            install_trusted_git_cli_binary().expect("install private trusted git-cli test binary")
        })
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

        let destination = destination_root.path().join("git-cli");
        let validated = copy_trusted_git_cli_binary(&source, &destination)
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
