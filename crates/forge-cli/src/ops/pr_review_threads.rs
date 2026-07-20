//! `pr review-threads` atom — read-side review-thread state for a PR / MR.
//!
//! Spec / ops: `cli.forge-cli.pr.review-threads.v1`. Returns code-review
//! threads with their resolved state: the `reviewThreads` GraphQL connection
//! on GitHub (REST review comments carry no resolved bit), resolvable
//! discussions on GitLab. Issue-style comments stay on `pr comments`.
//!
//! Also hosts [`ensure_payload_resolved`], the `pr merge` unresolved-thread gate
//! (rule 13), and [`ensure_review_threads_resolved`], a convenience composition
//! of [`compute_for_pr`] + [`ensure_payload_resolved`]. The gate blocks only on
//! non-outdated unresolved threads and fails closed with
//! `unresolved_review_threads` unless `--allow-unresolved-threads` (with a
//! recorded reason) is passed; outdated unresolved threads are dispositioned
//! `stale` by the merge step, not blocked. `pr merge` runs rule 13 inline (so it
//! can record those stale dispositions) rather than through the composition
//! helper. Bot reviewers post threads asynchronously after PR creation, so the
//! gate runs at merge time — the last action — rather than at creation.
//!
//! The local provider has no review-thread model: the atom returns an empty
//! thread list and the merge gate passes trivially.

use std::collections::BTreeSet;
use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload};
use crate::cli::{BINARY, GlobalFlags, PrReviewThreadsListArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_comments::{
    github_repo_slug_from_url, gitlab_host_from_url, gitlab_project_path_from_url,
    split_concatenated_arrays,
};
use crate::ops::pr_view;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;

const SCHEMA: &str = "pr.review-threads";
const SCHEMA_VERSION: u32 = 1;

const MAX_GITHUB_THREAD_PAGES: usize = 100;
const MAX_GITHUB_THREAD_COMMENT_PAGES: usize = 100;

/// GitHub exposes thread resolution only through GraphQL. Both the thread and
/// per-thread comment connections are paginated because semantic convergence
/// must not silently ignore a later reviewer reply.
const GITHUB_THREADS_QUERY: &str = "query($owner: String!, $name: String!, $pr: Int!, $after: String) { repository(owner: $owner, name: $name) { pullRequest(number: $pr) { headRefOid reviewThreads(first: 100, after: $after) { nodes { id isResolved isOutdated path diffSide line originalLine originalStartLine startDiffSide startLine subjectType comments(first: 100) { nodes { id author { login } body createdAt url } pageInfo { hasNextPage endCursor } } } pageInfo { hasNextPage endCursor } } } } }";
const GITHUB_THREAD_COMMENTS_QUERY: &str = "query($thread: ID!, $after: String) { node(id: $thread) { ... on PullRequestReviewThread { id comments(first: 100, after: $after) { nodes { id author { login } body createdAt url } pageInfo { hasNextPage endCursor } } } } }";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrReviewThreadComment {
    pub id: String,
    pub author: String,
    pub body: String,
    pub created_at: String,
    pub url: String,
}

/// One review thread, normalized across providers.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrReviewThreadSummary {
    /// Provider thread handle. GitHub: the `reviewThreads` node `id`
    /// (`PRRT_...`), used as both `threadId` for resolve and
    /// `pullRequestReviewThreadId` for reply. GitLab: the discussion id.
    pub id: String,
    pub resolved: bool,
    /// GitHub `isOutdated` (the anchored diff hunk changed); always false on
    /// GitLab. Outdated unresolved threads are mechanically dispositioned
    /// `stale` at the merge gate (recorded, non-blocking) rather than blocking
    /// the merge; see [`stale_dispositions`].
    pub outdated: bool,
    pub author: String,
    /// File the thread is anchored to; empty for non-inline threads.
    pub path: String,
    pub diff_side: Option<String>,
    pub line: Option<u32>,
    pub original_line: Option<u32>,
    pub original_start_line: Option<u32>,
    pub start_diff_side: Option<String>,
    pub start_line: Option<u32>,
    pub subject_type: Option<String>,
    pub created_at: String,
    pub url: String,
    /// First comment of the thread.
    pub body: String,
    /// Complete ordered comment stream for this thread. The first comment is
    /// retained in the unmarked summary fields above for additive compatibility.
    pub comments: Vec<PrReviewThreadComment>,
}

/// Envelope payload for `cli.forge-cli.pr.review-threads.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrReviewThreadsPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    pub total: usize,
    pub unresolved: usize,
    pub threads: Vec<PrReviewThreadSummary>,
}

struct GitHubThreadPage {
    head_sha: String,
    threads: Vec<GitHubThreadNode>,
    has_next_page: bool,
    end_cursor: Option<String>,
}

struct GitHubThreadNode {
    summary: PrReviewThreadSummary,
    comments_has_next_page: bool,
    comments_end_cursor: Option<String>,
}

struct GitHubThreadCommentsPage {
    thread_id: String,
    comments: Vec<PrReviewThreadComment>,
    has_next_page: bool,
    end_cursor: Option<String>,
}

/// An outdated, unresolved review thread mechanically dispositioned `stale`
/// at the merge gate (rule 13). Recorded so a genuine finding whose anchor
/// merely moved stays auditable rather than being silently dropped.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StaleThreadDisposition {
    pub thread_id: String,
    pub author: String,
    pub path: String,
    /// First line of the thread's first comment.
    pub summary: String,
    /// Always `"stale"` in v1.
    pub disposition: &'static str,
    /// Mechanical reason the thread was dispositioned rather than blocking.
    pub rationale: &'static str,
}

pub fn run(
    global: &GlobalFlags,
    args: PrReviewThreadsListArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    if global.is_local() {
        let runner = crate::local::LocalRunner::from_global(global)?;
        return run_with(&runner, global, args, format, git_remote_url);
    }
    let runner = default_runner();
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrReviewThreadsListArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        &remote_url_lookup,
    )?;

    if global.dry_run {
        if matches!(ctx.provider, Provider::Local) {
            return Ok(emit_success(
                schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
                DryRunPayload {
                    provider: ctx.provider.as_str(),
                    plan: Vec::new(),
                    review_convergence: None,
                },
                format,
                |p| println!("would run: {plan}", plan = p.plan.join(" ")),
            ));
        }

        let threads_call =
            build_threads_dry_run_call(&ctx, &global.remote, args.id, &remote_url_lookup)?;
        let payload = DryRunPayload::new(ctx.provider, &threads_call);
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            payload,
            format,
            |p| println!("would run: {plan}", plan = p.plan.join(" ")),
        ));
    }

    // Resolve the canonical PR/MR URL first (same pattern as `pr comments`):
    // both providers derive the API path segments from it.
    let view_output = runner.run(&pr_view::build_view_call(&ctx, &args.id.to_string()))?;
    let view = pr_view::parse_view_output(&ctx, &view_output)?;

    // No thread model on the local provider — empty list, not an error.
    if matches!(ctx.provider, Provider::Local) {
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            PrReviewThreadsPayload {
                provider: ctx.provider.as_str(),
                number: view.number,
                url: view.url,
                head_sha: None,
                total: 0,
                unresolved: 0,
                threads: Vec::new(),
            },
            format,
            render_text,
        ));
    }

    let payload = compute_for_pr(runner, &ctx, &view.url, view.number)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        payload,
        format,
        render_text,
    ))
}

/// Convenience composition of [`compute_for_pr`] + [`ensure_payload_resolved`]:
/// fetch review threads for the PR/MR and fail closed with
/// `unresolved_review_threads` (DATA 65) when a non-outdated thread is
/// unresolved (outdated threads are dispositioned `stale` by the merge step, not
/// blocked here). The local provider passes trivially (no thread model). `pr
/// merge` runs rule 13 inline rather than through this helper so it can also
/// record the stale dispositions; this composition remains for callers that only
/// need the gate result.
pub fn ensure_review_threads_resolved<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    pr_url: &str,
    number: u64,
) -> Result<(), ForgeError> {
    let payload = compute_for_pr(runner, ctx, pr_url, number)?;
    ensure_payload_resolved(&payload)
}

/// Fetch the thread snapshot for a known PR URL without emitting an envelope.
/// Review convergence uses this immediately before merge so its structured
/// snapshot and the existing independent rule-13 gate share one provider read.
pub fn compute_for_pr<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    pr_url: &str,
    number: u64,
) -> Result<PrReviewThreadsPayload, ForgeError> {
    if matches!(ctx.provider, Provider::Local) {
        return Ok(PrReviewThreadsPayload {
            provider: ctx.provider.as_str(),
            number,
            url: pr_url.to_string(),
            head_sha: None,
            total: 0,
            unresolved: 0,
            threads: Vec::new(),
        });
    }
    if matches!(ctx.provider, Provider::GitHub) {
        return compute_github_for_pr(runner, ctx, pr_url, number);
    }
    let call = build_threads_call(ctx, pr_url, number)?;
    let output = runner.run(&call)?;
    let threads = parse_threads(ctx, &output, pr_url)?;
    let unresolved = threads.iter().filter(|thread| !thread.resolved).count();
    Ok(PrReviewThreadsPayload {
        provider: ctx.provider.as_str(),
        number,
        url: pr_url.to_string(),
        head_sha: None,
        total: threads.len(),
        unresolved,
        threads,
    })
}

fn compute_github_for_pr<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    pr_url: &str,
    number: u64,
) -> Result<PrReviewThreadsPayload, ForgeError> {
    let slug = github_repo_slug_from_url(pr_url).ok_or_else(|| {
        snapshot_incomplete(
            "unable to derive GitHub owner/repo from PR url",
            Some(format!("url={pr_url}")),
        )
    })?;
    let (owner, name) = slug.split_once('/').ok_or_else(|| {
        snapshot_incomplete(
            "unable to split GitHub owner/repo slug",
            Some(format!("slug={slug}")),
        )
    })?;
    let mut cursor = None;
    let mut seen_cursors = BTreeSet::new();
    let mut expected_head = None;
    let mut threads = Vec::new();

    for page_index in 0..MAX_GITHUB_THREAD_PAGES {
        let output = runner.run(&build_github_threads_page_call(
            ctx,
            owner,
            name,
            number,
            cursor.as_deref(),
        ))?;
        let page = parse_github_thread_page(&output)?;
        if expected_head
            .as_deref()
            .is_some_and(|head| head != page.head_sha)
        {
            return Err(snapshot_incomplete(
                "the PR head changed while paginating review threads",
                Some(format!(
                    "expected_head={}; provider_head={}",
                    expected_head.as_deref().unwrap_or("<missing>"),
                    page.head_sha
                )),
            ));
        }
        expected_head.get_or_insert(page.head_sha);
        for node in page.threads {
            threads.push(complete_github_thread_comments(runner, ctx, node)?);
        }
        if !page.has_next_page {
            let unresolved = threads.iter().filter(|thread| !thread.resolved).count();
            return Ok(PrReviewThreadsPayload {
                provider: ctx.provider.as_str(),
                number,
                url: pr_url.to_string(),
                head_sha: expected_head,
                total: threads.len(),
                unresolved,
                threads,
            });
        }
        let next = page.end_cursor.ok_or_else(|| {
            snapshot_incomplete(
                "review-thread pagination is missing endCursor",
                Some(format!("page={}", page_index + 1)),
            )
        })?;
        if !seen_cursors.insert(next.clone()) {
            return Err(snapshot_incomplete(
                "review-thread pagination repeated a cursor",
                Some(format!("page={}; cursor={next}", page_index + 1)),
            ));
        }
        cursor = Some(next);
    }
    Err(snapshot_incomplete(
        "review-thread pagination exceeded the safety page limit",
        Some(format!("max_pages={MAX_GITHUB_THREAD_PAGES}")),
    ))
}

fn complete_github_thread_comments<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    mut node: GitHubThreadNode,
) -> Result<PrReviewThreadSummary, ForgeError> {
    let mut cursor = node.comments_end_cursor.take();
    let mut seen_cursors = BTreeSet::new();
    for page_index in 0..MAX_GITHUB_THREAD_COMMENT_PAGES {
        if !node.comments_has_next_page {
            return Ok(node.summary);
        }
        let after = cursor.as_deref().ok_or_else(|| {
            snapshot_incomplete(
                "review-thread comment pagination is missing endCursor",
                Some(format!(
                    "thread_id={}; page={}",
                    node.summary.id,
                    page_index + 1
                )),
            )
        })?;
        if !seen_cursors.insert(after.to_string()) {
            return Err(snapshot_incomplete(
                "review-thread comment pagination repeated a cursor",
                Some(format!("thread_id={}; cursor={after}", node.summary.id)),
            ));
        }
        let output = runner.run(&build_github_thread_comments_call(
            ctx,
            &node.summary.id,
            Some(after),
        ))?;
        let page = parse_github_thread_comments_page(&output)?;
        if page.thread_id != node.summary.id {
            return Err(snapshot_incomplete(
                "review-thread comment page returned a different thread",
                Some(format!(
                    "expected_thread={}; provider_thread={}",
                    node.summary.id, page.thread_id
                )),
            ));
        }
        node.summary.comments.extend(page.comments);
        node.comments_has_next_page = page.has_next_page;
        cursor = page.end_cursor;
    }
    Err(snapshot_incomplete(
        "review-thread comment pagination exceeded the safety page limit",
        Some(format!(
            "thread_id={}; max_pages={MAX_GITHUB_THREAD_COMMENT_PAGES}",
            node.summary.id
        )),
    ))
}

/// Apply the unresolved-thread merge gate to an already-fetched snapshot.
///
/// Only non-outdated unresolved threads block. An outdated unresolved thread
/// (its anchored diff hunk changed) is mechanically dispositioned `stale` by
/// [`stale_dispositions`] and recorded in the merge payload rather than
/// blocking the merge, so a stale bot review can no longer wedge convergence.
/// Resolved threads never block.
pub fn ensure_payload_resolved(payload: &PrReviewThreadsPayload) -> Result<(), ForgeError> {
    let blocking: Vec<&PrReviewThreadSummary> = payload
        .threads
        .iter()
        .filter(|thread| !thread.resolved && !thread.outdated)
        .collect();
    if blocking.is_empty() {
        return Ok(());
    }
    let listing = blocking
        .iter()
        .map(|t| {
            let first_line = t.body.lines().next().unwrap_or("");
            let anchor = if t.path.is_empty() {
                String::new()
            } else {
                format!(" @ {path}", path = t.path)
            };
            format!("- {author}{anchor}: {first_line}", author = t.author)
        })
        .collect::<Vec<_>>()
        .join("\n");
    Err(ForgeError::validation(
        schema_err(),
        "unresolved_review_threads",
        format!(
            "{n} unresolved review thread(s) on the PR/MR; disposition each (repair / resolve as accepted / convert to a follow-up) or pass --allow-unresolved-threads to bypass",
            n = blocking.len(),
        ),
        Some(listing),
    ))
}

/// Unresolved threads whose anchored diff hunk is outdated. At the merge gate
/// these are mechanically dispositioned `stale`: recorded (never silently
/// dropped) so a genuine finding whose anchor merely moved stays auditable,
/// but no longer counted as blocking by [`ensure_payload_resolved`].
pub fn stale_dispositions(payload: &PrReviewThreadsPayload) -> Vec<StaleThreadDisposition> {
    payload
        .threads
        .iter()
        .filter(|thread| !thread.resolved && thread.outdated)
        .map(|thread| StaleThreadDisposition {
            thread_id: thread.id.clone(),
            author: thread.author.clone(),
            path: thread.path.clone(),
            summary: thread.body.lines().next().unwrap_or("").to_string(),
            disposition: "stale",
            rationale: "the anchored diff hunk is outdated; the referenced code changed",
        })
        .collect()
}

pub(crate) fn build_threads_call(
    ctx: &ProviderContext,
    pr_url: &str,
    number: u64,
) -> Result<BackendCall, ForgeError> {
    match ctx.provider {
        Provider::GitHub | Provider::Local => {
            let slug = github_repo_slug_from_url(pr_url).ok_or_else(|| {
                ForgeError::software(
                    schema_err(),
                    "unable to derive GitHub owner/repo from PR url",
                    Some(format!("url={pr_url}")),
                )
            })?;
            let (owner, name) = slug.split_once('/').ok_or_else(|| {
                ForgeError::software(
                    schema_err(),
                    "unable to split GitHub owner/repo slug",
                    Some(format!("slug={slug}")),
                )
            })?;
            Ok(build_github_threads_call(ctx, owner, name, number))
        }
        Provider::GitLab => {
            let project = gitlab_project_path_from_url(pr_url).ok_or_else(|| {
                ForgeError::software(
                    schema_err(),
                    "unable to derive GitLab project path from MR web_url",
                    Some(format!("url={pr_url}")),
                )
            })?;
            let host = gitlab_host_from_url(pr_url).ok_or_else(|| {
                ForgeError::software(
                    schema_err(),
                    "unable to derive GitLab host from MR web_url",
                    Some(format!("url={pr_url}")),
                )
            })?;
            let encoded = project.replace('/', "%2F");
            let path =
                format!("projects/{encoded}/merge_requests/{number}/discussions?per_page=100");
            Ok(BackendCall::new(
                BackendProgram::Glab,
                [
                    OsString::from("api"),
                    OsString::from("--paginate"),
                    OsString::from("--hostname"),
                    OsString::from(host),
                    OsString::from(path),
                ],
            ))
        }
    }
}

fn build_threads_dry_run_call<F: Fn(&str) -> Option<String>>(
    ctx: &ProviderContext,
    remote: &str,
    number: u64,
    remote_url_lookup: &F,
) -> Result<BackendCall, ForgeError> {
    let slug = resolve_repo_slug(ctx, remote, remote_url_lookup)?;
    match ctx.provider {
        Provider::GitHub => {
            let (owner, name) = slug.split_once('/').ok_or_else(|| {
                ForgeError::validation(
                    schema_err(),
                    "repo_required",
                    "review-threads dry-run requires a repo slug shaped as owner/name",
                    Some(format!("repo={slug}")),
                )
            })?;
            Ok(build_github_threads_call(ctx, owner, name, number))
        }
        Provider::GitLab => {
            let encoded = slug.replace('/', "%2F");
            let path =
                format!("projects/{encoded}/merge_requests/{number}/discussions?per_page=100");
            Ok(BackendCall::new(
                BackendProgram::Glab,
                [
                    OsString::from("api"),
                    OsString::from("--paginate"),
                    OsString::from("--hostname"),
                    OsString::from(&ctx.host),
                    OsString::from(path),
                ],
            ))
        }
        Provider::Local => unreachable!("local dry-run returns before building a backend call"),
    }
}

fn build_github_threads_call(
    ctx: &ProviderContext,
    owner: &str,
    name: &str,
    number: u64,
) -> BackendCall {
    build_github_threads_page_call(ctx, owner, name, number, None)
}

fn build_github_threads_page_call(
    ctx: &ProviderContext,
    owner: &str,
    name: &str,
    number: u64,
    after: Option<&str>,
) -> BackendCall {
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from("-f"),
        OsString::from(format!("query={GITHUB_THREADS_QUERY}")),
        OsString::from("-f"),
        OsString::from(format!("owner={owner}")),
        OsString::from("-f"),
        OsString::from(format!("name={name}")),
        OsString::from("-F"),
        OsString::from(format!("pr={number}")),
    ]);
    if let Some(after) = after {
        argv.extend([
            OsString::from("-f"),
            OsString::from(format!("after={after}")),
        ]);
    }
    BackendCall::new(BackendProgram::Gh, argv)
}

fn build_github_thread_comments_call(
    ctx: &ProviderContext,
    thread_id: &str,
    after: Option<&str>,
) -> BackendCall {
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from("-f"),
        OsString::from(format!("query={GITHUB_THREAD_COMMENTS_QUERY}")),
        OsString::from("-f"),
        OsString::from(format!("thread={thread_id}")),
    ]);
    if let Some(after) = after {
        argv.extend([
            OsString::from("-f"),
            OsString::from(format!("after={after}")),
        ]);
    }
    BackendCall::new(BackendProgram::Gh, argv)
}

fn resolve_repo_slug<F: Fn(&str) -> Option<String>>(
    ctx: &ProviderContext,
    remote: &str,
    lookup: &F,
) -> Result<String, ForgeError> {
    if let Some(slug) = ctx.repo.clone() {
        return Ok(slug);
    }
    if let Some(url) = lookup(remote)
        && let Some(parsed) = nils_common::git::parse_git_remote_url(&url)
    {
        return Ok(parsed.path);
    }
    Err(ForgeError::validation(
        schema_err(),
        "repo_required",
        "review-threads dry-run requires --repo owner/name or a recognised forge remote",
        None,
    ))
}

pub(crate) fn parse_threads(
    ctx: &ProviderContext,
    output: &BackendSuccess,
    pr_url: &str,
) -> Result<Vec<PrReviewThreadSummary>, ForgeError> {
    match ctx.provider {
        Provider::GitHub | Provider::Local => parse_github_threads(output),
        Provider::GitLab => parse_gitlab_discussions(output, pr_url),
    }
}

pub(crate) fn ensure_thread_belongs_to_pr<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    number: u64,
    thread_id: &str,
) -> Result<(), ForgeError> {
    let view_output = runner.run(&pr_view::build_view_call(ctx, &number.to_string()))?;
    let view = pr_view::parse_view_output(ctx, &view_output)?;
    let threads_call = build_threads_call(ctx, &view.url, view.number)?;
    let threads_output = runner.run(&threads_call)?;
    let threads = parse_threads(ctx, &threads_output, &view.url)?;
    if threads.iter().any(|thread| thread.id == thread_id) {
        return Ok(());
    }
    Err(ForgeError::validation(
        schema_err(),
        "review_thread_pr_mismatch",
        format!("review thread {thread_id} does not belong to PR #{number}"),
        Some(format!(
            "thread_id={thread_id}; pr={number}; url={url}",
            url = view.url
        )),
    ))
}

fn parse_github_threads(output: &BackendSuccess) -> Result<Vec<PrReviewThreadSummary>, ForgeError> {
    Ok(parse_github_thread_page(output)?
        .threads
        .into_iter()
        .map(|node| node.summary)
        .collect())
}

fn parse_github_thread_page(output: &BackendSuccess) -> Result<GitHubThreadPage, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "review-threads response is invalid JSON",
            Some(e.to_string()),
        )
    })?;
    reject_graphql_errors(&value, "review threads")?;
    let pull = value
        .pointer("/data/repository/pullRequest")
        .ok_or_else(|| {
            snapshot_incomplete("review-threads response is missing pullRequest", None)
        })?;
    let head_sha = required_json_string(pull, "/headRefOid", "headRefOid")?;
    let connection = pull.pointer("/reviewThreads").ok_or_else(|| {
        snapshot_incomplete("review-threads response is missing reviewThreads", None)
    })?;
    let nodes = connection
        .pointer("/nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .ok_or_else(|| {
            snapshot_incomplete(
                "review-threads response is missing reviewThreads nodes",
                None,
            )
        })?;
    let (has_next_page, end_cursor) = parse_page_info(connection, "reviewThreads")?;
    let mut threads = Vec::new();
    for node in nodes {
        let id = required_json_string(&node, "/id", "reviewThread.id")?;
        let comments_connection = node.pointer("/comments").ok_or_else(|| {
            snapshot_incomplete(
                "review-thread response is missing comments",
                Some(format!("thread_id={id}")),
            )
        })?;
        let comments = parse_github_thread_comments(comments_connection, &id)?;
        let (comments_has_next_page, comments_end_cursor) =
            parse_page_info(comments_connection, "reviewThread.comments")?;
        let first = comments.first().cloned().unwrap_or(PrReviewThreadComment {
            id: String::new(),
            author: String::new(),
            body: String::new(),
            created_at: String::new(),
            url: String::new(),
        });
        threads.push(GitHubThreadNode {
            summary: PrReviewThreadSummary {
                id,
                resolved: required_json_bool(&node, "/isResolved", "reviewThread.isResolved")?,
                outdated: required_json_bool(&node, "/isOutdated", "reviewThread.isOutdated")?,
                author: first.author.clone(),
                path: node
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                diff_side: Some(required_json_string(
                    &node,
                    "/diffSide",
                    "reviewThread.diffSide",
                )?),
                line: optional_json_u32(&node, "/line"),
                original_line: optional_json_u32(&node, "/originalLine"),
                original_start_line: optional_json_u32(&node, "/originalStartLine"),
                start_diff_side: node
                    .pointer("/startDiffSide")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                start_line: optional_json_u32(&node, "/startLine"),
                subject_type: Some(required_json_string(
                    &node,
                    "/subjectType",
                    "reviewThread.subjectType",
                )?),
                created_at: first.created_at.clone(),
                url: first.url.clone(),
                body: first.body.clone(),
                comments,
            },
            comments_has_next_page,
            comments_end_cursor,
        });
    }
    Ok(GitHubThreadPage {
        head_sha,
        threads,
        has_next_page,
        end_cursor,
    })
}

fn parse_github_thread_comments_page(
    output: &BackendSuccess,
) -> Result<GitHubThreadCommentsPage, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "review-thread comments response is invalid JSON",
            Some(e.to_string()),
        )
    })?;
    reject_graphql_errors(&value, "review-thread comments")?;
    let node = value.pointer("/data/node").ok_or_else(|| {
        snapshot_incomplete("review-thread comments response is missing node", None)
    })?;
    let thread_id = required_json_string(node, "/id", "reviewThread.id")?;
    let connection = node.pointer("/comments").ok_or_else(|| {
        snapshot_incomplete(
            "review-thread comments response is missing comments",
            Some(format!("thread_id={thread_id}")),
        )
    })?;
    let comments = parse_github_thread_comments(connection, &thread_id)?;
    let (has_next_page, end_cursor) = parse_page_info(connection, "reviewThread.comments")?;
    Ok(GitHubThreadCommentsPage {
        thread_id,
        comments,
        has_next_page,
        end_cursor,
    })
}

fn parse_github_thread_comments(
    connection: &serde_json::Value,
    thread_id: &str,
) -> Result<Vec<PrReviewThreadComment>, ForgeError> {
    let nodes = connection
        .get("nodes")
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            snapshot_incomplete(
                "review-thread comments response is missing nodes",
                Some(format!("thread_id={thread_id}")),
            )
        })?;
    nodes
        .iter()
        .map(|comment| {
            Ok(PrReviewThreadComment {
                id: required_json_string(comment, "/id", "reviewThread.comment.id")?,
                author: comment
                    .pointer("/author/login")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string(),
                body: required_json_string_allow_empty(
                    comment,
                    "/body",
                    "reviewThread.comment.body",
                )?,
                created_at: required_json_string(
                    comment,
                    "/createdAt",
                    "reviewThread.comment.createdAt",
                )?,
                url: required_json_string(comment, "/url", "reviewThread.comment.url")?,
            })
        })
        .collect()
}

fn reject_graphql_errors(value: &serde_json::Value, label: &str) -> Result<(), ForgeError> {
    if value
        .get("errors")
        .and_then(|errors| errors.as_array())
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(snapshot_incomplete(
            "GitHub returned partial review-thread data",
            Some(format!(
                "surface={label}; graphql_errors={}",
                value["errors"].as_array().map_or(0, Vec::len)
            )),
        ));
    }
    Ok(())
}

fn parse_page_info(
    connection: &serde_json::Value,
    label: &str,
) -> Result<(bool, Option<String>), ForgeError> {
    let page_info = connection.get("pageInfo").ok_or_else(|| {
        snapshot_incomplete(
            "review-thread response is missing pageInfo",
            Some(format!("connection={label}")),
        )
    })?;
    let has_next_page = page_info
        .get("hasNextPage")
        .and_then(|value| value.as_bool())
        .ok_or_else(|| {
            snapshot_incomplete(
                "review-thread pageInfo is missing hasNextPage",
                Some(format!("connection={label}")),
            )
        })?;
    let end_cursor = page_info
        .get("endCursor")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Ok((has_next_page, end_cursor))
}

fn required_json_string(
    value: &serde_json::Value,
    pointer: &str,
    field: &str,
) -> Result<String, ForgeError> {
    value
        .pointer(pointer)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            snapshot_incomplete(
                "review-thread response is missing a required field",
                Some(format!("field={field}")),
            )
        })
}

fn required_json_string_allow_empty(
    value: &serde_json::Value,
    pointer: &str,
    field: &str,
) -> Result<String, ForgeError> {
    value
        .pointer(pointer)
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            snapshot_incomplete(
                "review-thread response is missing a required field",
                Some(format!("field={field}")),
            )
        })
}

fn optional_json_u32(value: &serde_json::Value, pointer: &str) -> Option<u32> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn required_json_bool(
    value: &serde_json::Value,
    pointer: &str,
    field: &str,
) -> Result<bool, ForgeError> {
    value
        .pointer(pointer)
        .and_then(|value| value.as_bool())
        .ok_or_else(|| {
            snapshot_incomplete(
                "review-thread response is missing a required field",
                Some(format!("field={field}")),
            )
        })
}

fn snapshot_incomplete(message: &str, detail: Option<String>) -> ForgeError {
    ForgeError::validation(schema_err(), "review_snapshot_incomplete", message, detail)
}

/// A GitLab discussion is a review thread when it has at least one
/// `resolvable` note; it is resolved when every resolvable note is resolved.
/// Comment-only and system discussions are skipped — they are `pr comments`
/// material, not review threads.
fn parse_gitlab_discussions(
    output: &BackendSuccess,
    mr_url: &str,
) -> Result<Vec<PrReviewThreadSummary>, ForgeError> {
    let chunks = split_concatenated_arrays(&output.stdout)?;
    let mut out = Vec::new();
    for chunk in chunks {
        let items = chunk.as_array().cloned().unwrap_or_else(|| vec![chunk]);
        for discussion in items {
            let notes = discussion
                .get("notes")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let resolvable: Vec<&serde_json::Value> = notes
                .iter()
                .filter(|n| {
                    n.get("resolvable")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .collect();
            let Some(first) = resolvable.first() else {
                continue;
            };
            let resolved = resolvable
                .iter()
                .all(|n| n.get("resolved").and_then(|v| v.as_bool()).unwrap_or(false));
            let id = first
                .get("id")
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_default();
            // The thread handle is the discussion id (a string on GitLab), not
            // the per-note id used to anchor the URL fragment below.
            let discussion_id = discussion
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            out.push(PrReviewThreadSummary {
                id: discussion_id,
                resolved,
                outdated: false,
                author: first
                    .pointer("/author/username")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                path: first
                    .pointer("/position/new_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                diff_side: None,
                line: first
                    .pointer("/position/new_line")
                    .or_else(|| first.pointer("/position/old_line"))
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok()),
                original_line: None,
                original_start_line: None,
                start_diff_side: None,
                start_line: None,
                subject_type: first
                    .pointer("/position/position_type")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                created_at: first
                    .get("created_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                url: format!("{mr_url}#note_{id}"),
                body: first
                    .get("body")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                comments: resolvable
                    .iter()
                    .map(|note| PrReviewThreadComment {
                        id: note
                            .get("id")
                            .and_then(|value| value.as_u64())
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                        author: note
                            .pointer("/author/username")
                            .and_then(|value| value.as_str())
                            .unwrap_or("")
                            .to_string(),
                        body: note
                            .get("body")
                            .and_then(|value| value.as_str())
                            .unwrap_or("")
                            .to_string(),
                        created_at: note
                            .get("created_at")
                            .and_then(|value| value.as_str())
                            .unwrap_or("")
                            .to_string(),
                        url: note
                            .get("id")
                            .and_then(|value| value.as_u64())
                            .map(|id| format!("{mr_url}#note_{id}"))
                            .unwrap_or_default(),
                    })
                    .collect(),
            });
        }
    }
    Ok(out)
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrReviewThreadsPayload) {
    for line in render_text_lines(payload) {
        println!("{line}");
    }
}

fn render_text_lines(payload: &PrReviewThreadsPayload) -> Vec<String> {
    let mut lines = vec![format!(
        "{provider} #{number} ({total} review threads, {unresolved} unresolved)\n  {url}",
        provider = payload.provider,
        number = payload.number,
        total = payload.total,
        unresolved = payload.unresolved,
        url = payload.url,
    )];
    for thread in &payload.threads {
        let body_first_line = thread.body.lines().next().unwrap_or("");
        let state = if thread.resolved {
            "resolved"
        } else {
            "UNRESOLVED"
        };
        let id = if thread.id.is_empty() {
            String::new()
        } else {
            format!(" {id}", id = thread.id)
        };
        let anchor = if thread.path.is_empty() {
            String::new()
        } else {
            format!(" @ {path}", path = thread.path)
        };
        lines.push(format!(
            "  - [{state}]{id} {author}{anchor}: {body}",
            author = thread.author,
            body = body_first_line,
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use nils_common::cli_contract::OutputFormat;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::backend::BackendOutput;
    use crate::cli::GlobalFlags;
    use crate::provider::DetectionSource;

    /// `(program, argv)` captured at construction time — race-free against
    /// env-var binary overrides (see the note in `pr_comments::tests`).
    type RecordedCall = (BackendProgram, Vec<String>);

    struct ScriptedRunner {
        outputs: RefCell<Vec<BackendSuccess>>,
        calls: RefCell<Vec<RecordedCall>>,
    }

    impl ScriptedRunner {
        fn new(outputs: Vec<BackendSuccess>) -> Self {
            Self {
                outputs: RefCell::new(outputs),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<RecordedCall> {
            self.calls.borrow().clone()
        }
    }

    impl BackendRunner for ScriptedRunner {
        fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
            let argv = call
                .argv
                .iter()
                .map(|os| os.to_string_lossy().into_owned())
                .collect();
            self.calls.borrow_mut().push((call.program, argv));
            Ok(self.outputs.borrow_mut().remove(0))
        }

        fn run_raw(&self, call: &BackendCall) -> Result<BackendOutput, ForgeError> {
            self.run(call).map(|s| BackendOutput {
                exit_code: 0,
                status_success: true,
                stdout: s.stdout,
                stderr: s.stderr,
            })
        }
    }

    fn ctx(provider: Provider) -> ProviderContext {
        ProviderContext {
            provider,
            host: match provider {
                Provider::GitLab => "gitlab.com".into(),
                _ => "github.com".into(),
            },
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    fn github_threads_json(resolved_states: &[(bool, &str, &str)]) -> String {
        let nodes: Vec<String> = resolved_states
            .iter()
            .enumerate()
            .map(|(index, (resolved, author, path))| {
                format!(
                    r#"{{"id":"PRRT_{index}","isResolved":{resolved},"isOutdated":false,"path":"{path}","diffSide":"RIGHT","line":10,"originalLine":10,"originalStartLine":null,"startDiffSide":null,"startLine":null,"subjectType":"LINE","comments":{{"nodes":[{{"id":"PRRC_{index}","author":{{"login":"{author}"}},"body":"finding body\nsecond line","createdAt":"2026-06-11T04:49:36Z","url":"https://github.com/acme/widgets/pull/7#discussion_r1"}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}"#
                )
            })
            .collect();
        format!(
            r#"{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-7","reviewThreads":{{"nodes":[{nodes}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}"#,
            nodes = nodes.join(",")
        )
    }

    #[test]
    fn github_threads_call_uses_graphql_with_owner_name_pr() {
        let call = build_threads_call(
            &ctx(Provider::GitHub),
            "https://github.com/acme/widgets/pull/7",
            7,
        )
        .expect("call");
        let argv: Vec<String> = call
            .argv
            .iter()
            .map(|os| os.to_string_lossy().into_owned())
            .collect();
        assert_eq!(call.program, BackendProgram::Gh);
        assert!(argv.iter().any(|s| s == "graphql"));
        assert!(argv.iter().any(|s| s == "owner=acme"));
        assert!(argv.iter().any(|s| s == "name=widgets"));
        assert!(argv.iter().any(|s| s == "pr=7"));
        assert!(argv.iter().any(|s| s.contains("reviewThreads")));
    }

    #[test]
    fn github_threads_call_adds_hostname_for_enterprise_host() {
        let mut ctx = ctx(Provider::GitHub);
        ctx.host = "internal.ghe.com".into();
        let call = build_threads_call(&ctx, "https://internal.ghe.com/acme/widgets/pull/7", 7)
            .expect("call");
        let argv: Vec<String> = call
            .argv
            .iter()
            .map(|os| os.to_string_lossy().into_owned())
            .collect();
        let pos = argv
            .iter()
            .position(|s| s == "--hostname")
            .expect("enterprise host must be passed to gh api");
        assert_eq!(argv[pos + 1], "internal.ghe.com");
    }

    #[test]
    fn gitlab_threads_call_targets_discussions_from_nested_group_url() {
        let call = build_threads_call(
            &ctx(Provider::GitLab),
            "https://gitlab.example.com/group/sub/project/-/merge_requests/9",
            9,
        )
        .expect("call");
        let argv: Vec<String> = call
            .argv
            .iter()
            .map(|os| os.to_string_lossy().into_owned())
            .collect();
        assert_eq!(call.program, BackendProgram::Glab);
        assert!(argv.iter().any(|s| s == "--hostname"));
        assert!(argv.iter().any(|s| s == "gitlab.example.com"));
        assert!(argv.iter().any(
            |s| s == "projects/group%2Fsub%2Fproject/merge_requests/9/discussions?per_page=100"
        ));
    }

    #[test]
    fn parse_github_threads_populates_thread_node_id() {
        let output = BackendSuccess {
            stdout: serde_json::json!({
                "data": {"repository": {"pullRequest": {
                    "headRefOid": "head-7",
                    "reviewThreads": {
                        "nodes": [{
                            "id": "PRRT_kwDOExample1",
                            "isResolved": false,
                            "isOutdated": false,
                            "path": "src/lib.rs",
                            "diffSide": "RIGHT",
                            "line": 10,
                            "originalLine": 10,
                            "originalStartLine": null,
                            "startDiffSide": null,
                            "startLine": null,
                            "subjectType": "LINE",
                            "comments": {
                                "nodes": [{
                                    "id": "PRRC_1",
                                    "author": {"login": "quality-bot"},
                                    "body": "finding",
                                    "createdAt": "t",
                                    "url": "https://github.com/acme/widgets/pull/7#discussion_r1"
                                }],
                                "pageInfo": {"hasNextPage": false, "endCursor": null}
                            }
                        }],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }
                }}}
            })
            .to_string(),
            stderr: String::new(),
        };
        let threads = parse_github_threads(&output).expect("parse");
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "PRRT_kwDOExample1");
    }

    #[test]
    fn parse_github_threads_extracts_state_author_and_path() {
        let output = BackendSuccess {
            stdout: github_threads_json(&[
                (false, "quality-bot", "src/db/postgres.ts"),
                (true, "reviewer", "src/cli.rs"),
            ]),
            stderr: String::new(),
        };
        let threads = parse_github_threads(&output).expect("parse");
        assert_eq!(threads.len(), 2);
        assert!(!threads[0].resolved);
        assert_eq!(threads[0].author, "quality-bot");
        assert_eq!(threads[0].path, "src/db/postgres.ts");
        assert_eq!(threads[0].body, "finding body\nsecond line");
        assert!(threads[1].resolved);
    }

    #[test]
    fn github_snapshot_includes_later_thread_comment_pages() {
        let first_page = BackendSuccess {
            stdout: serde_json::json!({
                "data": {"repository": {"pullRequest": {
                    "headRefOid": "head-7",
                    "reviewThreads": {
                    "nodes": [{
                        "id": "PRRT_1",
                        "isResolved": false,
                        "isOutdated": false,
                        "path": "src/lib.rs",
                        "diffSide": "RIGHT",
                        "line": 10,
                        "originalLine": 10,
                        "originalStartLine": null,
                        "startDiffSide": null,
                        "startLine": null,
                        "subjectType": "LINE",
                        "comments": {
                            "nodes": [{
                                "id": "PRRC_1",
                                "author": {"login": "reviewer"},
                                "body": "initial finding",
                                "createdAt": "2026-07-20T12:00:00Z",
                                "url": "https://github.com/acme/widgets/pull/7#discussion_r1"
                            }],
                            "pageInfo": {"hasNextPage": true, "endCursor": "comment-page-2"}
                        }
                    }],
                    "pageInfo": {"hasNextPage": false, "endCursor": null}
                }}}}
            })
            .to_string(),
            stderr: String::new(),
        };
        let second_page = BackendSuccess {
            stdout: serde_json::json!({
                "data": {"node": {
                    "id": "PRRT_1",
                    "comments": {
                        "nodes": [{
                            "id": "PRRC_2",
                            "author": {"login": "reviewer"},
                            "body": "later blocking finding",
                            "createdAt": "2026-07-20T12:01:00Z",
                            "url": "https://github.com/acme/widgets/pull/7#discussion_r2"
                        }],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }
                }}
            })
            .to_string(),
            stderr: String::new(),
        };
        let runner = ScriptedRunner::new(vec![first_page, second_page]);

        let payload = compute_for_pr(
            &runner,
            &ctx(Provider::GitHub),
            "https://github.com/acme/widgets/pull/7",
            7,
        )
        .expect("complete snapshot");
        let value = serde_json::to_value(&payload).expect("serialize payload");

        assert_eq!(
            value["threads"][0]["comments"]
                .as_array()
                .expect("complete comment list")
                .len(),
            2
        );
        assert_eq!(
            value["threads"][0]["comments"][1]["body"],
            "later blocking finding"
        );
        assert_eq!(runner.calls().len(), 2);
    }

    #[test]
    fn parse_github_threads_errors_on_missing_nodes() {
        let output = BackendSuccess {
            stdout: r#"{"data":{"repository":{"pullRequest":null}}}"#.into(),
            stderr: String::new(),
        };
        let err = parse_github_threads(&output).expect_err("must fail");
        assert_eq!(err.kind(), "review_snapshot_incomplete");
    }

    #[test]
    fn parse_gitlab_discussions_skips_non_resolvable_and_tracks_resolution() {
        let output = BackendSuccess {
            stdout: r#"[
                {"id":"d1","notes":[{"id":11,"resolvable":false,"system":true,"body":"label added","author":{"username":"bot"},"created_at":"t"}]},
                {"id":"d2","notes":[
                    {"id":21,"resolvable":true,"resolved":false,"body":"please fix","author":{"username":"quality-bot"},"created_at":"2026-06-11T04:49:36Z","position":{"new_path":"src/lib.rs"}},
                    {"id":22,"resolvable":true,"resolved":false,"body":"agreed","author":{"username":"human"},"created_at":"t"}
                ]},
                {"id":"d3","notes":[{"id":31,"resolvable":true,"resolved":true,"body":"done","author":{"username":"human"},"created_at":"t"}]}
            ]"#
            .into(),
            stderr: String::new(),
        };
        let threads = parse_gitlab_discussions(
            &output,
            "https://gitlab.example.com/group/project/-/merge_requests/9",
        )
        .expect("parse");
        assert_eq!(threads.len(), 2);
        assert!(!threads[0].resolved);
        assert_eq!(threads[0].author, "quality-bot");
        assert_eq!(threads[0].path, "src/lib.rs");
        // The thread handle is the discussion id, not the per-note id.
        assert_eq!(threads[0].id, "d2");
        assert_eq!(threads[1].id, "d3");
        assert_eq!(
            threads[0].url,
            "https://gitlab.example.com/group/project/-/merge_requests/9#note_21"
        );
        assert!(threads[1].resolved);
    }

    #[test]
    fn render_text_includes_thread_id_for_follow_up_commands() {
        let payload = PrReviewThreadsPayload {
            provider: "github",
            number: 7,
            url: "https://github.com/acme/widgets/pull/7".into(),
            head_sha: Some("head-7".into()),
            total: 1,
            unresolved: 1,
            threads: vec![PrReviewThreadSummary {
                id: "PRRT_kwDOExample1".into(),
                resolved: false,
                outdated: false,
                author: "reviewer".into(),
                path: "src/lib.rs".into(),
                diff_side: Some("RIGHT".into()),
                line: Some(10),
                original_line: Some(10),
                original_start_line: None,
                start_diff_side: None,
                start_line: None,
                subject_type: Some("LINE".into()),
                created_at: "t".into(),
                url: "https://github.com/acme/widgets/pull/7#discussion_r1".into(),
                body: "finding body\nsecond line".into(),
                comments: Vec::new(),
            }],
        };

        let lines = render_text_lines(&payload);
        assert!(
            lines.iter().any(|line| line.contains("PRRT_kwDOExample1")),
            "text output must expose the value accepted by --thread: {lines:?}"
        );
    }

    #[test]
    fn ensure_resolves_ok_when_all_threads_resolved() {
        let view = BackendSuccess {
            stdout: github_threads_json(&[(true, "bot", "a.rs"), (true, "bot", "b.rs")]),
            stderr: String::new(),
        };
        let runner = ScriptedRunner::new(vec![view]);
        ensure_review_threads_resolved(
            &runner,
            &ctx(Provider::GitHub),
            "https://github.com/acme/widgets/pull/7",
            7,
        )
        .expect("resolved threads must pass");
        assert_eq!(runner.calls().len(), 1);
    }

    #[test]
    fn ensure_fails_data_65_with_listing_when_threads_unresolved() {
        let output = BackendSuccess {
            stdout: github_threads_json(&[
                (false, "quality-bot", "src/db/postgres.ts"),
                (true, "reviewer", ""),
            ]),
            stderr: String::new(),
        };
        let runner = ScriptedRunner::new(vec![output]);
        let err = ensure_review_threads_resolved(
            &runner,
            &ctx(Provider::GitHub),
            "https://github.com/acme/widgets/pull/7",
            7,
        )
        .expect_err("unresolved threads must fail");
        assert_eq!(err.kind(), "unresolved_review_threads");
        assert_eq!(err.exit_code(), 65);
        let rendered = format!("{err:?}");
        assert!(rendered.contains("quality-bot"));
        assert!(rendered.contains("--allow-unresolved-threads"));
    }

    #[test]
    fn ensure_passes_trivially_on_local_provider_without_backend_calls() {
        let runner = ScriptedRunner::new(vec![]);
        ensure_review_threads_resolved(&runner, &ctx(Provider::Local), "local://store/pull/3", 3)
            .expect("local provider must pass");
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn ensure_payload_resolved_does_not_block_on_outdated_and_records_stale() {
        let payload = PrReviewThreadsPayload {
            provider: "github",
            number: 7,
            url: "https://github.com/acme/widgets/pull/7".into(),
            head_sha: Some("head-7".into()),
            total: 1,
            unresolved: 1,
            threads: vec![PrReviewThreadSummary {
                id: "PRRT_outdated".into(),
                resolved: false,
                outdated: true,
                author: "quality-bot".into(),
                path: "src/lib.rs".into(),
                diff_side: Some("RIGHT".into()),
                line: Some(10),
                original_line: Some(10),
                original_start_line: None,
                start_diff_side: None,
                start_line: None,
                subject_type: Some("LINE".into()),
                created_at: "t".into(),
                url: "https://github.com/acme/widgets/pull/7#discussion_r1".into(),
                body: "nit: rename this local\nsecond line".into(),
                comments: Vec::new(),
            }],
        };
        // An outdated unresolved thread must not block the gate...
        ensure_payload_resolved(&payload).expect("outdated threads must not block");
        // ...but must be recorded as a stale disposition for auditability.
        let stale = stale_dispositions(&payload);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].thread_id, "PRRT_outdated");
        assert_eq!(stale[0].disposition, "stale");
        assert_eq!(stale[0].summary, "nit: rename this local");
        assert_eq!(stale[0].path, "src/lib.rs");
    }

    fn json_global() -> GlobalFlags {
        GlobalFlags {
            format: Some(OutputFormat::Json),
            remote: "origin".into(),
            provider: Some(crate::cli::ProviderFlag::Github),
            host: None,
            repo: Some("acme/widgets".into()),
            store_root: None,
            dry_run: false,
        }
    }

    #[test]
    fn run_with_github_emits_two_calls_and_counts_unresolved() {
        let view = BackendSuccess {
            stdout: r#"{"number":7,"url":"https://github.com/acme/widgets/pull/7","state":"OPEN","isDraft":false,"title":"demo","headRefName":"feat/x","baseRefName":"main","mergeable":"MERGEABLE","mergedAt":null,"labels":[]}"#.into(),
            stderr: String::new(),
        };
        let threads = BackendSuccess {
            stdout: github_threads_json(&[(false, "quality-bot", "a.rs"), (true, "human", "b.rs")]),
            stderr: String::new(),
        };
        let runner = ScriptedRunner::new(vec![view, threads]);
        let args = PrReviewThreadsListArgs { id: 7 };
        let code = run_with(&runner, &json_global(), args, OutputFormat::Json, |_| {
            Some("git@github.com:acme/widgets.git".into())
        })
        .expect("run");
        assert_eq!(code, 0);
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[1].1.iter().any(|s| s == "graphql"));
    }

    #[test]
    fn run_with_dry_run_plans_threads_call_without_backend_calls() {
        let view = BackendSuccess {
            stdout: r#"{"number":7,"url":"https://github.com/acme/widgets/pull/7","state":"OPEN","isDraft":false,"title":"demo","headRefName":"feat/x","baseRefName":"main","mergeable":"MERGEABLE","mergedAt":null,"labels":[]}"#.into(),
            stderr: String::new(),
        };
        let runner = ScriptedRunner::new(vec![view]);
        let mut global = json_global();
        global.dry_run = true;
        let args = PrReviewThreadsListArgs { id: 7 };
        let code = run_with(&runner, &global, args, OutputFormat::Json, |_| {
            Some("git@github.com:acme/widgets.git".into())
        })
        .expect("run");
        assert_eq!(code, 0);
        assert!(
            runner.calls().is_empty(),
            "dry-run must not invoke pr view or any live backend call"
        );
    }
}
