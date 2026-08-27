//! Governed remote surfaces: publish a feature branch, and adopt the remote's
//! default branch locally.
//!
//! Both operations are ordinary Git mutations that agent policy still has to
//! account for, and neither had an owner before. Raw `git push` cannot prove it
//! leaves the default branch alone, and raw `git merge --ff-only origin/main`
//! cannot prove it publishes nothing — so the delivery guard has to distrust
//! both on sight. These two commands make the safe cases provable:
//!
//! * [`run_push`] resolves one fully qualified refspec for the checked-out
//!   branch and refuses outright when that branch is the remote's default.
//! * [`run_sync_default`] only ever fast-forwards the local default branch to a
//!   commit that is already published, and refuses anything else.
//! * [`run_sync_branch`] only ever fast-forwards the checked-out, published
//!   non-default branch to its own remote-tracking ref.
//!
//! Every refusal is a typed envelope error, so a caller can tell "this is
//! forbidden" from "this could not be proven" without parsing prose.

use crate::commit_shared::{
    git_output, git_status_success, git_stdout_trimmed, git_stdout_trimmed_optional,
};
use crate::worktree::{
    CliError, detect_format, emit_error, emit_success, ensure_inside_git_repo,
    linked_worktrees_by_branch, summarize_git_error, take_format, take_help,
};
use nils_common::cli_contract::OutputFormat;
use serde::Serialize;
use serde_json::json;

pub fn dispatch(cmd: &str, args: &[String]) -> Option<i32> {
    match cmd {
        "push" => Some(run_push(args)),
        "sync-default" => Some(run_sync_default(args)),
        "sync-branch" => Some(run_sync_branch(args)),
        _ => None,
    }
}

const DEFAULT_REMOTE: &str = "origin";
/// Branch names conventionally used as a repository default. Used only to refuse
/// an *unverifiable* push, never to admit one.
const WELL_KNOWN_DEFAULT_BRANCHES: &[&str] = &[
    "main",
    "master",
    "trunk",
    "develop",
    "development",
    "default",
];
const PUSH_DEFAULT_HINT: &str = "pushing the default branch is a delivery decision, not a publish step; use \
     `forge-cli repo push-default` with the expected base and reason file when \
     direct-main delivery was explicitly authorized";

#[derive(Debug)]
struct PushArgs {
    remote: String,
    expect_default: Option<String>,
    force_with_lease: bool,
    dry_run: bool,
    format: OutputFormat,
}

#[derive(Debug)]
struct SyncArgs {
    remote: String,
    fetch: bool,
    dry_run: bool,
    format: OutputFormat,
}

#[derive(Debug, Serialize)]
struct PushOutput {
    branch: String,
    remote: String,
    remote_branch: String,
    refspec: String,
    head: String,
    default_branch: String,
    pushed: bool,
    dry_run: bool,
    created_remote_branch: bool,
    upstream: String,
    forced: bool,
}

#[derive(Debug, Serialize)]
struct SyncOutput {
    default_branch: String,
    remote: String,
    remote_ref: String,
    previous_head: String,
    new_head: String,
    strategy: &'static str,
    already_current: bool,
    fast_forward: bool,
    dry_run: bool,
    fetched: bool,
}

#[derive(Debug, Serialize)]
struct SyncBranchOutput {
    branch: String,
    remote: String,
    remote_ref: String,
    previous_head: String,
    new_head: String,
    strategy: &'static str,
    already_current: bool,
    fast_forward: bool,
    dry_run: bool,
    fetched: bool,
}

// ------------------------------------------------------------------ push

fn run_push(args: &[String]) -> i32 {
    let requested_format = detect_format(args);
    if take_help(args) {
        print_push_help();
        return 0;
    }
    let parsed = match parse_push_args(args) {
        Ok(parsed) => parsed,
        Err(err) => return emit_error("push", requested_format, err),
    };

    match push_branch(&parsed) {
        Ok(output) => emit_success("push", parsed.format, &output, || {
            if output.dry_run {
                format!(
                    "Would push {} -> {}/{}\nHead: {}",
                    output.branch,
                    output.remote,
                    output.remote_branch,
                    short(&output.head)
                )
            } else {
                format!(
                    "Pushed {} -> {}/{}\nHead: {}\nUpstream: {}",
                    output.branch,
                    output.remote,
                    output.remote_branch,
                    short(&output.head),
                    output.upstream
                )
            }
        }),
        Err(err) => emit_error("push", parsed.format, err),
    }
}

fn push_branch(args: &PushArgs) -> Result<PushOutput, CliError> {
    ensure_inside_git_repo()?;
    require_remote(&args.remote)?;

    let branch = current_branch().ok_or_else(|| {
        CliError::data("detached-head", "HEAD is not attached to a branch").with_hint(
            "check out the branch you want to publish; a detached HEAD has no branch to push",
        )
    })?;

    // The only way this command can touch the default branch is by being *on*
    // it, because the refspec below is pinned to the checked-out branch. So the
    // default branch is the single fact that has to be established before any
    // mutation, and failing to establish it is a refusal, not a warning.
    //
    // `--expect-default` is an escape hatch for an uncached remote head, never a
    // second opinion that can widen admission. Cached truth always wins, and a
    // disagreeing assertion is a mismatch: otherwise `--expect-default develop`
    // while standing on `main` would publish the default branch.
    let default_branch = match (cached_default_branch(&args.remote), &args.expect_default) {
        (Some(cached), Some(expected)) if cached != *expected => {
            return Err(CliError::data(
                "expect-default-mismatch",
                format!(
                    "--expect-default '{expected}' disagrees with the cached default \
                     branch '{cached}' of remote '{}'",
                    args.remote
                ),
            )
            .with_hint(
                "drop `--expect-default`; it names the default branch only when the \
                 remote head is not cached locally",
            ));
        }
        (Some(cached), _) => cached,
        (None, Some(expected)) => {
            // The assertion is unverifiable here, so it cannot be trusted to
            // clear a branch that plausibly *is* a default branch.
            if WELL_KNOWN_DEFAULT_BRANCHES.contains(&branch.as_str()) {
                return Err(CliError::data(
                    "default-branch-unverifiable",
                    format!(
                        "'{branch}' is a conventional default-branch name and the \
                         default branch of remote '{}' is not cached, so this push \
                         cannot be proven safe",
                        args.remote
                    ),
                )
                .with_hint(format!(
                    "run `git remote set-head {} --auto` to establish the real \
                     default branch; `--expect-default` cannot admit this push",
                    args.remote
                )));
            }
            expected.clone()
        }
        (None, None) => {
            return Err(CliError::data(
                "default-branch-unresolved",
                format!(
                    "cannot resolve the default branch of remote '{}'",
                    args.remote
                ),
            )
            .with_hint(format!(
                "run `git remote set-head {} --auto` to cache it, or pass \
                 `--expect-default <branch>` to name it offline",
                args.remote
            )));
        }
    };

    if branch == default_branch {
        return Err(CliError::data(
            "refuse-default-branch",
            format!(
                "'{branch}' is the default branch of remote '{}'",
                args.remote
            ),
        )
        .with_hint(PUSH_DEFAULT_HINT)
        .with_details(json!({
            "branch": branch,
            "remote": args.remote,
            "default_branch": default_branch,
        })));
    }

    let head = git_stdout_trimmed(&["rev-parse", "HEAD"]).map_err(|err| {
        CliError::runtime("head-unresolved", summarize_git_error(&err.to_string()))
    })?;

    let remote_ref = format!("refs/remotes/{}/{branch}", args.remote);
    let created_remote_branch =
        !git_status_success(&["show-ref", "--verify", "--quiet", &remote_ref]);

    // A fully qualified refspec removes every source of ambiguity that makes a
    // bare `git push` unclassifiable: `push.default`, `remote.pushDefault`,
    // configured push refspecs, and upstream inference all stop mattering.
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    let upstream = format!("{}/{branch}", args.remote);

    if args.dry_run {
        return Ok(PushOutput {
            branch,
            remote: args.remote.clone(),
            remote_branch: refspec
                .rsplit_once("refs/heads/")
                .map(|(_, name)| name.to_string())
                .unwrap_or_default(),
            refspec,
            head,
            default_branch,
            pushed: false,
            dry_run: true,
            created_remote_branch,
            upstream,
            forced: args.force_with_lease,
        });
    }

    let mut argv: Vec<&str> = vec!["push", "--quiet"];
    if args.force_with_lease {
        argv.push("--force-with-lease");
    }
    // Set the upstream whenever it is not already this branch's own ref on this
    // remote. "Has an upstream" is not good enough: a branch created before
    // `worktree add` passed `--no-track` carries the *default* branch as its
    // upstream, and that is exactly the state worth repairing on publish.
    if !upstream_is_own_ref(&args.remote, &branch) {
        argv.push("--set-upstream");
    }
    argv.push(&args.remote);
    argv.push(&refspec);
    git_output(&argv).map_err(|err| {
        CliError::runtime("git-push-failed", summarize_git_error(&err.to_string()))
    })?;

    Ok(PushOutput {
        remote_branch: branch.clone(),
        branch,
        remote: args.remote.clone(),
        refspec,
        head,
        default_branch,
        pushed: true,
        dry_run: false,
        created_remote_branch,
        upstream,
        forced: args.force_with_lease,
    })
}

// ---------------------------------------------------------- sync-default

fn run_sync_default(args: &[String]) -> i32 {
    let requested_format = detect_format(args);
    if take_help(args) {
        print_sync_help();
        return 0;
    }
    let parsed = match parse_sync_args(args) {
        Ok(parsed) => parsed,
        Err(err) => return emit_error("sync-default", requested_format, err),
    };

    match sync_default(&parsed) {
        Ok(output) => emit_success("sync-default", parsed.format, &output, || {
            if output.already_current {
                format!(
                    "Already current: {} at {}",
                    output.default_branch,
                    short(&output.new_head)
                )
            } else if output.dry_run {
                format!(
                    "Would fast-forward {} {}..{} ({})",
                    output.default_branch,
                    short(&output.previous_head),
                    short(&output.new_head),
                    output.strategy
                )
            } else {
                format!(
                    "Fast-forwarded {} {}..{} ({})",
                    output.default_branch,
                    short(&output.previous_head),
                    short(&output.new_head),
                    output.strategy
                )
            }
        }),
        Err(err) => emit_error("sync-default", parsed.format, err),
    }
}

fn sync_default(args: &SyncArgs) -> Result<SyncOutput, CliError> {
    ensure_inside_git_repo()?;
    require_remote(&args.remote)?;

    let default_branch = cached_default_branch(&args.remote).ok_or_else(|| {
        CliError::data(
            "default-branch-unresolved",
            format!(
                "cannot resolve the default branch of remote '{}'",
                args.remote
            ),
        )
        .with_hint(format!(
            "run `git remote set-head {} --auto` to cache it",
            args.remote
        ))
    })?;

    let remote_ref = format!("refs/remotes/{}/{default_branch}", args.remote);
    let local_ref = format!("refs/heads/{default_branch}");

    if args.fetch {
        let refspec = format!("+refs/heads/{default_branch}:{remote_ref}");
        git_output(&["fetch", "--quiet", &args.remote, &refspec]).map_err(|err| {
            CliError::runtime("git-fetch-failed", summarize_git_error(&err.to_string()))
                .with_hint("pass `--no-fetch` to sync against the already-fetched remote ref")
        })?;
    }

    let previous_head = rev_parse_commit(&local_ref).ok_or_else(|| {
        CliError::data(
            "local-default-branch-missing",
            format!("the repository has no local branch '{default_branch}'"),
        )
    })?;
    let new_head = rev_parse_commit(&remote_ref).ok_or_else(|| {
        CliError::data(
            "remote-default-branch-missing",
            format!("'{remote_ref}' does not resolve to a commit"),
        )
        .with_hint("fetch the remote default branch first, or drop `--no-fetch`")
    })?;

    if previous_head == new_head {
        return Ok(SyncOutput {
            default_branch,
            remote: args.remote.clone(),
            remote_ref,
            previous_head: previous_head.clone(),
            new_head: previous_head,
            strategy: "noop",
            already_current: true,
            fast_forward: true,
            dry_run: args.dry_run,
            fetched: args.fetch,
        });
    }

    // This is the whole safety argument for the command: the local ref only ever
    // moves forward onto a commit that is already published, so nothing is
    // authored, nothing is overwritten, and `git reset --hard @{1}` undoes it.
    if !git_status_success(&["merge-base", "--is-ancestor", &previous_head, &new_head]) {
        return Err(CliError::data(
            "not-fast-forward",
            format!(
                "'{default_branch}' has diverged from '{remote_ref}'; adopting the \
                 remote head would discard local commits"
            ),
        )
        .with_hint(
            "the local default branch carries commits the remote does not have; \
             deliver or move them off the default branch first",
        )
        .with_details(json!({
            "local_head": previous_head,
            "remote_head": new_head,
        })));
    }

    let checked_out_here = current_branch().as_deref() == Some(default_branch.as_str());
    let strategy = if checked_out_here {
        "merge-ff-only"
    } else {
        if let Some(path) = worktree_holding(&default_branch)? {
            return Err(CliError::data(
                "default-branch-checked-out-elsewhere",
                format!("'{default_branch}' is checked out in another worktree: {path}"),
            )
            .with_hint("run `git-cli sync-default` from that worktree")
            .with_details(json!({ "worktree": path })));
        }
        "update-ref"
    };

    if args.dry_run {
        return Ok(SyncOutput {
            default_branch,
            remote: args.remote.clone(),
            remote_ref,
            previous_head,
            new_head,
            strategy,
            already_current: false,
            fast_forward: true,
            dry_run: true,
            fetched: args.fetch,
        });
    }

    match strategy {
        "merge-ff-only" => {
            require_clean_checkout()?;
            git_output(&["merge", "--ff-only", "--quiet", &remote_ref]).map_err(|err| {
                CliError::runtime("git-merge-failed", summarize_git_error(&err.to_string()))
            })?;
        }
        _ => {
            // Compare-and-swap on the old value, so a concurrent update loses
            // instead of being silently overwritten.
            git_output(&["update-ref", &local_ref, &new_head, &previous_head]).map_err(|err| {
                CliError::runtime(
                    "git-update-ref-failed",
                    summarize_git_error(&err.to_string()),
                )
            })?;
        }
    }

    Ok(SyncOutput {
        default_branch,
        remote: args.remote.clone(),
        remote_ref,
        previous_head,
        new_head,
        strategy,
        already_current: false,
        fast_forward: true,
        dry_run: false,
        fetched: args.fetch,
    })
}

// ----------------------------------------------------------- sync-branch

fn run_sync_branch(args: &[String]) -> i32 {
    let requested_format = detect_format(args);
    if take_help(args) {
        print_sync_branch_help();
        return 0;
    }
    let parsed = match parse_sync_args_for("sync-branch", args) {
        Ok(parsed) => parsed,
        Err(err) => return emit_error("sync-branch", requested_format, err),
    };

    match sync_branch(&parsed) {
        Ok(output) => emit_success("sync-branch", parsed.format, &output, || {
            if output.already_current {
                format!(
                    "Already current: {} at {}",
                    output.branch,
                    short(&output.new_head)
                )
            } else if output.dry_run {
                format!(
                    "Would fast-forward {} {}..{}",
                    output.branch,
                    short(&output.previous_head),
                    short(&output.new_head)
                )
            } else {
                format!(
                    "Fast-forwarded {} {}..{}",
                    output.branch,
                    short(&output.previous_head),
                    short(&output.new_head)
                )
            }
        }),
        Err(err) => emit_error("sync-branch", parsed.format, err),
    }
}

fn sync_branch(args: &SyncArgs) -> Result<SyncBranchOutput, CliError> {
    ensure_inside_git_repo()?;
    require_remote(&args.remote)?;

    let branch = current_branch().ok_or_else(|| {
        CliError::data("detached-head", "HEAD is not attached to a branch")
            .with_hint("check out the published non-default branch you want to synchronize")
    })?;
    let default_branch = cached_default_branch(&args.remote).ok_or_else(|| {
        CliError::data(
            "default-branch-unresolved",
            format!(
                "cannot prove '{branch}' is non-default because remote '{}' has no cached default branch",
                args.remote
            ),
        )
        .with_hint(format!(
            "run `git remote set-head {} --auto` to cache it",
            args.remote
        ))
    })?;
    if branch == default_branch {
        return Err(CliError::data(
            "refuse-default-branch",
            format!(
                "'{branch}' is the default branch of remote '{}'; use `git-cli sync-default`",
                args.remote
            ),
        ));
    }
    if !upstream_is_own_ref(&args.remote, &branch) {
        return Err(CliError::data(
            "branch-upstream-mismatch",
            format!(
                "'{branch}' does not track its own branch on remote '{}'",
                args.remote
            ),
        )
        .with_hint("publish or repair the branch upstream with `git-cli push` first"));
    }
    if !args.dry_run {
        require_clean_checkout()?;
    }

    let remote_ref = format!("refs/remotes/{}/{branch}", args.remote);
    if args.fetch {
        let refspec = format!("+refs/heads/{branch}:{remote_ref}");
        git_output(&["fetch", "--quiet", &args.remote, &refspec]).map_err(|err| {
            CliError::runtime("git-fetch-failed", summarize_git_error(&err.to_string()))
                .with_hint("pass `--no-fetch` to sync against the already-fetched remote ref")
        })?;
    }

    let previous_head = rev_parse_commit("HEAD")
        .ok_or_else(|| CliError::data("head-unresolved", "HEAD does not resolve to a commit"))?;
    let new_head = rev_parse_commit(&remote_ref).ok_or_else(|| {
        CliError::data(
            "remote-branch-missing",
            format!("'{remote_ref}' does not resolve to a commit"),
        )
        .with_hint("publish the branch first, or drop `--no-fetch`")
    })?;

    if previous_head == new_head {
        return Ok(SyncBranchOutput {
            branch,
            remote: args.remote.clone(),
            remote_ref,
            previous_head: previous_head.clone(),
            new_head: previous_head,
            strategy: "noop",
            already_current: true,
            fast_forward: true,
            dry_run: args.dry_run,
            fetched: args.fetch,
        });
    }
    if !git_status_success(&["merge-base", "--is-ancestor", &previous_head, &new_head]) {
        return Err(CliError::data(
            "not-fast-forward",
            format!(
                "'{branch}' has diverged from '{remote_ref}'; synchronizing would discard local commits"
            ),
        )
        .with_hint("deliver or move the local-only commits before synchronizing")
        .with_details(json!({
            "local_head": previous_head,
            "remote_head": new_head,
        })));
    }

    if !args.dry_run {
        git_output(&["merge", "--ff-only", "--quiet", &remote_ref]).map_err(|err| {
            CliError::runtime("git-merge-failed", summarize_git_error(&err.to_string()))
        })?;
    }

    Ok(SyncBranchOutput {
        branch,
        remote: args.remote.clone(),
        remote_ref,
        previous_head,
        new_head,
        strategy: "merge-ff-only",
        already_current: false,
        fast_forward: true,
        dry_run: args.dry_run,
        fetched: args.fetch,
    })
}

// ----------------------------------------------------------------- git

fn current_branch() -> Option<String> {
    git_stdout_trimmed_optional(&["symbolic-ref", "--quiet", "--short", "HEAD"])
}

/// Whether the branch's configured upstream is already its own ref on `remote`.
///
/// Read from config rather than `@{upstream}`, because the remote-tracking ref
/// does not exist yet on a first publish and would make the check fail for the
/// wrong reason.
fn upstream_is_own_ref(remote: &str, branch: &str) -> bool {
    let configured_remote =
        git_stdout_trimmed_optional(&["config", "--get", &format!("branch.{branch}.remote")]);
    let configured_merge =
        git_stdout_trimmed_optional(&["config", "--get", &format!("branch.{branch}.merge")]);
    configured_remote.as_deref() == Some(remote)
        && configured_merge.as_deref() == Some(format!("refs/heads/{branch}").as_str())
}

fn cached_default_branch(remote: &str) -> Option<String> {
    let cached = git_stdout_trimmed_optional(&[
        "symbolic-ref",
        "--quiet",
        "--short",
        &format!("refs/remotes/{remote}/HEAD"),
    ])?;
    cached
        .strip_prefix(&format!("{remote}/"))
        .map(str::to_string)
}

fn rev_parse_commit(reference: &str) -> Option<String> {
    git_stdout_trimmed_optional(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{reference}^{{commit}}"),
    ])
}

fn require_remote(remote: &str) -> Result<(), CliError> {
    if git_status_success(&["config", "--get", &format!("remote.{remote}.url")]) {
        return Ok(());
    }
    Err(
        CliError::data("unknown-remote", format!("no remote named '{remote}'"))
            .with_hint("pass `--remote <name>` to select a configured remote"),
    )
}

fn require_clean_checkout() -> Result<(), CliError> {
    let status =
        git_stdout_trimmed(&["status", "--porcelain", "--untracked-files=no"]).map_err(|err| {
            CliError::runtime("git-status-failed", summarize_git_error(&err.to_string()))
        })?;
    if status.trim().is_empty() {
        return Ok(());
    }
    Err(CliError::data(
        "dirty-checkout",
        "the checkout has staged or unstaged changes",
    )
    .with_hint("commit or stash the changes before moving the checked-out branch"))
}

/// Which other worktree, if any, holds `branch` checked out.
fn worktree_holding(branch: &str) -> Result<Option<String>, CliError> {
    let by_branch = linked_worktrees_by_branch()
        .map_err(|err| CliError::runtime("git-worktree-list-failed", err.to_string()))?;
    Ok(by_branch.get(branch).cloned())
}

fn short(sha: &str) -> String {
    sha.chars().take(12).collect()
}

// ----------------------------------------------------------------- args

fn parse_push_args(args: &[String]) -> Result<PushArgs, CliError> {
    let mut rest: Vec<String> = args.to_vec();
    let format = take_format(&mut rest)?;
    let mut remote = DEFAULT_REMOTE.to_string();
    let mut expect_default = None;
    let mut force_with_lease = false;
    let mut dry_run = false;

    let mut index = 0usize;
    while index < rest.len() {
        match rest[index].as_str() {
            "--remote" => {
                remote = take_value(&rest, index, "--remote")?;
                index += 2;
            }
            value if value.starts_with("--remote=") => {
                remote = value.trim_start_matches("--remote=").to_string();
                index += 1;
            }
            "--expect-default" => {
                expect_default = Some(take_value(&rest, index, "--expect-default")?);
                index += 2;
            }
            value if value.starts_with("--expect-default=") => {
                expect_default = Some(value.trim_start_matches("--expect-default=").to_string());
                index += 1;
            }
            "--force-with-lease" => {
                force_with_lease = true;
                index += 1;
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            other => {
                return Err(CliError::usage(
                    "unknown-argument",
                    format!("unknown argument: {other}"),
                )
                .with_hint("run `git-cli push --help` for the accepted flags"));
            }
        }
    }

    if remote.is_empty() {
        return Err(CliError::usage(
            "missing-remote",
            "--remote requires a value",
        ));
    }
    if expect_default.as_deref() == Some("") {
        return Err(CliError::usage(
            "missing-expect-default",
            "--expect-default requires a branch name",
        ));
    }

    Ok(PushArgs {
        remote,
        expect_default,
        force_with_lease,
        dry_run,
        format,
    })
}

fn parse_sync_args(args: &[String]) -> Result<SyncArgs, CliError> {
    parse_sync_args_for("sync-default", args)
}

fn parse_sync_args_for(command: &str, args: &[String]) -> Result<SyncArgs, CliError> {
    let mut rest: Vec<String> = args.to_vec();
    let format = take_format(&mut rest)?;
    let mut remote = DEFAULT_REMOTE.to_string();
    let mut fetch = true;
    let mut dry_run = false;

    let mut index = 0usize;
    while index < rest.len() {
        match rest[index].as_str() {
            "--remote" => {
                remote = take_value(&rest, index, "--remote")?;
                index += 2;
            }
            value if value.starts_with("--remote=") => {
                remote = value.trim_start_matches("--remote=").to_string();
                index += 1;
            }
            "--no-fetch" => {
                fetch = false;
                index += 1;
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            other => {
                return Err(CliError::usage(
                    "unknown-argument",
                    format!("unknown argument: {other}"),
                )
                .with_hint(format!(
                    "run `git-cli {command} --help` for the accepted flags"
                )));
            }
        }
    }

    if remote.is_empty() {
        return Err(CliError::usage(
            "missing-remote",
            "--remote requires a value",
        ));
    }

    Ok(SyncArgs {
        remote,
        fetch,
        dry_run,
        format,
    })
}

fn take_value(args: &[String], index: usize, flag: &str) -> Result<String, CliError> {
    args.get(index + 1)
        .cloned()
        .ok_or_else(|| CliError::usage("missing-value", format!("{flag} requires a value")))
}

fn print_push_help() {
    println!(
        "Usage: git-cli push [--remote <name>] [--expect-default <branch>] [--force-with-lease] [--dry-run] [--format text|json]"
    );
    println!("  Publish the checked-out branch to its own branch on <remote> (default: origin).");
    println!("  Refuses the remote's default branch; that is `forge-cli repo push-default`'s job.");
    println!("  Sets the upstream on first publish, so the branch tracks its own ref.");
    println!(
        "  --expect-default names the default branch when `refs/remotes/<remote>/HEAD` is not cached."
    );
}

fn print_sync_help() {
    println!(
        "Usage: git-cli sync-default [--remote <name>] [--no-fetch] [--dry-run] [--format text|json]"
    );
    println!("  Fast-forward the local default branch to its remote-tracking ref.");
    println!(
        "  Refuses anything that is not a pure fast-forward onto an already-published commit."
    );
    println!("  Moves the ref directly when no worktree holds the default branch checked out.");
    println!(
        "  --no-fetch syncs against the already-fetched remote ref instead of contacting the remote."
    );
}

fn print_sync_branch_help() {
    println!(
        "Usage: git-cli sync-branch [--remote <name>] [--no-fetch] [--dry-run] [--format text|json]"
    );
    println!("  Fast-forward the checked-out non-default branch to its own remote-tracking ref.");
    println!("  Refuses detached, default, untracked, dirty, or diverged branches.");
    println!(
        "  --no-fetch syncs against the already-fetched remote ref instead of contacting the remote."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_args_default_to_origin_and_no_mutation_modifiers() {
        let parsed = parse_push_args(&[]).expect("parse");
        assert_eq!(parsed.remote, "origin");
        assert!(!parsed.force_with_lease);
        assert!(!parsed.dry_run);
        assert!(parsed.expect_default.is_none());
    }

    #[test]
    fn push_args_accept_inline_and_separated_values() {
        let inline =
            parse_push_args(&["--remote=upstream".into(), "--expect-default=trunk".into()])
                .expect("parse inline");
        assert_eq!(inline.remote, "upstream");
        assert_eq!(inline.expect_default.as_deref(), Some("trunk"));

        let separated = parse_push_args(&[
            "--remote".into(),
            "upstream".into(),
            "--expect-default".into(),
            "trunk".into(),
        ])
        .expect("parse separated");
        assert_eq!(separated.remote, "upstream");
        assert_eq!(separated.expect_default.as_deref(), Some("trunk"));
    }

    #[test]
    fn push_args_reject_unknown_flags() {
        let err = parse_push_args(&["--force".into()]).expect_err("reject");
        assert_eq!(err.code, "unknown-argument");
    }

    #[test]
    fn sync_args_fetch_by_default_and_honor_no_fetch() {
        assert!(parse_sync_args(&[]).expect("parse").fetch);
        assert!(
            !parse_sync_args(&["--no-fetch".into()])
                .expect("parse")
                .fetch
        );
    }
}
