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

use git2::{Repository, Status, StatusOptions, Submodule};
use serde::Serialize;
use serde_json::json;

use crate::dsh_policy::GitLayout;
use crate::error::HookError;

const MAX_DIRTY_ENTRIES: usize = 2_048;
const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_SUBMODULES: usize = 512;
const MAX_SUBMODULE_DEPTH: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RepositoryIdentity {
    workdir: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
}

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

fn submodule_index_states(submodule: &Submodule<'_>) -> Vec<&'static str> {
    let mut result = Vec::new();
    match (submodule.head_id(), submodule.index_id()) {
        (None, Some(_)) => result.push("index-new"),
        (Some(_), None) => result.push("index-deleted"),
        (Some(head), Some(index)) if head != index => result.push("index-modified"),
        _ => {}
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

fn repository_identity(repository: &Repository) -> Result<RepositoryIdentity, HookError> {
    let workdir = repository
        .workdir()
        .and_then(|path| fs::canonicalize(path).ok())
        .ok_or_else(unavailable)?;
    let git_dir = fs::canonicalize(repository.path()).map_err(|_| unavailable())?;
    let common_dir = fs::canonicalize(repository.commondir()).map_err(|_| unavailable())?;
    Ok(RepositoryIdentity {
        workdir,
        git_dir,
        common_dir,
    })
}

fn canonical_descendant(root: &Path, relative: &Path) -> Result<PathBuf, HookError> {
    submodule_prefix("", relative)?;
    let root = fs::canonicalize(root).map_err(|_| unavailable())?;
    let mut candidate = root.clone();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(unavailable());
        };
        candidate.push(component);
        let metadata = fs::symlink_metadata(&candidate).map_err(|_| unavailable())?;
        if metadata.file_type().is_symlink() {
            return Err(unavailable());
        }
    }
    let candidate = fs::canonicalize(candidate).map_err(|_| unavailable())?;
    if !candidate.starts_with(&root) {
        return Err(unavailable());
    }
    Ok(candidate)
}

fn directory_nonempty(path: &Path) -> Result<bool, HookError> {
    let mut entries = fs::read_dir(path).map_err(|_| unavailable())?;
    entries
        .next()
        .transpose()
        .map_err(|_| unavailable())
        .map(|entry| entry.is_some())
}

fn reopened_identity(workdir: &Path) -> Result<RepositoryIdentity, HookError> {
    let repository = Repository::open(workdir).map_err(|_| unavailable())?;
    repository_identity(&repository)
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

struct InspectionState<'a, F, G> {
    seen: HashSet<PathBuf>,
    submodule_count: usize,
    result: &'a mut Vec<DirtyEntry>,
    before_submodule_status: &'a mut G,
    before_revalidate: &'a mut F,
}

fn collect_repository_status<F, G>(
    repository: &Repository,
    prefix: &str,
    depth: usize,
    state: &mut InspectionState<'_, F, G>,
) -> Result<(), HookError>
where
    F: FnMut(&Path) -> Result<(), HookError>,
    G: FnMut(&str) -> Result<(), HookError>,
{
    if depth > MAX_SUBMODULE_DEPTH {
        return Err(HookError::data(
            "workspace-inspection-too-large",
            "workspace submodule depth exceeds the bounded inspection",
        ));
    }
    let identity = repository_identity(repository)?;
    if !state.seen.insert(identity.git_dir.clone()) {
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
        push_entry(state.result, status, path, lossy)?;
    }
    let submodules = repository.submodules().map_err(|_| unavailable())?;
    for submodule in submodules {
        state.submodule_count = state.submodule_count.saturating_add(1);
        if state.submodule_count > MAX_SUBMODULES {
            return Err(HookError::data(
                "workspace-inspection-too-large",
                "workspace submodule count exceeds the bounded inspection",
            ));
        }
        let name = submodule.name().map_err(|_| unavailable())?;
        let path = submodule_prefix(prefix, submodule.path())?;
        let unresolved_workdir = identity.workdir.join(submodule.path());
        let expected_workdir = match fs::symlink_metadata(&unresolved_workdir) {
            Ok(_) => Some(canonical_descendant(&identity.workdir, submodule.path())?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(unavailable()),
        };
        let validated_nested = if let Some(expected_workdir) = expected_workdir.as_ref() {
            let git_marker = expected_workdir.join(".git");
            match fs::symlink_metadata(&git_marker) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        return Err(unavailable());
                    }
                    let expected_git_dir =
                        canonical_descendant(&identity.git_dir, &Path::new("modules").join(name))?;
                    let nested = submodule.open().map_err(|_| unavailable())?;
                    let nested_identity = repository_identity(&nested)?;
                    if nested_identity.workdir != *expected_workdir
                        || nested_identity.git_dir != expected_git_dir
                    {
                        return Err(unavailable());
                    }
                    Some((nested, nested_identity))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err(unavailable()),
            }
        } else {
            None
        };
        (state.before_submodule_status)(name)?;
        push_entry(
            state.result,
            submodule_index_states(&submodule),
            path.clone(),
            false,
        )?;
        if let Some((nested, nested_identity)) = validated_nested {
            let expected_workdir = expected_workdir.as_ref().ok_or_else(unavailable)?;
            let nested_head = nested
                .head()
                .ok()
                .and_then(|head| head.target())
                .ok_or_else(unavailable)?;
            match submodule.index_id() {
                Some(index) if index != nested_head => {
                    push_entry(state.result, vec!["worktree-modified"], path.clone(), false)?
                }
                None => push_entry(state.result, vec!["worktree-new"], path.clone(), false)?,
                _ => {}
            }
            let entries_before = state.result.len();
            collect_repository_status(&nested, &path, depth.saturating_add(1), state)?;
            if state.result.len() > entries_before {
                push_entry(state.result, vec!["worktree-modified"], path.clone(), false)?;
            }
            (state.before_revalidate)(expected_workdir)?;
            if reopened_identity(expected_workdir)? != nested_identity {
                return Err(unavailable());
            }
        } else {
            match (expected_workdir.as_ref(), submodule.index_id()) {
                (None, Some(_)) => {
                    push_entry(state.result, vec!["worktree-deleted"], path.clone(), false)?
                }
                (Some(workdir), index) if directory_nonempty(workdir)? => push_entry(
                    state.result,
                    vec![if index.is_some() {
                        "worktree-modified"
                    } else {
                        "worktree-new"
                    }],
                    path.clone(),
                    false,
                )?,
                _ => {}
            }
        }
    }
    (state.before_revalidate)(&identity.workdir)?;
    if reopened_identity(&identity.workdir)? != identity {
        return Err(unavailable());
    }
    Ok(())
}

fn dirty_entries_with_observers<F, G>(
    layout: &GitLayout,
    before_submodule_status: &mut G,
    before_revalidate: &mut F,
) -> Result<Vec<DirtyEntry>, HookError>
where
    F: FnMut(&Path) -> Result<(), HookError>,
    G: FnMut(&str) -> Result<(), HookError>,
{
    let repository = Repository::open(&layout.root).map_err(|_| unavailable())?;
    let mut result = Vec::new();
    let mut state = InspectionState {
        seen: HashSet::new(),
        submodule_count: 0,
        result: &mut result,
        before_submodule_status,
        before_revalidate,
    };
    collect_repository_status(&repository, "", 0, &mut state)?;
    result.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(result)
}

pub(crate) fn dirty_entries(layout: &GitLayout) -> Result<Vec<DirtyEntry>, HookError> {
    dirty_entries_with_observers(layout, &mut |_| Ok(()), &mut |_| Ok(()))
}

pub(crate) fn checkout_dirty(layout: &GitLayout) -> Result<bool, HookError> {
    Ok(!dirty_entries(layout)?.is_empty())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use git2::{IndexAddOption, Repository, Signature};
    use tempfile::TempDir;

    use super::{dirty_entries_with_observers, projected_path};
    use crate::dsh_policy::git_layout;

    fn committed_repository(path: &Path) {
        fs::create_dir_all(path).expect("repository directory");
        let repository = Repository::init(path).expect("repository init");
        fs::write(path.join("tracked.txt"), "base\n").expect("tracked file");
        let mut index = repository.index().expect("index");
        index
            .add_all(["tracked.txt"], IndexAddOption::DEFAULT, None)
            .expect("index add");
        index.write().expect("index write");
        let tree_id = index.write_tree().expect("write tree");
        let tree = repository.find_tree(tree_id).expect("tree");
        let signature =
            Signature::now("Workspace Test", "workspace@example.com").expect("signature");
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "test: initial",
                &tree,
                &[],
            )
            .expect("initial commit");
    }

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn path_projection_marks_invalid_utf8_without_a_filesystem_fixture() {
        let (path, lossy) = projected_path("nested", b"opaque-\xff.txt").expect("projection");

        assert_eq!(path, "nested/opaque-\u{fffd}.txt");
        assert!(lossy);
    }

    #[test]
    fn path_projection_preserves_valid_utf8() {
        let (path, lossy) = projected_path("nested", b"notes.txt").expect("projection");

        assert_eq!(path, "nested/notes.txt");
        assert!(!lossy);
    }

    #[test]
    fn current_checkout_identity_change_between_scan_and_revalidation_fails_closed() {
        let temp = TempDir::new().expect("temporary directory");
        let current = temp.path().join("current");
        let foreign = temp.path().join("foreign");
        committed_repository(&current);
        committed_repository(&foreign);
        fs::write(
            foreign.join("foreign-secret.txt"),
            "must not be projected\n",
        )
        .expect("foreign dirty file");
        let layout = git_layout(&current).expect("current layout");
        let canonical_current = fs::canonicalize(&current).expect("canonical current");
        let mut switched = false;

        let result = dirty_entries_with_observers(&layout, &mut |_| Ok(()), &mut |workdir| {
            if workdir == canonical_current && !switched {
                fs::rename(current.join(".git"), current.join(".git-before-switch"))
                    .expect("preserve original git directory");
                fs::write(
                    current.join(".git"),
                    format!("gitdir: {}\n", foreign.join(".git").display()),
                )
                .expect("redirect current checkout");
                switched = true;
            }
            Ok(())
        });

        assert!(switched, "phase-controlled identity switch was not reached");
        let error = result.expect_err("identity change must fail closed");
        assert_eq!(error.code, "workspace-inspection-unavailable");
        assert!(!error.message.contains("foreign-secret"));
    }

    #[test]
    fn redirected_submodule_identity_is_rejected_before_status_traversal() {
        let temp = TempDir::new().expect("temporary directory");
        let current = temp.path().join("current");
        let source = temp.path().join("source");
        let foreign = temp.path().join("foreign");
        committed_repository(&current);
        committed_repository(&source);
        committed_repository(&foreign);
        fs::write(foreign.join("tracked.txt"), "foreign head\n").expect("foreign tracked change");
        git(&foreign, &["add", "tracked.txt"]);
        git(
            &foreign,
            &[
                "-c",
                "user.email=workspace@example.com",
                "-c",
                "user.name=Workspace Test",
                "commit",
                "--quiet",
                "-m",
                "test: foreign head",
            ],
        );
        git(
            &current,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--quiet",
                source.to_str().expect("source UTF-8"),
                "module",
            ],
        );
        git(
            &current,
            &[
                "-c",
                "user.email=workspace@example.com",
                "-c",
                "user.name=Workspace Test",
                "commit",
                "--quiet",
                "-am",
                "test: add submodule",
            ],
        );
        fs::write(
            current.join("module/.git"),
            format!("gitdir: {}\n", foreign.join(".git").display()),
        )
        .expect("redirect submodule gitfile");
        let layout = git_layout(&current).expect("current layout");
        let mut status_attempted = false;

        let result = dirty_entries_with_observers(
            &layout,
            &mut |_| {
                status_attempted = true;
                Ok(())
            },
            &mut |_| Ok(()),
        );

        let error = result.expect_err("redirected submodule must fail closed");
        assert_eq!(error.code, "workspace-inspection-unavailable");
        assert!(
            !status_attempted,
            "redirected submodule reached status traversal before identity rejection"
        );
    }

    #[test]
    fn submodule_status_cannot_traverse_a_temporary_foreign_gitdir() {
        let temp = TempDir::new().expect("temporary directory");
        let current = temp.path().join("current");
        let source = temp.path().join("source");
        let foreign = temp.path().join("foreign");
        committed_repository(&current);
        committed_repository(&source);
        committed_repository(&foreign);
        fs::write(foreign.join("tracked.txt"), "foreign head\n").expect("foreign tracked change");
        git(&foreign, &["add", "tracked.txt"]);
        git(
            &foreign,
            &[
                "-c",
                "user.email=workspace@example.com",
                "-c",
                "user.name=Workspace Test",
                "commit",
                "--quiet",
                "-m",
                "test: foreign head",
            ],
        );
        git(
            &current,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--quiet",
                source.to_str().expect("source UTF-8"),
                "module",
            ],
        );
        git(
            &current,
            &[
                "-c",
                "user.email=workspace@example.com",
                "-c",
                "user.name=Workspace Test",
                "commit",
                "--quiet",
                "-am",
                "test: add submodule",
            ],
        );
        fs::write(
            foreign.join("foreign-secret.txt"),
            "must not affect trusted status\n",
        )
        .expect("foreign dirty file");
        let layout = git_layout(&current).expect("current layout");
        let gitfile = current.join("module/.git");
        let trusted_gitfile = temp.path().join("trusted-submodule-gitfile");
        let canonical_submodule =
            fs::canonicalize(current.join("module")).expect("canonical trusted submodule");
        let switched = Cell::new(false);
        let restored = Cell::new(false);

        let result = dirty_entries_with_observers(
            &layout,
            &mut |_| {
                fs::rename(&gitfile, &trusted_gitfile).expect("preserve trusted gitfile");
                fs::write(
                    &gitfile,
                    format!("gitdir: {}\n", foreign.join(".git").display()),
                )
                .expect("temporary foreign gitfile");
                switched.set(true);
                Ok(())
            },
            &mut |workdir| {
                if workdir == canonical_submodule && switched.get() && !restored.get() {
                    fs::remove_file(&gitfile).expect("remove temporary foreign gitfile");
                    fs::rename(&trusted_gitfile, &gitfile).expect("restore trusted gitfile");
                    restored.set(true);
                }
                Ok(())
            },
        );

        assert!(switched.get(), "status phase did not switch the gitfile");
        assert!(
            restored.get(),
            "revalidation phase did not restore the gitfile"
        );
        let entries = result.expect("trusted repository remains inspectable");
        assert!(
            entries.is_empty(),
            "foreign status affected result: {entries:?}"
        );
    }
}
