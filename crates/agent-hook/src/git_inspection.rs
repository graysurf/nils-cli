//! In-process, fail-closed Git inspection for pre-lease decisions.
//!
//! Core Git's worktree comparison may execute repository-configured clean or
//! process filters.  This module deliberately uses a fresh libgit2 process
//! context instead: libgit2 registers only its built-in CRLF and ident filters
//! unless the embedding application explicitly registers another filter.  The
//! agent-hook process never registers repository command filters and disables
//! index refreshes, so inspection cannot cross the
//! no-lease boundary by spawning repository-controlled programs or writing the
//! index.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use git2::{Repository, Status, StatusOptions, SubmoduleIgnore, SubmoduleStatus};
use serde::Serialize;
use serde_json::json;

use crate::dsh_policy::GitLayout;
use crate::error::HookError;

const MAX_DIRTY_ENTRIES: usize = 2_048;
const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_SUBMODULES: usize = 512;
const MAX_SUBMODULE_DEPTH: usize = 16;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DirtyEntry {
    pub(crate) states: Vec<&'static str>,
    pub(crate) path: String,
    pub(crate) lossy: bool,
}

fn unavailable() -> HookError {
    HookError::unavailable_with(
        "workspace-inspection-unavailable",
        "workspace status could not be inspected safely",
        json!({
            "retryable": true,
            "next_action": "verify-checkout-and-retry",
            "recovery": {
                "kind": "bounded-retry",
                "max_attempts": 1,
            },
        }),
    )
}

fn states(status: Status) -> Vec<&'static str> {
    let mut result = Vec::new();
    for (flag, name) in [
        (Status::INDEX_NEW, "index-new"),
        (Status::INDEX_MODIFIED, "index-modified"),
        (Status::INDEX_DELETED, "index-deleted"),
        (Status::INDEX_RENAMED, "index-renamed"),
        (Status::INDEX_TYPECHANGE, "index-typechange"),
        (Status::WT_NEW, "worktree-new"),
        (Status::WT_MODIFIED, "worktree-modified"),
        (Status::WT_DELETED, "worktree-deleted"),
        (Status::WT_RENAMED, "worktree-renamed"),
        (Status::WT_TYPECHANGE, "worktree-typechange"),
        (Status::WT_UNREADABLE, "worktree-unreadable"),
        (Status::CONFLICTED, "conflicted"),
    ] {
        if status.contains(flag) {
            result.push(name);
        }
    }
    result
}

fn submodule_states(status: SubmoduleStatus) -> Vec<&'static str> {
    let mut result = Vec::new();
    for (dirty, name) in [
        (status.is_index_added(), "index-new"),
        (status.is_index_modified(), "index-modified"),
        (status.is_index_deleted(), "index-deleted"),
        (status.is_wd_added(), "worktree-new"),
        (status.is_wd_deleted(), "worktree-deleted"),
        (status.is_wd_modified(), "worktree-modified"),
        (
            status.contains(SubmoduleStatus::WD_INDEX_MODIFIED),
            "worktree-modified",
        ),
        (status.is_wd_wd_modified(), "worktree-modified"),
        (status.is_wd_untracked(), "worktree-modified"),
    ] {
        if dirty && !result.contains(&name) {
            result.push(name);
        }
    }
    result
}

fn projected_path(prefix: &str, path: &[u8]) -> Result<(String, bool), HookError> {
    let full_len = prefix
        .len()
        .saturating_add(usize::from(!prefix.is_empty()))
        .saturating_add(path.len());
    if path.is_empty() || full_len > MAX_PATH_BYTES || path.contains(&0) {
        return Err(HookError::data(
            "workspace-inspection-path-invalid",
            "workspace status contains an invalid path",
        ));
    }
    let projected = String::from_utf8_lossy(path);
    let lossy = matches!(projected, std::borrow::Cow::Owned(_));
    let path = if prefix.is_empty() {
        projected.into_owned()
    } else {
        format!("{prefix}/{projected}")
    };
    Ok((path, lossy))
}

fn submodule_prefix(prefix: &str, path: &Path) -> Result<String, HookError> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_)
                    | Component::RootDir
                    | Component::ParentDir
                    | Component::CurDir
            )
        })
    {
        return Err(unavailable());
    }
    let relative = path.to_str().ok_or_else(unavailable)?;
    if relative.is_empty()
        || relative.as_bytes().contains(&0)
        || relative.chars().any(char::is_control)
    {
        return Err(unavailable());
    }
    let full = if prefix.is_empty() {
        relative.to_string()
    } else {
        format!("{prefix}/{relative}")
    };
    if full.len() > MAX_PATH_BYTES {
        return Err(HookError::data(
            "workspace-inspection-path-invalid",
            "workspace status contains an invalid path",
        ));
    }
    Ok(full)
}

fn push_entry(
    result: &mut Vec<DirtyEntry>,
    states: Vec<&'static str>,
    path: String,
    lossy: bool,
) -> Result<(), HookError> {
    if states.is_empty() {
        return Ok(());
    }
    if let Some(existing) = result.iter_mut().find(|entry| entry.path == path) {
        for state in states {
            if !existing.states.contains(&state) {
                existing.states.push(state);
            }
        }
        existing.lossy |= lossy;
        return Ok(());
    }
    if result.len() >= MAX_DIRTY_ENTRIES {
        return Err(HookError::data(
            "workspace-inspection-too-large",
            "workspace status exceeds the bounded recovery projection",
        ));
    }
    result.push(DirtyEntry {
        states,
        path,
        lossy,
    });
    Ok(())
}

fn collect_repository_status(
    repository: &Repository,
    prefix: &str,
    depth: usize,
    seen: &mut HashSet<PathBuf>,
    submodule_count: &mut usize,
    result: &mut Vec<DirtyEntry>,
) -> Result<(), HookError> {
    if depth > MAX_SUBMODULE_DEPTH {
        return Err(HookError::data(
            "workspace-inspection-too-large",
            "workspace submodule depth exceeds the bounded inspection",
        ));
    }
    let identity = fs::canonicalize(repository.path()).map_err(|_| unavailable())?;
    if !seen.insert(identity) {
        return Err(unavailable());
    }
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .include_unmodified(false)
        .exclude_submodules(true)
        .renames_head_to_index(false)
        .renames_index_to_workdir(false)
        .no_refresh(true)
        .update_index(false);
    let statuses = repository
        .statuses(Some(&mut options))
        .map_err(|_| unavailable())?;
    for entry in statuses.iter() {
        let status = states(entry.status());
        let (path, lossy) = projected_path(prefix, entry.path_bytes())?;
        push_entry(result, status, path, lossy)?;
    }
    let submodules = repository.submodules().map_err(|_| unavailable())?;
    for submodule in submodules {
        *submodule_count = submodule_count.saturating_add(1);
        if *submodule_count > MAX_SUBMODULES {
            return Err(HookError::data(
                "workspace-inspection-too-large",
                "workspace submodule count exceeds the bounded inspection",
            ));
        }
        let name = submodule.name().map_err(|_| unavailable())?;
        let path = submodule_prefix(prefix, submodule.path())?;
        let status = repository
            .submodule_status(name, SubmoduleIgnore::None)
            .map_err(|_| unavailable())?;
        push_entry(result, submodule_states(status), path.clone(), false)?;
        if status.is_in_wd() && !status.is_wd_uninitialized() {
            let nested = submodule.open().map_err(|_| unavailable())?;
            collect_repository_status(
                &nested,
                &path,
                depth.saturating_add(1),
                seen,
                submodule_count,
                result,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn dirty_entries(layout: &GitLayout) -> Result<Vec<DirtyEntry>, HookError> {
    let repository = Repository::open(&layout.root).map_err(|_| unavailable())?;
    let mut result = Vec::new();
    let mut submodule_count = 0;
    collect_repository_status(
        &repository,
        "",
        0,
        &mut HashSet::new(),
        &mut submodule_count,
        &mut result,
    )?;
    result.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(result)
}

pub(crate) fn checkout_dirty(layout: &GitLayout) -> Result<bool, HookError> {
    Ok(!dirty_entries(layout)?.is_empty())
}
