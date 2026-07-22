use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::HookError;

const MAX_GIT_MARKER_BYTES: u64 = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetBinding {
    pub effective_path: PathBuf,
    pub binding_root: PathBuf,
}

pub fn resolve_target_bindings(paths: &[PathBuf]) -> Result<Vec<TargetBinding>, HookError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    reject_repository_selection_environment()?;
    let mut root_cache = BTreeMap::<PathBuf, PathBuf>::new();
    paths
        .iter()
        .map(|path| {
            if !path.is_absolute() {
                return Err(untrusted(
                    "mutation target binding requires an absolute path",
                ));
            }
            let (effective_path, existing_ancestor, existing_is_directory) =
                resolve_effective_path(path)?;
            let checkout_start = if effective_path != existing_ancestor || existing_is_directory {
                existing_ancestor.clone()
            } else {
                existing_ancestor
                    .parent()
                    .map(Path::to_path_buf)
                    .ok_or_else(|| untrusted("mutation target has no resolvable parent"))?
            };
            let cache_key = checkout_cache_key(&checkout_start);
            let repository_root = match root_cache.get(&cache_key) {
                Some(root) if checkout_start.starts_with(root) => Some(root.clone()),
                Some(_) => {
                    return Err(untrusted(
                        "mutation target does not belong to its cached repository root",
                    ));
                }
                None => {
                    let root = repository_root(&checkout_start)?;
                    if let Some(root) = root.as_ref() {
                        root_cache.insert(cache_key, root.clone());
                    }
                    root
                }
            };
            Ok(TargetBinding {
                effective_path,
                binding_root: repository_root.unwrap_or(checkout_start),
            })
        })
        .collect()
}

fn reject_repository_selection_environment() -> Result<(), HookError> {
    const REPOSITORY_SELECTION_VARIABLES: [&str; 7] = [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
    ];
    if REPOSITORY_SELECTION_VARIABLES
        .iter()
        .any(|variable| std::env::var_os(variable).is_some())
    {
        return Err(untrusted(
            "mutation target repository selection environment is ambiguous",
        ));
    }
    Ok(())
}

fn checkout_cache_key(start: &Path) -> PathBuf {
    start
        .ancestors()
        .find(|ancestor| fs::symlink_metadata(ancestor.join(".git")).is_ok())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| start.to_path_buf())
}

fn resolve_effective_path(path: &Path) -> Result<(PathBuf, PathBuf, bool), HookError> {
    let mut candidate = path.to_path_buf();
    let mut missing_suffix = Vec::<OsString>::new();
    loop {
        match fs::symlink_metadata(&candidate) {
            Ok(_) => break,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let component = candidate.file_name().ok_or_else(|| {
                    untrusted("mutation target has no existing resolvable ancestor")
                })?;
                missing_suffix.push(component.to_os_string());
                candidate = candidate.parent().map(Path::to_path_buf).ok_or_else(|| {
                    untrusted("mutation target has no existing resolvable ancestor")
                })?;
            }
            Err(_) => {
                return Err(untrusted(
                    "mutation target ancestry cannot be resolved without ambiguity",
                ));
            }
        }
    }
    let existing_ancestor = fs::canonicalize(&candidate)
        .map_err(|_| untrusted("mutation target symlink cannot be resolved without ambiguity"))?;
    let existing_is_directory = fs::metadata(&existing_ancestor)
        .map_err(|_| untrusted("mutation target ancestor metadata is unavailable"))?
        .is_dir();
    if !missing_suffix.is_empty() && !existing_is_directory {
        return Err(untrusted(
            "mutation target descends from a non-directory ancestor",
        ));
    }
    let mut effective_path = existing_ancestor.clone();
    for component in missing_suffix.iter().rev() {
        effective_path.push(component);
    }
    Ok((effective_path, existing_ancestor, existing_is_directory))
}

fn repository_root(start: &Path) -> Result<Option<PathBuf>, HookError> {
    match repository_root_in_process(start)? {
        RepositoryRootProbe::Resolved(root) => Ok(Some(root)),
        RepositoryRootProbe::Fallback => repository_root_with_git(start),
    }
}

enum RepositoryRootProbe {
    Resolved(PathBuf),
    Fallback,
}

fn repository_root_in_process(start: &Path) -> Result<RepositoryRootProbe, HookError> {
    for checkout_root in start.ancestors() {
        let marker = checkout_root.join(".git");
        let metadata = match fs::symlink_metadata(&marker) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(_) => {
                return Err(untrusted(
                    "mutation target repository marker metadata is unavailable",
                ));
            }
        };
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            return Err(untrusted(
                "mutation target repository marker must not be a symlink",
            ));
        }
        if file_type.is_dir() {
            verify_stable_directory(&marker, &metadata)?;
            if standard_git_admin(&marker, &marker) {
                return canonical_repository_root(checkout_root, start)
                    .map(RepositoryRootProbe::Resolved);
            }
            return Ok(RepositoryRootProbe::Fallback);
        }
        if file_type.is_file() {
            return linked_worktree_root(&marker, checkout_root, start);
        }
        return Err(untrusted(
            "mutation target repository marker has an unsupported file type",
        ));
    }
    Ok(RepositoryRootProbe::Fallback)
}

fn verify_stable_directory(path: &Path, expected: &fs::Metadata) -> Result<(), HookError> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| untrusted("mutation target repository marker directory is unavailable"))?;
    let observed = directory
        .metadata()
        .map_err(|_| untrusted("mutation target repository marker metadata is unavailable"))?;
    if !observed.is_dir() || observed.dev() != expected.dev() || observed.ino() != expected.ino() {
        return Err(untrusted(
            "mutation target repository marker changed during resolution",
        ));
    }
    Ok(())
}

fn linked_worktree_root(
    marker: &Path,
    checkout_root: &Path,
    start: &Path,
) -> Result<RepositoryRootProbe, HookError> {
    let marker_bytes = read_bounded_regular_file(marker)?.ok_or_else(|| {
        untrusted("mutation target linked-worktree marker disappeared during resolution")
    })?;
    let admin_path = parse_gitdir_marker(&marker_bytes)?;
    let admin_path = resolve_marker_path(checkout_root, &admin_path)
        .ok_or_else(|| untrusted("mutation target linked-worktree git directory is unavailable"))?;
    if !admin_path.is_dir() {
        return Err(untrusted(
            "mutation target linked-worktree git directory is invalid",
        ));
    }

    let Some(commondir_bytes) = read_bounded_regular_file(&admin_path.join("commondir"))? else {
        return Ok(RepositoryRootProbe::Fallback);
    };
    let Some(backlink_bytes) = read_bounded_regular_file(&admin_path.join("gitdir"))? else {
        return Ok(RepositoryRootProbe::Fallback);
    };
    let common_path = parse_path_line(&commondir_bytes, "linked-worktree commondir")?;
    let common_path = resolve_marker_path(&admin_path, &common_path).ok_or_else(|| {
        untrusted("mutation target linked-worktree common directory is unavailable")
    })?;
    if !common_path.is_dir() {
        return Err(untrusted(
            "mutation target linked-worktree common directory is invalid",
        ));
    }
    let backlink = parse_path_line(&backlink_bytes, "linked-worktree gitdir")?;
    let backlink = resolve_marker_path(&admin_path, &backlink)
        .ok_or_else(|| untrusted("mutation target linked-worktree backlink is unavailable"))?;
    let canonical_marker = fs::canonicalize(marker)
        .map_err(|_| untrusted("mutation target linked-worktree marker cannot be canonicalized"))?;
    if backlink != canonical_marker {
        return Err(untrusted(
            "mutation target linked-worktree backlink does not match its marker",
        ));
    }
    if !standard_git_admin(&admin_path, &common_path) {
        return Ok(RepositoryRootProbe::Fallback);
    }

    canonical_repository_root(checkout_root, start).map(RepositoryRootProbe::Resolved)
}

fn standard_git_admin(admin_path: &Path, common_path: &Path) -> bool {
    let Some(head) = read_bounded_regular_file(&admin_path.join("HEAD"))
        .ok()
        .flatten()
    else {
        return false;
    };
    valid_head(&head)
        && stable_regular_file(&common_path.join("config"))
        && stable_directory(&common_path.join("objects"))
        && stable_directory(&common_path.join("refs"))
}

fn valid_head(bytes: &[u8]) -> bool {
    let Ok(line) = single_marker_line(bytes, "repository HEAD") else {
        return false;
    };
    if matches!(line.len(), 40 | 64) && line.iter().all(u8::is_ascii_hexdigit) {
        return true;
    }
    line.strip_prefix(b"ref: refs/heads/")
        .is_some_and(valid_head_ref)
}

fn valid_head_ref(reference: &[u8]) -> bool {
    !reference.is_empty()
        && !reference.windows(2).any(|pair| pair == b"..")
        && reference.split(|byte| *byte == b'/').all(|component| {
            !component.is_empty()
                && !component.starts_with(b".")
                && !component.ends_with(b".")
                && !component.ends_with(b".lock")
                && component
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}

fn stable_regular_file(path: &Path) -> bool {
    let Ok(expected) = fs::symlink_metadata(path) else {
        return false;
    };
    if expected.file_type().is_symlink() || !expected.is_file() {
        return false;
    }
    let Ok(file) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    else {
        return false;
    };
    file.metadata().is_ok_and(|observed| {
        observed.is_file() && observed.dev() == expected.dev() && observed.ino() == expected.ino()
    })
}

fn stable_directory(path: &Path) -> bool {
    let Ok(expected) = fs::symlink_metadata(path) else {
        return false;
    };
    !expected.file_type().is_symlink()
        && expected.is_dir()
        && verify_stable_directory(path, &expected).is_ok()
}

fn read_bounded_regular_file(path: &Path) -> Result<Option<Vec<u8>>, HookError> {
    let expected = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(untrusted(
                "mutation target repository marker metadata is unavailable",
            ));
        }
    };
    if expected.file_type().is_symlink() || !expected.is_file() {
        return Err(untrusted(
            "mutation target repository marker must be a regular file",
        ));
    }
    if expected.len() > MAX_GIT_MARKER_BYTES {
        return Err(untrusted(
            "mutation target repository marker exceeds the size limit",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| untrusted("mutation target repository marker cannot be opened safely"))?;
    let observed = file
        .metadata()
        .map_err(|_| untrusted("mutation target repository marker metadata is unavailable"))?;
    if !observed.is_file() || observed.dev() != expected.dev() || observed.ino() != expected.ino() {
        return Err(untrusted(
            "mutation target repository marker changed during resolution",
        ));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_GIT_MARKER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| untrusted("mutation target repository marker cannot be read safely"))?;
    if bytes.len() as u64 > MAX_GIT_MARKER_BYTES {
        return Err(untrusted(
            "mutation target repository marker exceeds the size limit",
        ));
    }
    Ok(Some(bytes))
}

fn parse_gitdir_marker(bytes: &[u8]) -> Result<PathBuf, HookError> {
    let line = single_marker_line(bytes, "linked-worktree .git")?;
    let path = line
        .strip_prefix(b"gitdir: ")
        .ok_or_else(|| untrusted("mutation target linked-worktree marker is malformed"))?;
    path_from_marker_bytes(path, "linked-worktree .git")
}

fn parse_path_line(bytes: &[u8], label: &str) -> Result<PathBuf, HookError> {
    let line = single_marker_line(bytes, label)?;
    path_from_marker_bytes(line, label)
}

fn single_marker_line<'a>(bytes: &'a [u8], label: &str) -> Result<&'a [u8], HookError> {
    let line = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.is_empty()
        || line
            .iter()
            .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r'))
    {
        return Err(untrusted(&format!(
            "mutation target {label} marker is malformed"
        )));
    }
    Ok(line)
}

fn path_from_marker_bytes(bytes: &[u8], label: &str) -> Result<PathBuf, HookError> {
    if bytes.is_empty() {
        return Err(untrusted(&format!(
            "mutation target {label} marker is malformed"
        )));
    }
    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

fn resolve_marker_path(base: &Path, path: &Path) -> Option<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    fs::canonicalize(path).ok()
}

fn repository_root_with_git(start: &Path) -> Result<Option<PathBuf>, HookError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| untrusted("mutation target repository root cannot be resolved"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let raw = std::str::from_utf8(&output.stdout)
        .map_err(|_| untrusted("mutation target repository root is not valid UTF-8"))?;
    let root = raw.strip_suffix('\n').unwrap_or(raw);
    let root = root.strip_suffix('\r').unwrap_or(root);
    if root.is_empty() || root.contains(['\n', '\r', '\0']) {
        return Err(untrusted(
            "mutation target repository root output is ambiguous",
        ));
    }
    let root = PathBuf::from(root);
    if !root.is_absolute() {
        return Err(untrusted("mutation target repository root is not absolute"));
    }
    canonical_repository_root(&root, start).map(Some)
}

fn canonical_repository_root(root: &Path, start: &Path) -> Result<PathBuf, HookError> {
    let canonical = fs::canonicalize(root)
        .map_err(|_| untrusted("mutation target repository root cannot be canonicalized"))?;
    if !canonical.is_dir() || !start.starts_with(&canonical) {
        return Err(untrusted(
            "mutation target repository root does not contain its effective target",
        ));
    }
    Ok(canonical)
}

fn untrusted(message: &str) -> HookError {
    HookError::data("provider-target-untrusted", message)
}
