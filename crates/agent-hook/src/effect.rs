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
        Some("default-branch") => match validated_default_branch_args(&words[2..], request) {
            Some(parsed) if parsed.dry_run => OperationEffectClass::ReadOnly,
            Some(_) => OperationEffectClass::LocalReversible,
            None => OperationEffectClass::Unknown,
        },
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

fn validated_default_branch_args<'a>(
    args: &'a [String],
    request: &NormalizedRequest,
) -> Option<DefaultBranchArgs<'a>> {
    let parsed = parse_default_branch_args(args)?;
    let head = parsed.expect_head;
    if !full_object_id(head) {
        return None;
    }
    let explicit_repo = parsed.repo.map(PathBuf::from);
    if explicit_repo
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return None;
    }
    let execution = explicit_repo.or_else(|| request.execution_path.clone());
    let execution = execution.and_then(|path| fs::canonicalize(path).ok())?;
    let worktree = execution
        .ancestors()
        .find(|path| path.join(".git").exists())
        .and_then(|path| fs::canonicalize(path).ok())?;
    let (git_dir, common_dir) = git_directories(&execution)?;
    if git_dir != common_dir {
        return None;
    }
    let branch = symbolic_head(&git_dir.join("HEAD"))?;
    if branch.is_empty()
        || branch.len() > 255
        || resolve_head(&git_dir, &branch).as_deref() != Some(head)
        || !authoritative_default_branch(&git_dir, &branch, head)
    {
        return None;
    }
    let valid = match (parsed.dry_run, parsed.receipt_out) {
        (true, None) => true,
        (true, Some(_)) | (false, None) => false,
        (false, Some(receipt)) => {
            let receipt = PathBuf::from(receipt);
            receipt.is_absolute()
                && receipt
                    .parent()
                    .and_then(|parent| fs::canonicalize(parent).ok())
                    .is_some_and(|parent| !parent.starts_with(worktree))
        }
    };
    if valid { Some(parsed) } else { None }
}

struct DefaultBranchArgs<'a> {
    expect_head: &'a str,
    repo: Option<&'a str>,
    receipt_out: Option<&'a str>,
    dry_run: bool,
}

fn parse_default_branch_args(args: &[String]) -> Option<DefaultBranchArgs<'_>> {
    let mut expect_head = None;
    let mut repo = None;
    let mut receipt_out = None;
    let mut format_seen = false;
    let mut dry_run = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--expect-head" => {
                bind_once(&mut expect_head, args.get(index + 1)?.as_str())?;
                index += 2;
            }
            "--repo" => {
                bind_once(&mut repo, args.get(index + 1)?.as_str())?;
                index += 2;
            }
            "--receipt-out" => {
                bind_once(&mut receipt_out, args.get(index + 1)?.as_str())?;
                index += 2;
            }
            "--format" => {
                if format_seen {
                    return None;
                }
                let value = args.get(index + 1)?.as_str();
                if !matches!(value, "text" | "json") {
                    return None;
                }
                format_seen = true;
                index += 2;
            }
            "--json" => {
                if format_seen {
                    return None;
                }
                format_seen = true;
                index += 1;
            }
            "--message" | "-m" | "--message-file" | "-F" | "--trailer" | "--type" | "--scope"
            | "--subject" | "--body-bullet" | "--bullet" | "--max-header-width" => {
                args.get(index + 1)?;
                index += 2;
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            "--automation" | "--non-interactive" | "--auto-fix" | "--signoff" => {
                index += 1;
            }
            _ => return None,
        }
    }

    Some(DefaultBranchArgs {
        expect_head: expect_head?,
        repo,
        receipt_out,
        dry_run,
    })
}

fn bind_once<'a>(slot: &mut Option<&'a str>, value: &'a str) -> Option<()> {
    if slot.is_some() {
        return None;
    }
    *slot = Some(value);
    Some(())
}

fn resolve_head(git_dir: &Path, branch: &str) -> Option<String> {
    let reference = format!("refs/heads/{branch}");
    resolve_reference(git_dir, &reference)
}

fn authoritative_default_branch(git_dir: &Path, branch: &str, head: &str) -> bool {
    let Some(config) = fs::read_to_string(git_dir.join("config")).ok() else {
        return false;
    };
    let mut section = String::new();
    let mut remotes = Vec::new();
    let mut branch_remote = None;
    let mut branch_merge = None;
    for raw in config.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_string();
            if let Some(name) = quoted_subsection(&section, "remote")
                && !remotes.iter().any(|remote| remote == name)
            {
                remotes.push(name.to_string());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if quoted_subsection(&section, "branch") == Some(branch) {
            match key.trim() {
                "remote" => branch_remote = Some(value.trim().to_string()),
                "merge" => branch_merge = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    if remotes.is_empty() {
        return branch_remote.is_none() && branch_merge.is_none();
    }
    let Some(remote) = branch_remote
        .filter(|value| value != "." && remotes.iter().any(|candidate| candidate == value))
    else {
        return false;
    };
    let expected_merge = format!("refs/heads/{branch}");
    if branch_merge.as_deref() != Some(expected_merge.as_str()) {
        return false;
    }
    let expected_reference = format!("refs/remotes/{remote}/{branch}");
    let cached_head = git_dir.join(format!("refs/remotes/{remote}/HEAD"));
    if symbolic_reference(&cached_head).as_deref() != Some(expected_reference.as_str()) {
        return false;
    }
    resolve_reference(git_dir, &expected_reference).as_deref() == Some(head)
}

fn quoted_subsection<'a>(section: &'a str, kind: &str) -> Option<&'a str> {
    let rest = section.strip_prefix(kind)?.trim_start();
    rest.strip_prefix('"')?.strip_suffix('"')
}

fn symbolic_reference(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()?
        .trim()
        .strip_prefix("ref: ")
        .map(str::to_string)
}

fn resolve_reference(git_dir: &Path, reference: &str) -> Option<String> {
    let loose = git_dir.join(reference);
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
    use crate::model::{Product, REQUEST_VERSION};
    use serde_json::json;

    struct RemoveOnDrop(PathBuf);

    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn request_for(path: &Path) -> NormalizedRequest {
        NormalizedRequest {
            schema_version: REQUEST_VERSION.to_string(),
            request_id: "request".to_string(),
            product: Product::Codex,
            event: "PreToolUse".to_string(),
            matcher: Some("Bash".to_string()),
            target_digest: "sha256:target".to_string(),
            command_digest: "sha256:command".to_string(),
            snapshot_digest: "sha256:snapshot".to_string(),
            worktree_fingerprint: None,
            semantic_conflict: None,
            stop_reentry: None,
            target_paths: Vec::new(),
            execution_path: Some(path.to_path_buf()),
            binding_roots: Vec::new(),
        }
    }

    fn classify_command(command: &str, request: &NormalizedRequest) -> OperationEffectClass {
        let raw = serde_json::to_vec(&json!({
            "tool_input": {
                "command": command,
            },
        }))
        .expect("provider payload");
        classify(&raw, request)
    }

    fn default_branch_fixture() -> (tempfile::TempDir, String) {
        let directory = tempfile::tempdir().expect("temporary repository");
        let git_dir = directory.path().join(".git");
        fs::create_dir_all(git_dir.join("refs/heads")).expect("create local refs");
        fs::create_dir_all(git_dir.join("refs/remotes/origin")).expect("create remote refs");
        let head = "a".repeat(40);
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
        fs::write(git_dir.join("refs/heads/main"), format!("{head}\n")).expect("write branch ref");
        fs::write(
            git_dir.join("refs/remotes/origin/HEAD"),
            "ref: refs/remotes/origin/main\n",
        )
        .expect("write cached remote HEAD");
        fs::write(
            git_dir.join("refs/remotes/origin/main"),
            format!("{head}\n"),
        )
        .expect("write cached upstream");
        fs::write(
            git_dir.join("config"),
            "[remote \"origin\"]\n\turl = local\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .expect("write config");
        (directory, head)
    }

    #[test]
    fn git_administrative_paths_are_sensitive() {
        assert!(sensitive(Path::new("/repo/.git/config")));
        assert!(sensitive(Path::new("/repo/.git/refs/heads/main")));
        assert!(!sensitive(Path::new("/repo/src/main.rs")));
    }

    #[test]
    fn exact_default_branch_form_replaces_local_default_admission() {
        let (directory, head) = default_branch_fixture();
        let receipt = directory
            .path()
            .parent()
            .expect("temp parent")
            .join("receipt.json");
        let request = request_for(directory.path());
        let new_args = vec![
            "--expect-head".to_string(),
            head.clone(),
            "--receipt-out".to_string(),
            receipt.to_string_lossy().into_owned(),
        ];
        assert!(
            validated_default_branch_args(&new_args, &request).is_some(),
            "exact new form should be admitted"
        );

        let old_args = vec![
            "--expected-branch".to_string(),
            "main".to_string(),
            "--expect-head".to_string(),
            head,
            "--receipt-out".to_string(),
            receipt.to_string_lossy().into_owned(),
        ];
        assert!(
            validated_default_branch_args(&old_args, &request).is_none(),
            "removed form must not be admitted"
        );
    }

    #[test]
    fn classifier_observes_the_complete_default_branch_admission_boundary() {
        let (directory, head) = default_branch_fixture();
        let request = request_for(directory.path());
        let receipt_dir = tempfile::tempdir().expect("receipt directory");
        let receipt = receipt_dir.path().join("receipt.json");
        let other_receipt = receipt_dir.path().join("other.json");
        let in_worktree_receipt = directory.path().join("receipt.json");
        let other_repo = tempfile::tempdir().expect("other repository");

        let current = std::env::current_exe().expect("current test executable");
        let trusted = current
            .parent()
            .expect("test executable parent")
            .join("semantic-commit");
        assert!(!trusted.exists(), "test sibling unexpectedly exists");
        fs::write(&trusted, b"test sibling").expect("trusted sibling fixture");
        let _trusted_cleanup = RemoveOnDrop(trusted.clone());

        let untrusted_root = tempfile::tempdir().expect("untrusted executable root");
        let untrusted = untrusted_root.path().join("semantic-commit");
        fs::write(&untrusted, b"untrusted fixture").expect("untrusted executable fixture");

        let base = format!("{} default-branch --expect-head {head}", trusted.display());
        let mutation = format!(
            "{base} --receipt-out {} --message 'docs: test'",
            receipt.display()
        );
        let cases = [
            (
                "mutation",
                mutation.clone(),
                OperationEffectClass::LocalReversible,
            ),
            (
                "dry-run token consumed as subject value",
                format!(
                    "{base} --type docs --subject --dry-run --receipt-out {}",
                    receipt.display()
                ),
                OperationEffectClass::LocalReversible,
            ),
            (
                "dry-run token consumed as message value",
                format!(
                    "{base} --message --dry-run --receipt-out {}",
                    receipt.display()
                ),
                OperationEffectClass::LocalReversible,
            ),
            (
                "dry-run token consumed as body bullet value",
                format!(
                    "{base} --type docs --subject test --body-bullet --dry-run --receipt-out {}",
                    receipt.display()
                ),
                OperationEffectClass::LocalReversible,
            ),
            (
                "dry-run",
                format!("{base} --message 'docs: test' --dry-run"),
                OperationEffectClass::ReadOnly,
            ),
            (
                "dry-run token consumed as invalid format value",
                format!(
                    "{base} --message 'docs: test' --format --dry-run --receipt-out {}",
                    receipt.display()
                ),
                OperationEffectClass::Unknown,
            ),
            (
                "old command",
                format!("{} local-default --expect-head {head}", trusted.display()),
                OperationEffectClass::Unknown,
            ),
            (
                "duplicate repo",
                format!(
                    "{mutation} --repo {} --repo {}",
                    directory.path().display(),
                    other_repo.path().display()
                ),
                OperationEffectClass::Unknown,
            ),
            (
                "duplicate expect-head",
                format!("{mutation} --expect-head {head}"),
                OperationEffectClass::Unknown,
            ),
            (
                "duplicate receipt-out",
                format!("{mutation} --receipt-out {}", other_receipt.display()),
                OperationEffectClass::Unknown,
            ),
            (
                "duplicate format",
                format!("{mutation} --format text --format json"),
                OperationEffectClass::Unknown,
            ),
            (
                "message-out",
                format!(
                    "{base} --message 'docs: test' --message-out {} --dry-run",
                    other_receipt.display()
                ),
                OperationEffectClass::Unknown,
            ),
            (
                "missing receipt",
                format!("{base} --message 'docs: test'"),
                OperationEffectClass::Unknown,
            ),
            (
                "relative receipt",
                format!("{base} --message 'docs: test' --receipt-out receipt.json"),
                OperationEffectClass::Unknown,
            ),
            (
                "in-worktree receipt",
                format!(
                    "{base} --message 'docs: test' --receipt-out {}",
                    in_worktree_receipt.display()
                ),
                OperationEffectClass::Unknown,
            ),
            (
                "invalid expected head",
                format!(
                    "{} default-branch --expect-head invalid --receipt-out {} --message 'docs: test'",
                    trusted.display(),
                    receipt.display()
                ),
                OperationEffectClass::Unknown,
            ),
            (
                "untrusted executable",
                format!(
                    "{} default-branch --expect-head {head} --receipt-out {} --message 'docs: test'",
                    untrusted.display(),
                    receipt.display()
                ),
                OperationEffectClass::Unknown,
            ),
            (
                "malformed composition",
                format!("{mutation} && true"),
                OperationEffectClass::Unknown,
            ),
            (
                "unknown option",
                format!("{mutation} --not-a-real-option"),
                OperationEffectClass::Unknown,
            ),
        ];

        for (label, command, expected) in cases {
            assert_eq!(
                classify_command(&command, &request),
                expected,
                "{label}: {command}"
            );
        }
    }
}
