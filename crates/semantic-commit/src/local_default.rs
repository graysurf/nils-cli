use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use nils_common::execution_effect::digest_parts;
use nils_common::local_default_receipt::{
    LocalDefaultCompletion, LocalDefaultData, LocalDefaultReceipt, LocalDefaultRemote,
    SCHEMA_VERSION,
};

use crate::commit;

const EXIT_ERROR: i32 = 1;
const EXIT_USAGE: i32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug)]
struct Options {
    expected_branch: String,
    expect_head: String,
    receipt_out: Option<PathBuf>,
    remote_mode: Option<String>,
    repo: Option<PathBuf>,
    dry_run: bool,
    validate_only: bool,
    output_format: OutputFormat,
    forwarded: Vec<String>,
}

#[derive(Debug)]
struct RepositoryState {
    root: PathBuf,
    branch: String,
    head: String,
    staged_file_count: usize,
    remote_count: usize,
    upstream: Option<String>,
    upstream_sha: Option<String>,
    ahead_before: usize,
    relation_before: String,
    fingerprint: String,
}

pub fn run(args: &[String]) -> i32 {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    let state = match preflight(&options) {
        Ok(state) => state,
        Err(message) => return fail(&message),
    };

    let mut forwarded = options.forwarded.clone();
    ensure_flag(&mut forwarded, "--require-clean");
    ensure_pair(&mut forwarded, "--expect-head", &state.head);
    ensure_flag(&mut forwarded, "--no-summary");
    ensure_flag(&mut forwarded, "--no-progress");

    if options.validate_only {
        ensure_flag(&mut forwarded, "--validate-only");
        let code = commit::run(&forwarded);
        if code != 0 {
            return code;
        }
        return emit(
            &receipt_for_read_only(&state, &options, false),
            options.output_format,
        );
    }

    if options.dry_run {
        ensure_flag(&mut forwarded, "--dry-run");
        let code = commit::run(&forwarded);
        if code != 0 {
            return code;
        }
        return emit(
            &receipt_for_read_only(&state, &options, true),
            options.output_format,
        );
    }

    let code = commit::run_forced_signing(&forwarded);
    if code != 0 {
        return code;
    }

    let new_head = match git_stdout(&state.root, ["rev-parse", "--verify", "HEAD^{commit}"]) {
        Ok(value) => value,
        Err(message) => return partial_failure(None, &message),
    };
    let post = match verify_postconditions(&state, &options, &new_head) {
        Ok(post) => post,
        Err(message) => return partial_failure(Some(&new_head), &message),
    };
    let receipt = LocalDefaultReceipt {
        schema_version: SCHEMA_VERSION.to_string(),
        ok: true,
        data: LocalDefaultData {
            mode: "local-default".to_string(),
            repository_fingerprint: state.fingerprint.clone(),
            branch: state.branch.clone(),
            old_head: state.head.clone(),
            new_head: new_head.clone(),
            parent_sha: post.parent,
            tree_sha: post.tree,
            signature: "verified-good".to_string(),
            staged_file_count: state.staged_file_count,
            remote: LocalDefaultRemote {
                configured_count: state.remote_count,
                mode: remote_mode_label(state.remote_count).to_string(),
                network_observed: false,
                provider_mutated: false,
                upstream: state.upstream.clone(),
                cached_relation_before: state.relation_before.clone(),
                cached_relation_after: post.relation_after,
            },
            completion: LocalDefaultCompletion {
                local_default_committed: true,
                provider_delivered: false,
                provider_reconciliation_required: state.remote_count > 0,
            },
        },
    };
    let receipt_out = options
        .receipt_out
        .as_deref()
        .expect("mutating local-default requires a receipt path");
    if let Err(message) = write_receipt(receipt_out, &receipt) {
        return partial_failure(Some(&new_head), &message);
    }
    emit(&receipt, options.output_format)
}

fn parse_args(args: &[String]) -> Result<Options, i32> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        print_usage(false);
        return Err(0);
    }
    for prohibited in ["--amend", "--no-edit", "--message-only", "--allow-empty"] {
        if args.iter().any(|arg| arg == prohibited) {
            eprintln!("error: {prohibited} is not supported by local-default");
            return Err(EXIT_USAGE);
        }
    }

    let mut expected_branch = None;
    let mut expect_head = None;
    let mut receipt_out = None;
    let mut remote_mode = None;
    let mut repo = None;
    let mut dry_run = false;
    let mut validate_only = false;
    let mut output_format = OutputFormat::Text;
    let mut forwarded = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--expected-branch" => {
                expected_branch = Some(required_value(args, index, "--expected-branch")?);
                index += 2;
            }
            "--expect-head" => {
                let value = required_value(args, index, "--expect-head")?;
                expect_head = Some(value.clone());
                forwarded.extend(["--expect-head".to_string(), value]);
                index += 2;
            }
            "--receipt-out" => {
                receipt_out = Some(PathBuf::from(required_value(args, index, "--receipt-out")?));
                index += 2;
            }
            "--remote-mode" => {
                remote_mode = Some(required_value(args, index, "--remote-mode")?);
                index += 2;
            }
            "--repo" => {
                let value = required_value(args, index, "--repo")?;
                repo = Some(PathBuf::from(&value));
                forwarded.extend(["--repo".to_string(), value]);
                index += 2;
            }
            "--format" => {
                let value = required_value(args, index, "--format")?;
                output_format = match value.as_str() {
                    "text" => OutputFormat::Text,
                    "json" => OutputFormat::Json,
                    _ => {
                        eprintln!("error: invalid --format value: {value} (expected: text, json)");
                        return Err(EXIT_USAGE);
                    }
                };
                index += 2;
            }
            "--json" => {
                output_format = OutputFormat::Json;
                index += 1;
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            "--validate-only" => {
                validate_only = true;
                index += 1;
            }
            other => {
                forwarded.push(other.to_string());
                if option_takes_value(other) {
                    let value = required_value(args, index, other)?;
                    forwarded.push(value);
                    index += 2;
                } else {
                    index += 1;
                }
            }
        }
    }
    if dry_run && validate_only {
        eprintln!("error: use only one of --dry-run or --validate-only");
        return Err(EXIT_USAGE);
    }
    let expected_branch = required(expected_branch, "--expected-branch")?;
    let expect_head = required(expect_head, "--expect-head")?;
    let receipt_out = if dry_run || validate_only {
        receipt_out
    } else {
        Some(required(receipt_out, "--receipt-out")?)
    };
    Ok(Options {
        expected_branch,
        expect_head,
        receipt_out,
        remote_mode,
        repo,
        dry_run,
        validate_only,
        output_format,
        forwarded,
    })
}

fn option_takes_value(option: &str) -> bool {
    matches!(
        option,
        "--message"
            | "-m"
            | "--message-file"
            | "-F"
            | "--message-out"
            | "--summary"
            | "--trailer"
            | "--type"
            | "--scope"
            | "--subject"
            | "--body-bullet"
            | "--bullet"
            | "--max-header-width"
    )
}

fn required_value(args: &[String], index: usize, option: &str) -> Result<String, i32> {
    args.get(index + 1).cloned().ok_or_else(|| {
        eprintln!("error: {option} requires a value");
        EXIT_USAGE
    })
}

fn required<T>(value: Option<T>, option: &str) -> Result<T, i32> {
    value.ok_or_else(|| {
        eprintln!("error: {option} is required for local-default");
        EXIT_USAGE
    })
}

fn preflight(options: &Options) -> Result<RepositoryState, String> {
    if !valid_object_id(&options.expect_head) {
        return Err("--expect-head must be a full lowercase object ID".to_string());
    }
    let start = options
        .repo
        .clone()
        .unwrap_or(std::env::current_dir().map_err(|error| error.to_string())?);
    let root = PathBuf::from(git_stdout(&start, ["rev-parse", "--show-toplevel"])?);
    let root = fs::canonicalize(&root)
        .map_err(|error| format!("failed to canonicalize repository root: {error}"))?;
    if git_stdout(&root, ["rev-parse", "--is-bare-repository"])? != "false" {
        return Err("local-default requires a non-bare repository".to_string());
    }
    verify_primary_worktree(&root)?;
    let branch = git_stdout(&root, ["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map_err(|_| "local-default requires an attached HEAD".to_string())?;
    if branch != options.expected_branch {
        return Err(format!(
            "checked-out branch mismatch (expected {}, found {branch})",
            options.expected_branch
        ));
    }
    run_git(&root, ["check-ref-format", "--branch", branch.as_str()])?;
    let head = git_stdout(&root, ["rev-parse", "--verify", "HEAD^{commit}"])?;
    if head != options.expect_head {
        return Err(format!(
            "HEAD mismatch (expected {}, found {head})",
            options.expect_head
        ));
    }
    verify_no_git_operation(&root)?;
    let staged = run_git(
        &root,
        [
            "diff",
            "--cached",
            "--name-only",
            "-z",
            "--diff-filter=ACDMRTUXB",
        ],
    )?;
    let staged_file_count = staged
        .stdout
        .split(|byte| *byte == 0)
        .filter(|p| !p.is_empty())
        .count();
    if staged_file_count == 0 {
        return Err("no staged changes (stage files with git add first)".to_string());
    }
    let status = git_stdout_allow_empty(&root, ["status", "--porcelain", "--untracked-files=all"])?;
    if status.lines().any(|line| {
        line.starts_with("??") || line.as_bytes().get(1).is_some_and(|byte| *byte != b' ')
    }) {
        return Err("unstaged or untracked changes present".to_string());
    }
    if let Some(receipt_out) = options.receipt_out.as_deref() {
        verify_receipt_destination(&root, receipt_out)?;
    }

    let remotes = git_stdout_allow_empty(&root, ["remote"])?;
    let remote_count = remotes
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    match (remote_count, options.remote_mode.as_deref()) {
        (0, None) => {}
        (0, Some(_)) => {
            return Err("--remote-mode is not accepted when no remotes are configured".to_string());
        }
        (_, Some("local-only")) => {}
        _ => {
            return Err(
                "configured remotes require exact --remote-mode local-only acknowledgement"
                    .to_string(),
            );
        }
    }
    let (upstream, upstream_sha, ahead_before, relation_before) =
        resolve_upstream(&root, &branch, &head)?;
    let common = git_stdout(&root, ["rev-parse", "--git-common-dir"])?;
    let common = canonical_git_path(&root, &common)?;
    let object_format = git_stdout(&root, ["rev-parse", "--show-object-format"])?;
    let fingerprint = digest_parts([
        common.as_os_str().as_encoded_bytes(),
        object_format.as_bytes(),
    ]);
    Ok(RepositoryState {
        root,
        branch,
        head,
        staged_file_count,
        remote_count,
        upstream,
        upstream_sha,
        ahead_before,
        relation_before,
        fingerprint,
    })
}

fn verify_primary_worktree(root: &Path) -> Result<(), String> {
    let listing = git_stdout(root, ["worktree", "list", "--porcelain"])?;
    let primary = listing
        .lines()
        .find_map(|line| line.strip_prefix("worktree "))
        .ok_or_else(|| "failed to resolve primary worktree".to_string())?;
    let primary = fs::canonicalize(primary)
        .map_err(|error| format!("failed to canonicalize primary worktree: {error}"))?;
    if primary != root {
        return Err("local-default may run only in the repository primary worktree".to_string());
    }
    Ok(())
}

fn verify_no_git_operation(root: &Path) -> Result<(), String> {
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "BISECT_LOG",
        "rebase-merge",
        "rebase-apply",
    ] {
        let path = git_stdout(root, ["rev-parse", "--git-path", marker])?;
        if canonical_git_path_unchecked(root, &path).exists() {
            return Err(format!("Git operation in progress ({marker})"));
        }
    }
    Ok(())
}

fn verify_receipt_destination(root: &Path, path: &Path) -> Result<(), String> {
    if fs::symlink_metadata(path).is_ok() {
        return Err("--receipt-out must name a new non-symlink file".to_string());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "--receipt-out must have an existing parent directory".to_string())?;
    if fs::symlink_metadata(parent)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err("--receipt-out parent must not be a symlink".to_string());
    }
    let parent = fs::canonicalize(parent)
        .map_err(|error| format!("failed to resolve --receipt-out parent: {error}"))?;
    if !parent.is_dir() {
        return Err("--receipt-out parent is not a directory".to_string());
    }
    if parent.starts_with(root) {
        return Err("--receipt-out must be outside the repository worktree".to_string());
    }
    tempfile::NamedTempFile::new_in(&parent)
        .map_err(|error| format!("--receipt-out parent is not writable: {error}"))?;
    Ok(())
}

fn resolve_upstream(
    root: &Path,
    branch: &str,
    head: &str,
) -> Result<(Option<String>, Option<String>, usize, String), String> {
    match run_git(
        root,
        [
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    ) {
        Ok(output) => {
            let upstream = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let upstream_sha = git_stdout(root, ["rev-parse", "--verify", "@{upstream}^{commit}"])
                .map_err(|_| "configured upstream cached ref cannot be resolved".to_string())?;
            let (behind, ahead) = cached_relation_counts(root, &upstream_sha, head)?;
            if behind > 0 {
                let relation = if ahead > 0 { "diverged" } else { "behind" };
                return Err(format!(
                    "current HEAD is {relation} relative to the cached upstream"
                ));
            }
            Ok((
                Some(upstream),
                Some(upstream_sha),
                ahead,
                cached_relation_label(ahead),
            ))
        }
        Err(_) => {
            let remote = git_config_optional(root, &format!("branch.{branch}.remote"))?;
            let merge = git_config_optional(root, &format!("branch.{branch}.merge"))?;
            if remote.is_some() || merge.is_some() {
                return Err("configured upstream cached ref cannot be resolved".to_string());
            }
            Ok((None, None, 0, "untracked".to_string()))
        }
    }
}

fn cached_relation_counts(
    root: &Path,
    upstream_sha: &str,
    head: &str,
) -> Result<(usize, usize), String> {
    let range = format!("{upstream_sha}...{head}");
    let counts = git_stdout(
        root,
        ["rev-list", "--left-right", "--count", range.as_str()],
    )?;
    let mut values = counts.split_whitespace();
    let behind = values
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| "failed to parse cached upstream behind count".to_string())?;
    let ahead = values
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| "failed to parse cached upstream ahead count".to_string())?;
    if values.next().is_some() {
        return Err("failed to parse cached upstream relation counts".to_string());
    }
    Ok((behind, ahead))
}

fn cached_relation_label(ahead: usize) -> String {
    match ahead {
        0 => "aligned".to_string(),
        1 => "ahead-by-one".to_string(),
        count => format!("ahead-by-{count}"),
    }
}

struct Postconditions {
    parent: String,
    tree: String,
    relation_after: String,
}

fn verify_postconditions(
    state: &RepositoryState,
    options: &Options,
    new_head: &str,
) -> Result<Postconditions, String> {
    let branch = git_stdout(&state.root, ["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    if branch != options.expected_branch {
        return Err("branch changed after commit".to_string());
    }
    let parent = git_stdout(&state.root, ["rev-parse", "--verify", "HEAD^1^{commit}"])?;
    if parent != state.head {
        return Err("created commit parent does not match --expect-head".to_string());
    }
    let parent_count = git_stdout(&state.root, ["rev-list", "--parents", "-n", "1", "HEAD"])?
        .split_whitespace()
        .count()
        .saturating_sub(1);
    if parent_count != 1 {
        return Err("created commit must have exactly one parent".to_string());
    }
    if git_stdout(&state.root, ["rev-parse", "--verify", "HEAD^{commit}"])? != new_head {
        return Err("HEAD changed during postcondition verification".to_string());
    }
    let status = git_stdout_allow_empty(
        &state.root,
        ["status", "--porcelain", "--untracked-files=all"],
    )?;
    if !status.is_empty() {
        return Err("worktree or index is not clean after commit".to_string());
    }
    run_git(&state.root, ["verify-commit", "HEAD"])
        .map_err(|_| "created commit signature verification failed".to_string())?;
    if git_stdout(&state.root, ["log", "-1", "--format=%G?", "HEAD"])? != "G" {
        return Err("created commit signature is not locally verified-good".to_string());
    }
    let relation_after = verify_cached_relation_after(state, new_head)?;
    let tree = git_stdout(&state.root, ["rev-parse", "--verify", "HEAD^{tree}"])?;
    Ok(Postconditions {
        parent,
        tree,
        relation_after,
    })
}

fn verify_cached_relation_after(state: &RepositoryState, new_head: &str) -> Result<String, String> {
    match state.upstream.as_deref() {
        Some(expected_upstream) => {
            let upstream = git_stdout(
                &state.root,
                [
                    "rev-parse",
                    "--abbrev-ref",
                    "--symbolic-full-name",
                    "@{upstream}",
                ],
            )?;
            if upstream != expected_upstream {
                return Err("configured upstream changed after commit".to_string());
            }
            let upstream_sha = git_stdout(
                &state.root,
                ["rev-parse", "--verify", "@{upstream}^{commit}"],
            )?;
            if Some(upstream_sha.as_str()) != state.upstream_sha.as_deref() {
                return Err("cached upstream changed after commit".to_string());
            }
            let (behind, ahead) = cached_relation_counts(&state.root, &upstream_sha, new_head)?;
            let expected_ahead = state
                .ahead_before
                .checked_add(1)
                .ok_or_else(|| "cached upstream ahead count overflowed".to_string())?;
            if behind != 0 || ahead != expected_ahead {
                let expected_relation = cached_relation_label(expected_ahead);
                return Err(format!(
                    "cached upstream relation after commit is not the expected {expected_relation}"
                ));
            }
            Ok(cached_relation_label(ahead))
        }
        None => {
            let remote =
                git_config_optional(&state.root, &format!("branch.{}.remote", state.branch))?;
            let merge =
                git_config_optional(&state.root, &format!("branch.{}.merge", state.branch))?;
            if remote.is_some() || merge.is_some() {
                return Err("configured upstream appeared after commit".to_string());
            }
            Ok("untracked".to_string())
        }
    }
}

fn receipt_for_read_only(
    state: &RepositoryState,
    _options: &Options,
    dry_run: bool,
) -> LocalDefaultReceipt {
    let tree = git_stdout(&state.root, ["rev-parse", "--verify", "HEAD^{tree}"])
        .unwrap_or_else(|_| state.head.clone());
    LocalDefaultReceipt {
        schema_version: SCHEMA_VERSION.to_string(),
        ok: true,
        data: LocalDefaultData {
            mode: "local-default".to_string(),
            repository_fingerprint: state.fingerprint.clone(),
            branch: state.branch.clone(),
            old_head: state.head.clone(),
            new_head: state.head.clone(),
            parent_sha: state.head.clone(),
            tree_sha: tree,
            signature: if dry_run { "pending" } else { "validated" }.to_string(),
            staged_file_count: state.staged_file_count,
            remote: LocalDefaultRemote {
                configured_count: state.remote_count,
                mode: remote_mode_label(state.remote_count).to_string(),
                network_observed: false,
                provider_mutated: false,
                upstream: state.upstream.clone(),
                cached_relation_before: state.relation_before.clone(),
                cached_relation_after: state.relation_before.clone(),
            },
            completion: LocalDefaultCompletion {
                local_default_committed: false,
                provider_delivered: false,
                provider_reconciliation_required: state.remote_count > 0,
            },
        },
    }
}

fn write_receipt(path: &Path, receipt: &LocalDefaultReceipt) -> Result<(), String> {
    let parent = path.parent().expect("receipt parent preflighted");
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("failed to allocate receipt temp file: {error}"))?;
    serde_json::to_writer_pretty(temp.as_file_mut(), receipt)
        .map_err(|error| format!("failed to serialize local-default receipt: {error}"))?;
    temp.as_file_mut()
        .write_all(b"\n")
        .map_err(|error| format!("failed to finalize local-default receipt: {error}"))?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|error| format!("failed to sync local-default receipt: {error}"))?;
    temp.persist_noclobber(path).map_err(|error| {
        format!("failed to create local-default receipt without overwrite: {error}")
    })?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("failed to sync local-default receipt directory: {error}"))?;
    Ok(())
}

fn emit(receipt: &LocalDefaultReceipt, format: OutputFormat) -> i32 {
    match format {
        OutputFormat::Json => match serde_json::to_string(receipt) {
            Ok(json) => println!("{json}"),
            Err(error) => return fail(&format!("failed to render local-default result: {error}")),
        },
        OutputFormat::Text => {
            if receipt.data.completion.local_default_committed {
                println!("local-default committed {}", receipt.data.new_head);
            } else {
                println!("local-default contract validated");
            }
        }
    }
    0
}

fn run_git<I, S>(root: &Path, args: I) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .output()
        .map_err(|error| format!("failed to launch git: {error}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            "git command failed".to_string()
        } else {
            stderr
        })
    }
}

fn git_stdout<I, S>(root: &Path, args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_git(root, args)?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        Err("git command returned empty output".to_string())
    } else {
        Ok(value)
    }
}

fn git_stdout_allow_empty<I, S>(root: &Path, args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Ok(String::from_utf8_lossy(&run_git(root, args)?.stdout)
        .trim()
        .to_string())
}

fn git_config_optional(root: &Path, key: &str) -> Result<Option<String>, String> {
    match run_git(root, ["config", "--get", key]) {
        Ok(output) => Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        )),
        Err(_) => Ok(None),
    }
}

fn canonical_git_path(root: &Path, value: &str) -> Result<PathBuf, String> {
    fs::canonicalize(canonical_git_path_unchecked(root, value))
        .map_err(|error| format!("failed to resolve Git path: {error}"))
}

fn canonical_git_path_unchecked(root: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn valid_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn ensure_flag(args: &mut Vec<String>, flag: &str) {
    if !args.iter().any(|arg| arg == flag) {
        args.push(flag.to_string());
    }
}

fn ensure_pair(args: &mut Vec<String>, flag: &str, value: &str) {
    if !args.iter().any(|arg| arg == flag) {
        args.extend([flag.to_string(), value.to_string()]);
    }
}

fn remote_mode_label(remote_count: usize) -> &'static str {
    if remote_count == 0 {
        "none"
    } else {
        "local-only"
    }
}

fn fail(message: &str) -> i32 {
    eprintln!("error: {message}");
    EXIT_ERROR
}

fn partial_failure(new_head: Option<&str>, message: &str) -> i32 {
    match new_head {
        Some(new_head) => eprintln!(
            "error: local-default commit {new_head} was created but finalization failed: {message}; inspect the commit and recover manually"
        ),
        None => eprintln!(
            "error: local-default commit may have been created but HEAD could not be resolved: {message}; inspect the repository and recover manually"
        ),
    }
    EXIT_ERROR
}

fn print_usage(stderr: bool) {
    let usage = "Usage: semantic-commit local-default --expect-head <full-sha> --expected-branch <name> [--receipt-out <path>] [message options] [--remote-mode local-only] [--dry-run|--validate-only] [--format text|json] (receipt-out is required when committing)";
    if stderr {
        eprintln!("{usage}");
    } else {
        println!("{usage}");
    }
}
