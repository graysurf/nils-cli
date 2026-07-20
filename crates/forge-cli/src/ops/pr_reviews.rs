//! `pr reviews` atom — native GitHub review summaries bound to a PR head.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::time::{Duration, Instant};

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload};
use crate::cli::{BINARY, GlobalFlags, PrReviewsArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_comments::github_repo_slug_from_url;
use crate::ops::pr_view;
use crate::ops::review_state;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;

pub const SCHEMA: &str = "pr.reviews";
pub const SCHEMA_VERSION: u32 = 1;
const MAX_SUMMARY_BYTES: usize = 4096;
const MAX_REVIEW_PAGES: usize = 100;
pub(crate) const MAX_PENDING_REVIEW_BODY_BYTES: usize = 64 * 1024;

const GITHUB_REVIEWS_QUERY: &str = "query($owner: String!, $name: String!, $pr: Int!, $after: String) { repository(owner: $owner, name: $name) { pullRequest(number: $pr) { headRefOid reviews(first: 100, after: $after) { nodes { id databaseId url author { login } state commit { oid } submittedAt body } pageInfo { hasNextPage endCursor } } } } }";
const GITHUB_PENDING_REVIEWS_QUERY: &str = "query($owner: String!, $name: String!, $pr: Int!, $after: String) { repository(owner: $owner, name: $name) { pullRequest(number: $pr) { headRefOid reviews(first: 100, after: $after, states: [PENDING]) { nodes { id url author { login } state commit { oid } body viewerDidAuthor viewerCanDelete } pageInfo { hasNextPage endCursor } } } } }";
const GITHUB_PENDING_REVIEW_TARGET_QUERY: &str = "query($review: ID!) { node(id: $review) { ... on PullRequestReview { id url author { login } state commit { oid } body viewerDidAuthor viewerCanDelete comments(first: 1) { totalCount } pullRequest { number url headRefOid } } } }";
const GITHUB_PENDING_REVIEW_SNAPSHOT_QUERY: &str = "query($review: ID!, $after: String) { node(id: $review) { ... on PullRequestReview { id url author { login } state commit { oid } body viewerDidAuthor viewerCanDelete comments(first: 100, after: $after) { totalCount nodes { id url author { login } body createdAt path line originalLine startLine originalStartLine subjectType } pageInfo { hasNextPage endCursor } } pullRequest { number url headRefOid } } } }";

struct ReviewPage {
    head_sha: String,
    reviews: Vec<NativeReviewSummary>,
    pending_reviews: Vec<PendingReviewSummary>,
    has_next_page: bool,
    end_cursor: Option<String>,
}

struct PendingReviewGuardPage {
    head_sha: String,
    reviews: Vec<PendingReviewGuard>,
    has_next_page: bool,
    end_cursor: Option<String>,
}

/// One provider-native review summary. `summary` is bounded to 4096 UTF-8
/// bytes so the read surface cannot accidentally become an unbounded comment
/// transport.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NativeReviewSummary {
    pub id: String,
    pub database_id: Option<u64>,
    pub url: String,
    pub author: String,
    pub state: String,
    pub commit_sha: String,
    pub submitted_at: String,
    pub summary: String,
    pub summary_truncated: bool,
}

/// One provider-valid draft review. Pending reviews have no `submittedAt` and
/// therefore remain separate from submitted current-head/stale activity.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PendingReviewSummary {
    pub id: String,
    pub database_id: Option<u64>,
    pub url: String,
    pub author: String,
    pub state: String,
    pub commit_sha: Option<String>,
    pub summary: String,
    pub summary_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingReviewGuard {
    pub id: String,
    pub url: String,
    pub author: String,
    pub commit_sha: Option<String>,
    pub body: String,
    pub viewer_did_author: bool,
    pub viewer_can_delete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingReviewGuardSnapshot {
    pub head_sha: String,
    pub reviews: Vec<PendingReviewGuard>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingReviewTarget {
    pub number: u64,
    pub pr_url: String,
    pub head_sha: String,
    pub inline_comment_count: u64,
    pub review: PendingReviewGuard,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PendingReviewInlineComment {
    pub id: String,
    pub url: String,
    pub author: String,
    pub body: String,
    pub semantic_body: String,
    pub created_at: String,
    pub path: String,
    pub line: Option<u32>,
    pub original_line: Option<u32>,
    pub start_line: Option<u32>,
    pub original_start_line: Option<u32>,
    pub subject_type: String,
    pub body_digest: String,
    pub review_run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PendingReviewSnapshot {
    pub number: u64,
    pub pr_url: String,
    pub head_sha: String,
    pub review_id: String,
    pub review_url: String,
    pub author: String,
    pub commit_sha: Option<String>,
    pub body: String,
    pub semantic_body: String,
    pub viewer_did_author: bool,
    pub viewer_can_delete: bool,
    pub review_run_id: Option<String>,
    pub provenance: &'static str,
    pub inline_comments: Vec<PendingReviewInlineComment>,
    pub snapshot_digest: String,
}

struct PendingReviewSnapshotPage {
    snapshot: PendingReviewSnapshot,
    total_count: u64,
    has_next_page: bool,
    end_cursor: Option<String>,
}

/// Envelope payload for `cli.forge-cli.pr.reviews.v1`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrReviewsPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub head_sha: String,
    pub current_head_reviews: Vec<NativeReviewSummary>,
    pub stale_reviews: Vec<NativeReviewSummary>,
    pub pending_reviews: Vec<PendingReviewSummary>,
}

pub fn run(
    global: &GlobalFlags,
    args: PrReviewsArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrReviewsArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        &remote_url_lookup,
    )?;
    ensure_github(&ctx)?;

    if global.dry_run {
        let slug = resolve_repo_slug(&ctx, &global.remote, &remote_url_lookup)?;
        let (owner, name) = split_slug(&slug)?;
        let call = build_github_reviews_call(&ctx, owner, name, args.id, None);
        let payload = DryRunPayload::new(ctx.provider, &call);
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            payload,
            format,
            |p| println!("would run: {}", p.plan.join(" ")),
        ));
    }

    let view_output = runner.run(&pr_view::build_view_call(&ctx, &args.id.to_string()))?;
    let view = pr_view::parse_view_output(&ctx, &view_output)?;
    let payload = compute_for_pr(runner, &ctx, view.number, &view.url)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        payload,
        format,
        render_text,
    ))
}

/// Fetch one native-review snapshot for a known GitHub PR URL. Merge
/// convergence reuses this seam so the standalone atom and merge gate parse
/// exactly the same provider payload.
pub fn compute_for_pr<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    number: u64,
    pr_url: &str,
) -> Result<PrReviewsPayload, ForgeError> {
    compute_for_pr_with_timeout(runner, ctx, number, pr_url, None)
}

/// Fetch only pending reviews and the provider-native viewer ownership/capability
/// fields required by exact-node recovery. Submitted-review parsing remains an
/// independent convergence concern and cannot block recovery.
pub(crate) fn compute_pending_guards_for_pr<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    number: u64,
    pr_url: &str,
) -> Result<PendingReviewGuardSnapshot, ForgeError> {
    compute_pending_guards_for_pr_matching(runner, ctx, number, pr_url, None)
}

/// Complete the pending-only pagination while retaining only the named review.
/// Every page and node is still parsed and validated, but unrelated bodies are
/// dropped before the next provider page is read.
pub(crate) fn compute_pending_guard_for_pr<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    number: u64,
    pr_url: &str,
    review_id: &str,
) -> Result<PendingReviewGuardSnapshot, ForgeError> {
    compute_pending_guards_for_pr_matching(runner, ctx, number, pr_url, Some(review_id))
}

fn compute_pending_guards_for_pr_matching<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    number: u64,
    pr_url: &str,
    review_id: Option<&str>,
) -> Result<PendingReviewGuardSnapshot, ForgeError> {
    ensure_github(ctx)?;
    let slug = github_repo_slug_from_url(pr_url).ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "unable to derive GitHub owner/repo from PR url",
            Some(format!("url={pr_url}")),
        )
    })?;
    let (owner, name) = split_slug(&slug)?;
    let mut cursor = None;
    let mut seen_cursors = BTreeSet::new();
    let mut expected_head = None;
    let mut reviews = Vec::new();

    for page_index in 0..MAX_REVIEW_PAGES {
        let output = runner.run(&build_github_pending_reviews_call(
            ctx,
            owner,
            name,
            number,
            cursor.as_deref(),
        ))?;
        let page = parse_github_pending_review_page(&output)?;
        if expected_head
            .as_deref()
            .is_some_and(|head| head != page.head_sha)
        {
            return Err(snapshot_incomplete(
                "the PR head changed while paginating pending reviews",
                Some(format!(
                    "expected_head={} provider_head={}",
                    expected_head.as_deref().unwrap_or("<missing>"),
                    page.head_sha
                )),
            ));
        }
        expected_head.get_or_insert(page.head_sha);
        for mut review in page.reviews {
            if review_id.is_some_and(|target| review.id != target) {
                continue;
            }
            if review_id.is_some() && !reviews.is_empty() {
                return Err(snapshot_incomplete(
                    "pending review pagination returned the target more than once",
                    Some(format!("review_id={}", review.id)),
                ));
            }
            if review_id.is_none() {
                // Native-review submission needs only viewer ownership and
                // delete capability counts. Do not retain every provider body
                // across the complete snapshot.
                review.body = String::new();
            }
            reviews.push(review);
        }
        if !page.has_next_page {
            return Ok(PendingReviewGuardSnapshot {
                head_sha: expected_head.unwrap_or_default(),
                reviews,
            });
        }
        let next = page.end_cursor.ok_or_else(|| {
            snapshot_incomplete(
                "pending review pagination is missing endCursor",
                Some(format!("page={}", page_index + 1)),
            )
        })?;
        if !seen_cursors.insert(next.clone()) {
            return Err(snapshot_incomplete(
                "pending review pagination repeated a cursor",
                Some(format!("page={}; cursor={next}", page_index + 1)),
            ));
        }
        cursor = Some(next);
    }

    Err(snapshot_incomplete(
        "pending review pagination exceeded the safety page limit",
        Some(format!("max_pages={MAX_REVIEW_PAGES}")),
    ))
}

/// Re-fetch one exact pending review immediately before destructive recovery.
/// This target-bound read prevents an early pagination page from becoming the
/// final mutation authority.
pub(crate) fn compute_pending_target<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    review_id: &str,
) -> Result<Option<PendingReviewTarget>, ForgeError> {
    ensure_github(ctx)?;
    let output = runner.run(&build_github_pending_review_target_call(ctx, review_id))?;
    parse_github_pending_review_target(&output)
}

/// Fetch one exact pending review including its complete inline-comment
/// manifest. Every page repeats and revalidates the review/head metadata so the
/// returned digest is safe to use as a recovery compare-and-swap input.
pub(crate) fn compute_pending_snapshot<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    review_id: &str,
) -> Result<Option<PendingReviewSnapshot>, ForgeError> {
    ensure_github(ctx)?;
    let mut cursor = None;
    let mut seen_cursors = BTreeSet::new();
    let mut snapshot: Option<PendingReviewSnapshot> = None;
    let mut expected_total = None;

    for page_index in 0..MAX_REVIEW_PAGES {
        let output = runner.run(&build_github_pending_review_target_page_call(
            ctx,
            review_id,
            cursor.as_deref(),
        ))?;
        let Some(mut page) = parse_github_pending_review_snapshot_page(&output)? else {
            return if snapshot.is_none() {
                Ok(None)
            } else {
                Err(snapshot_incomplete(
                    "pending review disappeared while paginating inline comments",
                    Some(format!("review_id={review_id}")),
                ))
            };
        };
        if let Some(existing) = snapshot.as_mut() {
            validate_snapshot_page_identity(existing, &page.snapshot)?;
            existing
                .inline_comments
                .append(&mut page.snapshot.inline_comments);
        } else {
            expected_total = Some(page.total_count);
            snapshot = Some(page.snapshot);
        }
        if !page.has_next_page {
            let mut snapshot = snapshot.expect("page created a snapshot");
            if snapshot.inline_comments.len() as u64 != expected_total.unwrap_or(0) {
                return Err(snapshot_incomplete(
                    "pending review inline-comment snapshot is incomplete",
                    Some(format!(
                        "review_id={review_id}; expected_comments={}; observed_comments={}",
                        expected_total.unwrap_or(0),
                        snapshot.inline_comments.len()
                    )),
                ));
            }
            classify_pending_snapshot(&mut snapshot)?;
            snapshot.snapshot_digest = pending_snapshot_digest(&snapshot)?;
            return Ok(Some(snapshot));
        }
        let next = page.end_cursor.ok_or_else(|| {
            snapshot_incomplete(
                "pending review comment pagination is missing endCursor",
                Some(format!("review_id={review_id}; page={}", page_index + 1)),
            )
        })?;
        if !seen_cursors.insert(next.clone()) {
            return Err(snapshot_incomplete(
                "pending review comment pagination repeated a cursor",
                Some(format!("review_id={review_id}; cursor={next}")),
            ));
        }
        cursor = Some(next);
    }
    Err(snapshot_incomplete(
        "pending review comment pagination exceeded the safety page limit",
        Some(format!(
            "review_id={review_id}; max_pages={MAX_REVIEW_PAGES}"
        )),
    ))
}

/// Deadline-aware variant used by merge convergence. The timeout covers the
/// complete paginated snapshot, including rate-limit preflight and subprocess
/// execution in deadline-aware runners.
pub fn compute_for_pr_with_timeout<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    number: u64,
    pr_url: &str,
    timeout: Option<Duration>,
) -> Result<PrReviewsPayload, ForgeError> {
    ensure_github(ctx)?;
    let slug = github_repo_slug_from_url(pr_url).ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "unable to derive GitHub owner/repo from PR url",
            Some(format!("url={pr_url}")),
        )
    })?;
    let (owner, name) = split_slug(&slug)?;
    let started = Instant::now();
    let mut cursor = None;
    let mut seen_cursors = BTreeSet::new();
    let mut expected_head = None;
    let mut current_head_reviews = Vec::new();
    let mut stale_reviews = Vec::new();
    let mut pending_reviews = Vec::new();

    for page_index in 0..MAX_REVIEW_PAGES {
        let remaining = remaining_timeout(timeout, started)?;
        let output = runner.run_with_timeout(
            &build_github_reviews_call(ctx, owner, name, number, cursor.as_deref()),
            remaining,
        )?;
        let page = parse_github_review_page(&output)?;
        if expected_head
            .as_deref()
            .is_some_and(|head| head != page.head_sha)
        {
            return Err(snapshot_incomplete(
                "the PR head changed while paginating native reviews",
                Some(format!(
                    "expected_head={} provider_head={}",
                    expected_head.as_deref().unwrap_or("<missing>"),
                    page.head_sha
                )),
            ));
        }
        let head_sha = expected_head.get_or_insert(page.head_sha);
        pending_reviews.extend(page.pending_reviews);
        for review in page.reviews {
            if review.commit_sha == head_sha.as_str() {
                current_head_reviews.push(review);
            } else {
                stale_reviews.push(review);
            }
        }
        if !page.has_next_page {
            return Ok(PrReviewsPayload {
                provider: ctx.provider.as_str(),
                number,
                url: pr_url.to_string(),
                head_sha: expected_head.unwrap_or_default(),
                current_head_reviews,
                stale_reviews,
                pending_reviews,
            });
        }
        let next = page.end_cursor.ok_or_else(|| {
            snapshot_incomplete(
                "native review pagination is missing endCursor",
                Some(format!("page={}", page_index + 1)),
            )
        })?;
        if !seen_cursors.insert(next.clone()) {
            return Err(snapshot_incomplete(
                "native review pagination repeated a cursor",
                Some(format!("page={}; cursor={next}", page_index + 1)),
            ));
        }
        cursor = Some(next);
    }

    Err(snapshot_incomplete(
        "native review pagination exceeded the safety page limit",
        Some(format!("max_pages={MAX_REVIEW_PAGES}")),
    ))
}

fn ensure_github(ctx: &ProviderContext) -> Result<(), ForgeError> {
    if matches!(ctx.provider, Provider::GitHub) {
        return Ok(());
    }
    Err(ForgeError::provider_unsupported(
        schema_err(),
        format!(
            "pr reviews is GitHub-only in v1 (provider: {})",
            ctx.provider.as_str()
        ),
        None,
    ))
}

pub(crate) fn build_github_reviews_call(
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
        OsString::from(format!("query={GITHUB_REVIEWS_QUERY}")),
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

pub(crate) fn build_github_pending_reviews_call(
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
        OsString::from(format!("query={GITHUB_PENDING_REVIEWS_QUERY}")),
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

pub(crate) fn build_github_pending_review_target_call(
    ctx: &ProviderContext,
    review_id: &str,
) -> BackendCall {
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from("-f"),
        OsString::from(format!("query={GITHUB_PENDING_REVIEW_TARGET_QUERY}")),
        OsString::from("-f"),
        OsString::from(format!("review={review_id}")),
    ]);
    BackendCall::new(BackendProgram::Gh, argv)
}

pub(crate) fn build_github_pending_review_target_page_call(
    ctx: &ProviderContext,
    review_id: &str,
    after: Option<&str>,
) -> BackendCall {
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from("-f"),
        OsString::from(format!("query={GITHUB_PENDING_REVIEW_SNAPSHOT_QUERY}")),
        OsString::from("-f"),
        OsString::from(format!("review={review_id}")),
    ]);
    if let Some(after) = after {
        argv.extend([
            OsString::from("-f"),
            OsString::from(format!("after={after}")),
        ]);
    }
    BackendCall::new(BackendProgram::Gh, argv)
}

fn parse_github_review_page(output: &BackendSuccess) -> Result<ReviewPage, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|err| {
        ForgeError::software(
            schema_err(),
            "reviews response is invalid JSON",
            Some(err.to_string()),
        )
    })?;
    if value
        .get("errors")
        .and_then(|errors| errors.as_array())
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(snapshot_incomplete(
            "GitHub returned partial native-review data",
            Some(format!(
                "graphql_errors={}",
                value["errors"].as_array().map_or(0, Vec::len)
            )),
        ));
    }
    let pull = value
        .pointer("/data/repository/pullRequest")
        .ok_or_else(|| snapshot_incomplete("reviews response is missing pullRequest", None))?;
    let head_sha = required_string(pull, "/headRefOid", "headRefOid")?;
    let nodes = pull
        .pointer("/reviews/nodes")
        .and_then(|item| item.as_array())
        .ok_or_else(|| snapshot_incomplete("reviews response is missing reviews nodes", None))?;
    let page_info = pull
        .pointer("/reviews/pageInfo")
        .ok_or_else(|| snapshot_incomplete("reviews response is missing pageInfo", None))?;
    let has_next_page = page_info
        .get("hasNextPage")
        .and_then(|item| item.as_bool())
        .ok_or_else(|| snapshot_incomplete("reviews pageInfo is missing hasNextPage", None))?;
    let end_cursor = page_info
        .get("endCursor")
        .and_then(|item| item.as_str())
        .filter(|item| !item.is_empty())
        .map(str::to_string);

    let mut reviews = Vec::with_capacity(nodes.len());
    let mut pending_reviews = Vec::new();
    for node in nodes {
        let body = node
            .get("body")
            .and_then(|item| item.as_str())
            .unwrap_or("");
        let (summary, summary_truncated) = bounded_summary(body);
        let id = required_string(node, "/id", "review.id")?;
        let database_id = node.get("databaseId").and_then(|item| item.as_u64());
        let url = required_string(node, "/url", "review.url")?;
        let author = string_at(node, "/author/login");
        let state = required_review_state(node)?;
        if state == "PENDING" {
            pending_reviews.push(PendingReviewSummary {
                id,
                database_id,
                url,
                author,
                state,
                commit_sha: optional_string(node, "/commit/oid"),
                summary,
                summary_truncated,
            });
            continue;
        }
        let commit_sha = required_string(node, "/commit/oid", "review.commit.oid")?;
        let review = NativeReviewSummary {
            id,
            database_id,
            url,
            author,
            state,
            commit_sha,
            submitted_at: required_string(node, "/submittedAt", "review.submittedAt")?,
            summary,
            summary_truncated,
        };
        reviews.push(review);
    }

    Ok(ReviewPage {
        head_sha,
        reviews,
        pending_reviews,
        has_next_page,
        end_cursor,
    })
}

fn parse_github_pending_review_page(
    output: &BackendSuccess,
) -> Result<PendingReviewGuardPage, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|err| {
        ForgeError::software(
            schema_err(),
            "pending reviews response is invalid JSON",
            Some(err.to_string()),
        )
    })?;
    if value
        .get("errors")
        .and_then(|errors| errors.as_array())
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(snapshot_incomplete(
            "GitHub returned partial pending-review data",
            Some(format!(
                "graphql_errors={}",
                value["errors"].as_array().map_or(0, Vec::len)
            )),
        ));
    }
    let pull = value
        .pointer("/data/repository/pullRequest")
        .ok_or_else(|| {
            snapshot_incomplete("pending reviews response is missing pullRequest", None)
        })?;
    let head_sha = required_string(pull, "/headRefOid", "headRefOid")?;
    let nodes = pull
        .pointer("/reviews/nodes")
        .and_then(|item| item.as_array())
        .ok_or_else(|| {
            snapshot_incomplete("pending reviews response is missing reviews nodes", None)
        })?;
    let page_info = pull
        .pointer("/reviews/pageInfo")
        .ok_or_else(|| snapshot_incomplete("pending reviews response is missing pageInfo", None))?;
    let has_next_page = page_info
        .get("hasNextPage")
        .and_then(|item| item.as_bool())
        .ok_or_else(|| {
            snapshot_incomplete("pending reviews pageInfo is missing hasNextPage", None)
        })?;
    let end_cursor = optional_string(page_info, "/endCursor");

    let reviews = nodes
        .iter()
        .map(|node| {
            let id = required_string(node, "/id", "review.id")?;
            let state = required_review_state(node)?;
            if state != "PENDING" {
                return Err(snapshot_incomplete(
                    "pending-only review query returned a non-pending review",
                    Some(format!("review_id={id}; state={state}")),
                ));
            }
            let viewer_did_author =
                required_bool(node, "/viewerDidAuthor", "review.viewerDidAuthor")?;
            let viewer_can_delete =
                required_bool(node, "/viewerCanDelete", "review.viewerCanDelete")?;
            let author = optional_string(node, "/author/login");
            if viewer_did_author && author.is_none() {
                return Err(snapshot_incomplete(
                    "a viewer-owned pending review is missing author login",
                    Some(format!("review_id={id}")),
                ));
            }
            let body = required_pending_review_body(node, &id)?;
            Ok(PendingReviewGuard {
                id,
                url: required_string(node, "/url", "review.url")?,
                author: author.unwrap_or_else(|| "<unknown>".to_string()),
                commit_sha: optional_string(node, "/commit/oid"),
                body,
                viewer_did_author,
                viewer_can_delete,
            })
        })
        .collect::<Result<Vec<_>, ForgeError>>()?;

    Ok(PendingReviewGuardPage {
        head_sha,
        reviews,
        has_next_page,
        end_cursor,
    })
}

fn parse_github_pending_review_target(
    output: &BackendSuccess,
) -> Result<Option<PendingReviewTarget>, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|err| {
        ForgeError::software(
            schema_err(),
            "pending review target response is invalid JSON",
            Some(err.to_string()),
        )
    })?;
    if value
        .get("errors")
        .and_then(|errors| errors.as_array())
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(snapshot_incomplete(
            "GitHub returned partial pending-review target data",
            Some(format!(
                "graphql_errors={}",
                value["errors"].as_array().map_or(0, Vec::len)
            )),
        ));
    }
    let node = value.pointer("/data/node").ok_or_else(|| {
        snapshot_incomplete("pending review target response is missing node", None)
    })?;
    if node.is_null() {
        return Ok(None);
    }
    let id = required_string(node, "/id", "review.id")?;
    let state = required_review_state(node)?;
    if state != "PENDING" {
        return Ok(None);
    }
    let viewer_did_author = required_bool(node, "/viewerDidAuthor", "review.viewerDidAuthor")?;
    let viewer_can_delete = required_bool(node, "/viewerCanDelete", "review.viewerCanDelete")?;
    let author = optional_string(node, "/author/login");
    if viewer_did_author && author.is_none() {
        return Err(snapshot_incomplete(
            "a viewer-owned pending review is missing author login",
            Some(format!("review_id={id}")),
        ));
    }
    let body = required_pending_review_body(node, &id)?;

    Ok(Some(PendingReviewTarget {
        number: required_u64(node, "/pullRequest/number", "review.pullRequest.number")?,
        pr_url: required_string(node, "/pullRequest/url", "review.pullRequest.url")?,
        head_sha: required_string(
            node,
            "/pullRequest/headRefOid",
            "review.pullRequest.headRefOid",
        )?,
        inline_comment_count: required_u64(
            node,
            "/comments/totalCount",
            "review.comments.totalCount",
        )?,
        review: PendingReviewGuard {
            id,
            url: required_string(node, "/url", "review.url")?,
            author: author.unwrap_or_else(|| "<unknown>".to_string()),
            commit_sha: optional_string(node, "/commit/oid"),
            body,
            viewer_did_author,
            viewer_can_delete,
        },
    }))
}

fn parse_github_pending_review_snapshot_page(
    output: &BackendSuccess,
) -> Result<Option<PendingReviewSnapshotPage>, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|err| {
        ForgeError::software(
            schema_err(),
            "pending review snapshot response is invalid JSON",
            Some(err.to_string()),
        )
    })?;
    if value
        .get("errors")
        .and_then(|errors| errors.as_array())
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(snapshot_incomplete(
            "GitHub returned partial pending-review snapshot data",
            Some(format!(
                "graphql_errors={}",
                value["errors"].as_array().map_or(0, Vec::len)
            )),
        ));
    }
    let node = value.pointer("/data/node").ok_or_else(|| {
        snapshot_incomplete("pending review snapshot response is missing node", None)
    })?;
    if node.is_null() || required_review_state(node)? != "PENDING" {
        return Ok(None);
    }
    let comments = node.pointer("/comments").ok_or_else(|| {
        snapshot_incomplete("pending review snapshot response is missing comments", None)
    })?;
    let comment_nodes = comments
        .get("nodes")
        .and_then(|nodes| nodes.as_array())
        .ok_or_else(|| {
            snapshot_incomplete(
                "pending review snapshot response is missing comment nodes",
                None,
            )
        })?;
    let inline_comments = comment_nodes
        .iter()
        .map(|comment| {
            let body = required_string_allow_empty(comment, "/body", "review.comment.body")?;
            let semantic_body = review_state::strip_owned_markers(&body);
            let body_digest = review_state::sha256_digest(semantic_body.as_bytes());
            let line = optional_u32(comment, "/line");
            Ok(PendingReviewInlineComment {
                id: required_string(comment, "/id", "review.comment.id")?,
                url: required_string(comment, "/url", "review.comment.url")?,
                author: optional_string(comment, "/author/login")
                    .unwrap_or_else(|| "<unknown>".to_string()),
                review_run_id: review_state::parse_finding_marker(&body).map(|(run_id, _)| run_id),
                body,
                semantic_body,
                created_at: required_string(comment, "/createdAt", "review.comment.createdAt")?,
                path: optional_string(comment, "/path").unwrap_or_default(),
                line,
                original_line: optional_u32(comment, "/originalLine"),
                start_line: optional_u32(comment, "/startLine"),
                original_start_line: optional_u32(comment, "/originalStartLine"),
                subject_type: optional_string(comment, "/subjectType")
                    .unwrap_or_else(|| if line.is_some() { "LINE" } else { "FILE" }.to_string()),
                body_digest,
            })
        })
        .collect::<Result<Vec<_>, ForgeError>>()?;
    let page_info = comments.get("pageInfo").ok_or_else(|| {
        snapshot_incomplete(
            "pending review snapshot response is missing comment pageInfo",
            None,
        )
    })?;
    let has_next_page = page_info
        .get("hasNextPage")
        .and_then(|value| value.as_bool())
        .ok_or_else(|| {
            snapshot_incomplete(
                "pending review comment pageInfo is missing hasNextPage",
                None,
            )
        })?;
    let body = required_pending_review_body(node, &required_string(node, "/id", "review.id")?)?;
    let semantic_body = review_state::strip_owned_markers(&body);
    Ok(Some(PendingReviewSnapshotPage {
        total_count: required_u64(node, "/comments/totalCount", "review.comments.totalCount")?,
        has_next_page,
        end_cursor: optional_string(page_info, "/endCursor"),
        snapshot: PendingReviewSnapshot {
            number: required_u64(node, "/pullRequest/number", "review.pullRequest.number")?,
            pr_url: required_string(node, "/pullRequest/url", "review.pullRequest.url")?,
            head_sha: required_string(
                node,
                "/pullRequest/headRefOid",
                "review.pullRequest.headRefOid",
            )?,
            review_id: required_string(node, "/id", "review.id")?,
            review_url: required_string(node, "/url", "review.url")?,
            author: optional_string(node, "/author/login")
                .unwrap_or_else(|| "<unknown>".to_string()),
            commit_sha: optional_string(node, "/commit/oid"),
            review_run_id: review_state::parse_review_run_id(&body),
            body,
            semantic_body,
            viewer_did_author: required_bool(node, "/viewerDidAuthor", "review.viewerDidAuthor")?,
            viewer_can_delete: required_bool(node, "/viewerCanDelete", "review.viewerCanDelete")?,
            provenance: "unmarked",
            inline_comments,
            snapshot_digest: String::new(),
        },
    }))
}

fn validate_snapshot_page_identity(
    expected: &PendingReviewSnapshot,
    observed: &PendingReviewSnapshot,
) -> Result<(), ForgeError> {
    if expected.number == observed.number
        && expected.pr_url == observed.pr_url
        && expected.head_sha == observed.head_sha
        && expected.review_id == observed.review_id
        && expected.review_url == observed.review_url
        && expected.author == observed.author
        && expected.commit_sha == observed.commit_sha
        && expected.body == observed.body
        && expected.viewer_did_author == observed.viewer_did_author
        && expected.viewer_can_delete == observed.viewer_can_delete
    {
        return Ok(());
    }
    Err(snapshot_incomplete(
        "pending review metadata changed while paginating inline comments",
        Some(format!("review_id={}", expected.review_id)),
    ))
}

fn classify_pending_snapshot(snapshot: &mut PendingReviewSnapshot) -> Result<(), ForgeError> {
    let body_run = review_state::parse_review_run_id(&snapshot.body);
    let mut comment_runs = BTreeSet::new();
    for comment in &snapshot.inline_comments {
        if let Some((run_id, marker_digest)) = review_state::parse_finding_marker(&comment.body) {
            if marker_digest != comment.body_digest {
                return Err(ForgeError::validation(
                    schema_err(),
                    "pending_review_manifest_mismatch",
                    "an inline review marker does not match its normalized body",
                    Some(format!("comment_id={}", comment.id)),
                ));
            }
            comment_runs.insert(run_id);
        } else if body_run.is_some() {
            return Err(ForgeError::validation(
                schema_err(),
                "pending_review_manifest_mismatch",
                "a receipt-bound pending review contains an unmarked inline comment",
                Some(format!("comment_id={}", comment.id)),
            ));
        }
    }
    match body_run {
        Some(run_id)
            if comment_runs.is_empty()
                || (comment_runs.len() == 1 && comment_runs.contains(&run_id)) =>
        {
            snapshot.review_run_id = Some(run_id);
            snapshot.provenance = "receipt-bound";
            Ok(())
        }
        Some(run_id) => Err(ForgeError::validation(
            schema_err(),
            "pending_review_manifest_mismatch",
            "pending review body and inline comments carry different review runs",
            Some(format!("body_run={run_id}; comment_runs={comment_runs:?}")),
        )),
        None if comment_runs.is_empty() => {
            snapshot.review_run_id = None;
            snapshot.provenance = "unmarked";
            Ok(())
        }
        None => Err(ForgeError::validation(
            schema_err(),
            "pending_review_manifest_mismatch",
            "pending review inline comments are marked but the review body is not",
            Some(format!("comment_runs={comment_runs:?}")),
        )),
    }
}

fn pending_snapshot_digest(snapshot: &PendingReviewSnapshot) -> Result<String, ForgeError> {
    #[derive(Serialize)]
    struct SnapshotPreimage<'a> {
        number: u64,
        pr_url: &'a str,
        head_sha: &'a str,
        review_id: &'a str,
        review_url: &'a str,
        author: &'a str,
        commit_sha: &'a Option<String>,
        body: &'a str,
        viewer_did_author: bool,
        viewer_can_delete: bool,
        inline_comments: &'a [PendingReviewInlineComment],
    }
    let bytes = serde_json::to_vec(&SnapshotPreimage {
        number: snapshot.number,
        pr_url: &snapshot.pr_url,
        head_sha: &snapshot.head_sha,
        review_id: &snapshot.review_id,
        review_url: &snapshot.review_url,
        author: &snapshot.author,
        commit_sha: &snapshot.commit_sha,
        body: &snapshot.body,
        viewer_did_author: snapshot.viewer_did_author,
        viewer_can_delete: snapshot.viewer_can_delete,
        inline_comments: &snapshot.inline_comments,
    })
    .map_err(|error| {
        ForgeError::software(
            schema_err(),
            "failed to serialize pending-review snapshot digest",
            Some(error.to_string()),
        )
    })?;
    Ok(review_state::sha256_digest(&bytes))
}

fn required_pending_review_body(
    node: &serde_json::Value,
    review_id: &str,
) -> Result<String, ForgeError> {
    let body = required_string_allow_empty(node, "/body", "review.body")?;
    if body.len() > MAX_PENDING_REVIEW_BODY_BYTES {
        return Err(snapshot_incomplete(
            "pending review body exceeds the recovery safety limit",
            Some(format!(
                "review_id={review_id}; body_bytes={}; max_bytes={MAX_PENDING_REVIEW_BODY_BYTES}",
                body.len()
            )),
        ));
    }
    Ok(body)
}

fn required_review_state(node: &serde_json::Value) -> Result<String, ForgeError> {
    let state = required_string(node, "/state", "review.state")?;
    if matches!(
        state.as_str(),
        "APPROVED" | "CHANGES_REQUESTED" | "COMMENTED" | "DISMISSED" | "PENDING"
    ) {
        return Ok(state);
    }
    Err(snapshot_incomplete(
        "reviews response contains an unknown review state",
        Some(format!(
            "review_id={}; state={state}",
            string_at(node, "/id")
        )),
    ))
}

fn required_string(
    value: &serde_json::Value,
    pointer: &str,
    field: &str,
) -> Result<String, ForgeError> {
    value
        .pointer(pointer)
        .and_then(|item| item.as_str())
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            snapshot_incomplete(
                "reviews response is missing a required field",
                Some(format!("field={field}")),
            )
        })
}

fn required_string_allow_empty(
    value: &serde_json::Value,
    pointer: &str,
    field: &str,
) -> Result<String, ForgeError> {
    value
        .pointer(pointer)
        .and_then(|item| item.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            snapshot_incomplete(
                "reviews response is missing a required field",
                Some(format!("field={field}")),
            )
        })
}

fn required_bool(
    value: &serde_json::Value,
    pointer: &str,
    field: &str,
) -> Result<bool, ForgeError> {
    value
        .pointer(pointer)
        .and_then(|item| item.as_bool())
        .ok_or_else(|| {
            snapshot_incomplete(
                "reviews response is missing a required field",
                Some(format!("field={field}")),
            )
        })
}

fn required_u64(value: &serde_json::Value, pointer: &str, field: &str) -> Result<u64, ForgeError> {
    value
        .pointer(pointer)
        .and_then(|item| item.as_u64())
        .ok_or_else(|| {
            snapshot_incomplete(
                "reviews response is missing a required field",
                Some(format!("field={field}")),
            )
        })
}

fn optional_string(value: &serde_json::Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(|item| item.as_str())
        .filter(|item| !item.is_empty())
        .map(str::to_string)
}

fn optional_u32(value: &serde_json::Value, pointer: &str) -> Option<u32> {
    value
        .pointer(pointer)
        .and_then(|item| item.as_u64())
        .and_then(|item| u32::try_from(item).ok())
}

fn remaining_timeout(
    timeout: Option<Duration>,
    started: Instant,
) -> Result<Option<Duration>, ForgeError> {
    let Some(timeout) = timeout else {
        return Ok(None);
    };
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(ForgeError::unavailable(
            schema_err(),
            "backend_timeout",
            "native review snapshot exceeded its provider-call timeout",
            Some(format!("timeout_ms={}", timeout.as_millis())),
        ));
    }
    Ok(Some(remaining))
}

fn snapshot_incomplete(message: &str, detail: Option<String>) -> ForgeError {
    ForgeError::validation(schema_err(), "review_snapshot_incomplete", message, detail)
}

fn bounded_summary(value: &str) -> (String, bool) {
    if value.len() <= MAX_SUMMARY_BYTES {
        return (value.to_string(), false);
    }
    let mut end = MAX_SUMMARY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

fn string_at(value: &serde_json::Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .to_string()
}

pub(crate) fn resolve_repo_slug<F: Fn(&str) -> Option<String>>(
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
        "reviews dry-run requires --repo owner/name or a recognised forge remote",
        None,
    ))
}

pub(crate) fn split_slug(slug: &str) -> Result<(&str, &str), ForgeError> {
    slug.split_once('/').ok_or_else(|| {
        ForgeError::validation(
            schema_err(),
            "repo_required",
            "reviews require a repository slug shaped as owner/name",
            Some(format!("repo={slug}")),
        )
    })
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrReviewsPayload) {
    println!(
        "reviews for #{number} at {head}: {current} current, {stale} stale, {pending} pending\n  {url}",
        number = payload.number,
        head = payload.head_sha,
        current = payload.current_head_reviews.len(),
        stale = payload.stale_reviews.len(),
        pending = payload.pending_reviews.len(),
        url = payload.url,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn summary_is_truncated_on_a_utf8_boundary() {
        let value = format!("{}é", "a".repeat(MAX_SUMMARY_BYTES - 1));
        let (summary, truncated) = bounded_summary(&value);
        assert!(truncated);
        assert_eq!(summary.len(), MAX_SUMMARY_BYTES - 1);
        assert!(summary.is_char_boundary(summary.len()));
    }

    #[test]
    fn summary_at_exact_limit_is_not_truncated() {
        let input = "a".repeat(MAX_SUMMARY_BYTES);
        assert_eq!(bounded_summary(&input), (input, false));
    }
}
