//! Regression coverage for the shared trusted `git-cli` test binary cache.
//!
//! The installer used to hold its `TempDir` in a `static OnceLock`. Statics are
//! never dropped, so every test process leaked a private `/tmp/.tmpXXXXXX`
//! holding a full ~19 MB copy of the binary. Under `cargo nextest` — one
//! process per test — that is one leaked copy per test rather than one per test
//! binary, which is how the leak reached hundreds of gigabytes.
//!
//! The contract these tests pin: the trusted copy lives at a deterministic path
//! under `CARGO_TARGET_TMPDIR`, is reused instead of recreated, and never lands
//! in the system temp directory.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use crate::common::{install_trusted_git_cli_binary, trusted_git_cli_binary};

#[test]
fn trusted_binary_lives_under_cargo_target_tmpdir() {
    let path = trusted_git_cli_binary();
    let cargo_target_tmpdir = Path::new(env!("CARGO_TARGET_TMPDIR"));

    assert!(
        path.starts_with(cargo_target_tmpdir),
        "the trusted test binary must live under CARGO_TARGET_TMPDIR so `cargo clean` \
         reclaims it; a system-temp copy is leaked once per test process: {}",
        path.display()
    );
}

#[test]
fn trusted_binary_is_not_installed_in_the_system_temp_dir() {
    let path = trusted_git_cli_binary();
    let system_temp = std::env::temp_dir();

    assert!(
        !path.starts_with(&system_temp),
        "the trusted test binary must not be installed under {}: that copy outlives the \
         process because no destructor runs for it",
        system_temp.display()
    );
}

#[test]
fn repeated_installs_reuse_one_copy_instead_of_creating_another() {
    let first = install_trusted_git_cli_binary().expect("first install");
    let second = install_trusted_git_cli_binary().expect("second install");

    assert_eq!(
        first, second,
        "the installer must resolve to one deterministic cached path"
    );

    let first_metadata = std::fs::metadata(&first).expect("first metadata");
    let second_metadata = std::fs::metadata(&second).expect("second metadata");
    assert_eq!(
        (first_metadata.dev(), first_metadata.ino()),
        (second_metadata.dev(), second_metadata.ino()),
        "a second install must reuse the cached inode rather than copy the binary again"
    );
}

#[test]
fn cached_binary_keeps_the_trusted_permission_contract() {
    let path = trusted_git_cli_binary();
    let metadata = std::fs::metadata(&path).expect("cached binary metadata");
    let mode = metadata.permissions().mode();

    assert!(
        metadata.file_type().is_file(),
        "cached binary must be a file"
    );
    assert_eq!(
        metadata.uid(),
        unsafe { libc::geteuid() },
        "cached binary must be owned by the test user"
    );
    assert_ne!(mode & 0o111, 0, "cached binary must stay executable");
    assert_eq!(
        mode & 0o022,
        0,
        "cached binary must not be group- or world-writable"
    );
}

#[test]
fn cached_binary_keeps_the_git_cli_file_name() {
    let path = trusted_git_cli_binary();

    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("git-cli"),
        "the cache key belongs in the directory name: the binary reports its own argv[0] in \
         usage output and checks current_exe().file_name() == \"git-cli\" to trust itself, so \
         renaming the executable breaks both: {}",
        path.display()
    );
}

#[test]
fn install_leaves_no_staging_file_behind() {
    install_trusted_git_cli_binary().expect("install");

    // Scoped to this process: nextest runs test processes in parallel, so on a
    // cold cache a sibling process can legitimately have its own staging file
    // in flight. The contract is that an install cleans up after *itself*.
    let staging_name = format!(".staging-{}", std::process::id());
    let cache_root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("trusted-git-cli");
    let staging: Vec<_> = std::fs::read_dir(&cache_root)
        .expect("cache root")
        .filter_map(Result::ok)
        .flat_map(|key_dir| {
            std::fs::read_dir(key_dir.path())
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
        })
        .filter(|entry| entry.file_name().to_string_lossy() == staging_name)
        .map(|entry| entry.path())
        .collect();

    assert!(
        staging.is_empty(),
        "a completed install must rename its staging file into place, leaving none behind: \
         {staging:?}"
    );
}
