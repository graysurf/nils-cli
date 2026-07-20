use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::HookError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetBinding {
    pub effective_path: PathBuf,
    pub binding_root: PathBuf,
}

pub fn resolve_target_bindings(paths: &[PathBuf]) -> Result<Vec<TargetBinding>, HookError> {
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
    let canonical = fs::canonicalize(root)
        .map_err(|_| untrusted("mutation target repository root cannot be canonicalized"))?;
    if !canonical.is_dir() || !start.starts_with(&canonical) {
        return Err(untrusted(
            "mutation target repository root does not contain its effective target",
        ));
    }
    Ok(Some(canonical))
}

fn untrusted(message: &str) -> HookError {
    HookError::data("provider-target-untrusted", message)
}
