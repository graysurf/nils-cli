//! In-process, fail-closed operation-effect classification for timeout posture.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::model::{NormalizedRequest, OperationEffectClass};

pub(crate) fn classify(raw: &[u8], request: &NormalizedRequest) -> OperationEffectClass {
    if request.event != "PreToolUse" {
        return OperationEffectClass::Unknown;
    }
    if request.target_paths.iter().any(|path| sensitive(path)) {
        return OperationEffectClass::SensitiveConfiguration;
    }
    match request.matcher.as_deref() {
        Some("Write" | "Edit" | "NotebookEdit" | "MultiEdit" | "apply_patch") => {
            if local_targets(request) {
                OperationEffectClass::LocalReversible
            } else {
                OperationEffectClass::Unknown
            }
        }
        Some("Read" | "Glob" | "Grep") => OperationEffectClass::ReadOnly,
        Some("Bash") => classify_bash(raw, request),
        _ => OperationEffectClass::Unknown,
    }
}

fn classify_bash(raw: &[u8], request: &NormalizedRequest) -> OperationEffectClass {
    let value: Value = match serde_json::from_slice(raw) {
        Ok(value) => value,
        Err(_) => return OperationEffectClass::Unknown,
    };
    let Some(object) = value.as_object() else {
        return OperationEffectClass::Unknown;
    };
    let Some(command) = crate::adapter::command_text(object) else {
        return OperationEffectClass::Unknown;
    };
    let mut words = match crate::read_only::parse_simple_command(command) {
        Ok(words) => words,
        Err(_) => return OperationEffectClass::Unknown,
    };
    if words.first().map(String::as_str) == Some("builtin")
        && words.get(1).map(String::as_str) == Some("command")
    {
        words.drain(..2);
    }
    let Some(program) = words.first() else {
        return OperationEffectClass::Unknown;
    };
    let Some(name) = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return OperationEffectClass::Unknown;
    };
    if name == "forge-cli" && words.get(1).map(String::as_str) == Some("repo") {
        return OperationEffectClass::ExternalMutation;
    }
    if name == "git" {
        return match words.get(1).map(String::as_str) {
            Some("push") => OperationEffectClass::ExternalMutation,
            Some("reset" | "clean" | "update-ref" | "branch") => {
                OperationEffectClass::LocalDestructive
            }
            Some("status" | "diff" | "show" | "log" | "rev-parse") => {
                OperationEffectClass::ReadOnly
            }
            _ => OperationEffectClass::Unknown,
        };
    }
    if name != "semantic-commit" || !trusted_sibling(program) {
        return OperationEffectClass::Unknown;
    }
    match words.get(1).map(String::as_str) {
        Some("local-default") if valid_local_default(&words[2..], request) => {
            OperationEffectClass::LocalReversible
        }
        Some("commit" | "fixup" | "squash")
            if valid_feature_branch_commit(&words[2..], request) =>
        {
            OperationEffectClass::LocalReversible
        }
        Some("commit" | "fixup" | "squash") => OperationEffectClass::Unknown,
        Some("staged-context") => OperationEffectClass::ReadOnly,
        _ => OperationEffectClass::Unknown,
    }
}

fn valid_feature_branch_commit(args: &[String], request: &NormalizedRequest) -> bool {
    if args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--amend" | "--allow-empty" | "--no-edit" | "--message-only"
        )
    }) {
        return false;
    }
    let execution = option(args, "--repo")
        .map(PathBuf::from)
        .or_else(|| request.execution_path.clone());
    let Some(execution) = execution.and_then(|path| fs::canonicalize(path).ok()) else {
        return false;
    };
    let Some((git_dir, common_dir)) = git_directories(&execution) else {
        return false;
    };
    if git_dir == common_dir {
        return false;
    }
    let Some(current_branch) = symbolic_head(&git_dir.join("HEAD")) else {
        return false;
    };
    let Some(primary_branch) = symbolic_head(&common_dir.join("HEAD")) else {
        return false;
    };
    current_branch != primary_branch
}

fn git_directories(start: &Path) -> Option<(PathBuf, PathBuf)> {
    let worktree = start.ancestors().find(|path| path.join(".git").exists())?;
    let dot_git = worktree.join(".git");
    let git_dir = if dot_git.is_dir() {
        fs::canonicalize(dot_git).ok()?
    } else {
        let content = fs::read_to_string(dot_git).ok()?;
        let value = content.strip_prefix("gitdir:")?.trim();
        let path = PathBuf::from(value);
        fs::canonicalize(if path.is_absolute() {
            path
        } else {
            worktree.join(path)
        })
        .ok()?
    };
    let common_dir = match fs::read_to_string(git_dir.join("commondir")) {
        Ok(value) => {
            let path = PathBuf::from(value.trim());
            fs::canonicalize(if path.is_absolute() {
                path
            } else {
                git_dir.join(path)
            })
            .ok()?
        }
        Err(_) => git_dir.clone(),
    };
    Some((git_dir, common_dir))
}

fn symbolic_head(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()?
        .trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
}

fn valid_local_default(args: &[String], request: &NormalizedRequest) -> bool {
    if args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--amend" | "--allow-empty" | "--message-only" | "--no-edit"
        )
    }) {
        return false;
    }
    let Some(branch) = option(args, "--expected-branch") else {
        return false;
    };
    if branch.is_empty() || branch.len() > 255 {
        return false;
    }
    let Some(head) = option(args, "--expect-head") else {
        return false;
    };
    if !full_object_id(head) {
        return false;
    }
    let execution = option(args, "--repo")
        .map(PathBuf::from)
        .or_else(|| request.execution_path.clone());
    let Some(execution) = execution.and_then(|path| fs::canonicalize(path).ok()) else {
        return false;
    };
    let Some((git_dir, common_dir)) = git_directories(&execution) else {
        return false;
    };
    if git_dir != common_dir
        || symbolic_head(&git_dir.join("HEAD")).as_deref() != Some(branch)
        || resolve_head(&git_dir, branch).as_deref() != Some(head)
    {
        return false;
    }
    if option(args, "--remote-mode").is_some_and(|mode| mode != "local-only") {
        return false;
    }
    let Some(receipt) = option(args, "--receipt-out") else {
        return false;
    };
    let receipt = PathBuf::from(receipt);
    if !receipt.is_absolute() {
        return false;
    }
    let Some(parent) = receipt
        .parent()
        .and_then(|parent| fs::canonicalize(parent).ok())
    else {
        return false;
    };
    !parent.starts_with(execution)
}

fn resolve_head(git_dir: &Path, branch: &str) -> Option<String> {
    let reference = format!("refs/heads/{branch}");
    let loose = git_dir.join(&reference);
    if let Ok(value) = fs::read_to_string(loose) {
        let value = value.trim();
        return full_object_id(value).then(|| value.to_string());
    }
    let packed = fs::read_to_string(git_dir.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (object_id, name) = line.split_once(' ')?;
        (name == reference && full_object_id(object_id)).then(|| object_id.to_string())
    })
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let index = args.iter().position(|arg| arg == name)?;
    args.get(index + 1).map(String::as_str)
}

fn full_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn trusted_sibling(program: &str) -> bool {
    let candidate = if Path::new(program).components().count() > 1 {
        PathBuf::from(program)
    } else {
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        let Some(found) = std::env::split_paths(&path)
            .map(|directory| directory.join(program))
            .find(|path| path.is_file())
        else {
            return false;
        };
        found
    };
    let Ok(candidate) = fs::canonicalize(candidate) else {
        return false;
    };
    let Ok(current) = std::env::current_exe().and_then(fs::canonicalize) else {
        return false;
    };
    candidate.parent() == current.parent()
}

fn local_targets(request: &NormalizedRequest) -> bool {
    if request.target_paths.is_empty() || request.binding_roots.is_empty() {
        return false;
    }
    request.binding_roots.iter().any(|root| {
        fs::canonicalize(root).ok().is_some_and(|root| {
            request.target_paths.iter().all(|target| {
                target
                    .parent()
                    .and_then(|parent| fs::canonicalize(parent).ok())
                    .is_some_and(|parent| parent.starts_with(&root))
            })
        })
    })
}

fn sensitive(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some(
                ".git"
                    | ".codex"
                    | ".claude"
                    | ".agents"
                    | "hooks"
                    | "secrets"
                    | "credentials"
                    | "auth.json"
                    | "AGENTS.md"
            )
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_administrative_paths_are_sensitive() {
        assert!(sensitive(Path::new("/repo/.git/config")));
        assert!(sensitive(Path::new("/repo/.git/refs/heads/main")));
        assert!(!sensitive(Path::new("/repo/src/main.rs")));
    }
}
