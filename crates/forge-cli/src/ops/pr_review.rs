//! `pr review` atom.
//!
//! Spec / ops: `cli.forge-cli.pr.review.v1`. This is intentionally a posting
//! primitive, not a review-orchestration engine: callers pass an already
//! rendered review outcome, and forge-cli posts it to the PR/MR plus an
//! optional compact issue activity mirror.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    ffi::OsString,
    fs,
    io::Read,
};

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::{Deserialize, Serialize};

use crate::backend::{BackendCall, BackendProgram, BackendRunner};
use crate::cli::{
    BINARY, GlobalFlags, PrReviewArgs, PrReviewCommand, PrReviewDecision, PrReviewValidateArgs,
};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::{pr_comment, pr_review_threads, pr_reviews, review_state};
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;
use crate::validations::{no_escaped_control_markdown, no_local_path};

const SCHEMA: &str = "pr.review";
const SCHEMA_VERSION: u32 = 1;
const REVIEW_THREAD_FILE_MAX_BYTES: u64 = 256 * 1024;
const REVIEW_THREAD_MAX_COUNT: usize = 50;
const REVIEW_THREAD_PATH_MAX_BYTES: usize = 1024;
const REVIEW_THREAD_BODY_MAX_BYTES: usize = 16 * 1024;
const MAX_REVIEW_STATE_PAGES: usize = 100;
const MAX_REVIEW_STATE_PAGE_BYTES: usize = 8 * 1024 * 1024;

/// Placeholder `review_url` used only to validate the generated issue mirror
/// body *before* the PR comment is posted. The real URL is provider-returned
/// (never user-controlled), so validating the user-controlled parts — the
/// `--lens` values embedded in the mirror — against this placeholder is
/// sufficient to catch a bad lens before any backend mutation.
const MIRROR_URL_PENDING: &str = "<pending>";

const GITHUB_REVIEW_TARGET_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { id url } } }";
const GITHUB_ADD_PENDING_REVIEW_MUTATION: &str = "mutation($pullRequestId: ID!, $commitOID: GitObjectID!, $body: String) { addPullRequestReview(input: {pullRequestId: $pullRequestId, commitOID: $commitOID, body: $body}) { pullRequestReview { id url } } }";
const GITHUB_ADD_REVIEW_THREAD_MUTATION: &str = "mutation($reviewId: ID!, $path: String!, $body: String!, $line: Int, $side: DiffSide!, $startLine: Int, $startSide: DiffSide, $subjectType: PullRequestReviewThreadSubjectType!) { addPullRequestReviewThread(input: {pullRequestReviewId: $reviewId, path: $path, body: $body, line: $line, side: $side, startLine: $startLine, startSide: $startSide, subjectType: $subjectType}) { thread { id path line subjectType comments(first: 1) { nodes { url } } } } }";
const GITHUB_SUBMIT_REVIEW_MUTATION: &str = "mutation($reviewId: ID!, $event: PullRequestReviewEvent!, $body: String) { submitPullRequestReview(input: {pullRequestReviewId: $reviewId, event: $event, body: $body}) { pullRequestReview { url } } }";
const GITHUB_DELETE_PENDING_REVIEW_MUTATION: &str = "mutation($reviewId: ID!) { deletePullRequestReview(input: {pullRequestReviewId: $reviewId}) { pullRequestReview { id url } } }";
const GITHUB_REVIEW_STATE_COMMENTS_QUERY: &str = "query($owner: String!, $name: String!, $pr: Int!, $after: String) { viewer { login } repository(owner: $owner, name: $name) { pullRequest(number: $pr) { comments(first: 100, after: $after) { nodes { author { login } authorAssociation body createdAt } pageInfo { hasNextPage endCursor } } } } }";

/// Which `glab` review-note form to emit for a GitLab `pr review`, resolved by
/// probing the local `glab` build (see [`glab_note_form`]). Covers every glab
/// version class so the GitLab path works whether or not `mr note create` and
/// its `--resolvable` flag exist. Irrelevant for GitHub / Local.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlabNoteForm {
    /// `mr note create … --resolvable=false` — non-resolvable status note
    /// (modern glab; does not register on the merge gate).
    CreateResolvable,
    /// `mr note create … --message` — `create` exists but lacks `--resolvable`
    /// (the note stays resolvable; no worse than before this guard).
    Create,
    /// bare `mr note <id> --message` — this glab has no `mr note create`
    /// subcommand at all.
    BareNote,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrReviewPayload {
    pub provider: &'static str,
    pub number: u64,
    pub decision: &'static str,
    /// `true` when a native provider review event was submitted
    /// (`--submit-review`); `false` when an outcome comment was posted. When
    /// `true`, `pr_comment_url` holds the `#pullrequestreview-` object URL.
    pub submitted_review: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    pub pr_comment_url: String,
    pub issue_number: Option<u64>,
    pub issue_comment_url: Option<String>,
    pub mirrored: bool,
    pub lenses: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_threads: Vec<CreatedReviewThread>,
    /// Number of finding threads skipped as cross-run idempotent duplicates:
    /// a finding whose `(path, body)` already had a live (non-resolved,
    /// non-outdated) thread on the current head. Absent when zero. When every
    /// finding was a duplicate the review event itself is skipped, so
    /// `submitted_review` is `false` and `review_threads` is empty.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub threads_skipped_idempotent: usize,
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct PrReviewDryRunPayload {
    provider: &'static str,
    number: u64,
    decision: &'static str,
    /// `true` when the live run would submit a native review event
    /// (`--submit-review`); `false` for the outcome-comment form.
    submitted_review: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    head_sha: Option<String>,
    /// GitHub-only PR-existence guard read that runs before the live post.
    /// `None` on GitLab / Local. Surfaced so dry-run renders every backend
    /// command the live run performs.
    guard_plan: Option<Vec<String>>,
    /// GitHub-only pending-review ownership snapshot that runs before any
    /// native review mutation.
    pending_review_guard_plan: Option<Vec<String>>,
    /// Provider-visible durable state-chain read performed before recovery.
    review_state_plan: Option<Vec<String>>,
    /// Possible immutable receipt append performed before native review mutation.
    review_receipt_plan: Option<Vec<String>>,
    /// Tail/read-back verification performed after an append attempt.
    review_state_verify_plan: Option<Vec<String>>,
    plan: Vec<String>,
    issue_number: Option<u64>,
    issue_plan: Option<Vec<String>>,
    mirror_issue: bool,
    lenses: Vec<String>,
    planned_review_threads: usize,
    target_plan: Option<Vec<String>>,
    thread_plan: Vec<Vec<String>>,
    submit_plan: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct PrReviewValidatePayload {
    provider: &'static str,
    number: Option<u64>,
    check_diff: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff_plan: Option<Vec<String>>,
    comment: ReviewCommentValidation,
    review_threads: ReviewThreadValidation,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct ReviewCommentValidation {
    present: bool,
    bytes: usize,
    lines: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct ReviewThreadValidation {
    count: usize,
    diff_checked: bool,
    specs: Vec<ReviewThreadSpecPreview>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct ReviewThreadSpecPreview {
    index: usize,
    path: String,
    line: Option<u32>,
    side: &'static str,
    start_line: Option<u32>,
    start_side: Option<&'static str>,
    subject_type: &'static str,
    body_bytes: usize,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ReviewThreadDiffSide {
    Left,
    #[default]
    Right,
}

impl ReviewThreadDiffSide {
    fn as_str(self) -> &'static str {
        match self {
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ReviewThreadSubjectType {
    Line,
    File,
}

impl ReviewThreadSubjectType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Line => "LINE",
            Self::File => "FILE",
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct ReviewThreadSpec {
    path: String,
    body: String,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    side: Option<ReviewThreadDiffSide>,
    #[serde(default, alias = "startLine")]
    start_line: Option<u32>,
    #[serde(default, alias = "startSide")]
    start_side: Option<ReviewThreadDiffSide>,
    #[serde(default, alias = "subjectType")]
    subject_type: Option<ReviewThreadSubjectType>,
}

#[derive(Debug, Clone, PartialEq)]
struct PreparedReviewThreadSpec {
    path: String,
    body: String,
    line: Option<u32>,
    side: ReviewThreadDiffSide,
    start_line: Option<u32>,
    start_side: Option<ReviewThreadDiffSide>,
    subject_type: ReviewThreadSubjectType,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubPrFile {
    filename: String,
    #[serde(default)]
    patch: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CreatedReviewThread {
    pub id: String,
    pub url: String,
    pub path: String,
    pub line: Option<u32>,
    pub subject_type: String,
}

#[derive(Debug, Clone, PartialEq)]
struct GitHubReviewTarget {
    pull_request_id: String,
    url: String,
}

#[derive(Debug, Clone, PartialEq)]
struct GitHubPendingReview {
    review_id: String,
    url: String,
}

pub fn run(
    global: &GlobalFlags,
    args: PrReviewArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrReviewArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    if let Some(command) = args.command.clone() {
        return match command {
            PrReviewCommand::Validate(validate_args) => {
                let validate_args = validate_args_with_parent_fallbacks(&args, validate_args);
                run_validate_with(runner, global, validate_args, format, remote_url_lookup)
            }
        };
    }

    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    if ctx.provider == Provider::Local {
        return Err(ForgeError::provider_unsupported(
            schema_err(),
            "pr review is not supported by provider 'local' in v1",
            None,
        ));
    }
    let id = args.id.ok_or_else(review_id_required_err)?;

    let thread_specs_requested = args.thread_file.is_some();
    if thread_specs_requested && !args.submit_review {
        return Err(ForgeError::validation(
            schema_err(),
            "thread_file_requires_submit_review",
            "--thread-file requires --submit-review on github; omit --thread-file for a summary-only review",
            None,
        ));
    }
    if thread_specs_requested && ctx.provider != Provider::GitHub {
        return Err(ForgeError::provider_unsupported(
            schema_err(),
            "pr review --thread-file creates resolvable review threads on github only in v1; omit --thread-file for a summary-only review",
            None,
        ));
    }

    // Native review submission (the #pullrequestreview- object) is GitHub-only
    // in v1. GitLab has no equivalent single review event with an approve /
    // request-changes / comment verb, so `--submit-review` there would have no
    // faithful mapping; it keeps the outcome-comment (mr note) form instead.
    if args.submit_review && ctx.provider != Provider::GitHub {
        return Err(ForgeError::provider_unsupported(
            schema_err(),
            "pr review --submit-review (native review event) is only supported on github in v1; gitlab/local keep the outcome-comment form",
            None,
        ));
    }

    // `--mirror-issue` requires `--issue`. Resolve this BEFORE reading the
    // comment body or thread-file so a missing `--issue` fails fast with
    // `issue_required` rather than first blocking on stdin (`--comment-file -`
    // / `--thread-file -`) or surfacing a file-read `software_error`.
    // (`--mirror-issue` carries no clap `requires = "issue"`, so this runtime
    // guard is the only enforcement point.)
    let mirror_issue_number = if args.mirror_issue {
        Some(args.issue.ok_or_else(issue_required_err)?)
    } else {
        None
    };
    let thread_specs = if let Some(path) = args.thread_file.as_deref() {
        read_review_thread_specs(path)?
    } else {
        Vec::new()
    };

    let body = pr_comment::read_body_with_file_flag(
        args.comment.as_deref(),
        args.comment_file.as_deref(),
        "--comment-file",
    )?;
    let body_present = !body.trim().is_empty();
    // GitHub permits a body-less APPROVE review, so the empty-body guard is
    // relaxed only for a native approve submission. Every other case — outcome
    // comments, and native COMMENT / REQUEST_CHANGES reviews (GitHub requires a
    // body for both) — still needs a body.
    let body_required = !(args.submit_review && args.decision == PrReviewDecision::Approve);
    if !body_present && body_required {
        return Err(ForgeError::validation(
            schema_err(),
            "body_missing_summary",
            "review comment body is empty (supply --comment or --comment-file)",
            None,
        ));
    }
    if body_present {
        no_local_path(&body, "review comment")?;
        no_escaped_control_markdown(&body)?;
    }

    // Validate the generated issue-mirror body BEFORE any backend mutation
    // (validate-before-side-effect). It embeds user-controlled `--lens` values,
    // so it must hit the same `no_local_path` and escaped-control guards the
    // review body does — otherwise a bad lens either leaks a local path to the
    // provider issue or fails only after the PR comment was already posted,
    // leaving an outcome comment with no mirror.
    let mirror_issue = if let Some(issue_number) = mirror_issue_number {
        let preview = build_issue_mirror_body(
            ctx.provider,
            id,
            args.decision,
            &args.lenses,
            MIRROR_URL_PENDING,
        );
        no_local_path(&preview, "issue mirror")?;
        no_escaped_control_markdown(&preview)?;
        Some(issue_number)
    } else {
        None
    };

    let expected_review_head = if args.submit_review {
        Some(args.expected_head.as_deref().ok_or_else(|| {
            ForgeError::validation(
                schema_err(),
                "expected_review_head_required",
                "--submit-review requires --expected-head <SHA> so the native review cannot attach to an unreviewed PR head",
                None,
            )
        })?)
    } else {
        if args.expected_head.is_some() {
            return Err(ForgeError::validation(
                schema_err(),
                "expected_review_head_requires_submit_review",
                "--expected-head is only valid with --submit-review",
                None,
            ));
        }
        None
    };

    if global.dry_run {
        // dry-run must not touch a backend, so it cannot probe `glab` capability;
        // render the preferred non-resolvable GitLab form (the live run may pick
        // a more compatible form on older `glab`).
        let thread_plan = thread_specs
            .iter()
            .map(|spec| {
                build_github_add_review_thread_call(&ctx, "<pending-review-id>", spec).plan_argv()
            })
            .collect::<Vec<_>>();
        let (pr_call, target_plan, submit_plan) = if thread_specs.is_empty() {
            (
                build_review_post_call(
                    &ctx,
                    id,
                    &args,
                    &body,
                    body_present,
                    GlabNoteForm::CreateResolvable,
                ),
                None,
                None,
            )
        } else {
            let (owner, name) = github_owner_name(&ctx)?;
            (
                build_github_pending_review_call(
                    &ctx,
                    "<pull-request-id>",
                    expected_review_head.expect("validated native review head"),
                    body_present.then_some(body.as_str()),
                ),
                Some(build_github_review_target_call(&ctx, owner, name, id).plan_argv()),
                Some(
                    build_github_submit_review_call(
                        &ctx,
                        "<pending-review-id>",
                        args.decision.to_github_event(),
                        body_present.then_some(body.as_str()),
                    )
                    .plan_argv(),
                ),
            )
        };
        // The GitHub PR-existence guard is a live backend read; surface it in the
        // dry-run plan so wrappers inspecting dry-run output see every call.
        let guard_plan = (ctx.provider == Provider::GitHub).then(|| {
            BackendCall::new(BackendProgram::Gh, github_pull_lookup_argv(&ctx, id)).plan_argv()
        });
        let pending_review_guard_plan = if args.submit_review && ctx.provider == Provider::GitHub {
            let (owner, name) = github_owner_name(&ctx)?;
            Some(
                pr_reviews::build_github_pending_reviews_call(&ctx, owner, name, id, None)
                    .plan_argv(),
            )
        } else {
            None
        };
        let (review_state_plan, review_receipt_plan, review_state_verify_plan) = if !thread_specs
            .is_empty()
            && ctx.provider == Provider::GitHub
        {
            let repository = ctx.repo.as_deref().ok_or_else(|| {
                    ForgeError::validation(
                        schema_err(),
                        "repo_required",
                        "github threaded-review dry-run requires --repo owner/name or a recognised GitHub remote",
                        None,
                    )
                })?;
            let state_plan =
                build_github_review_state_comments_call(&ctx, repository, id).plan_argv();
            (
                    Some(state_plan.clone()),
                    Some(
                        build_issue_comment_call(
                            &ctx,
                            id,
                            "<!-- forge-cli:review-state:v1 <record-dependent-on-provider-chain-tip> -->",
                        )
                        .plan_argv(),
                    ),
                    Some(state_plan),
                )
        } else {
            (None, None, None)
        };
        let issue_plan = mirror_issue.map(|issue| {
            let mirror_body = build_issue_mirror_body(
                ctx.provider,
                id,
                args.decision,
                &args.lenses,
                "<pr-comment-url-unavailable-in-dry-run>",
            );
            build_issue_comment_call(&ctx, issue, &mirror_body).plan_argv()
        });
        return Ok(emit_success(
            schema_version(),
            PrReviewDryRunPayload {
                provider: ctx.provider.as_str(),
                number: id,
                decision: args.decision.as_str(),
                submitted_review: args.submit_review,
                head_sha: expected_review_head.map(str::to_string),
                guard_plan,
                pending_review_guard_plan,
                review_state_plan,
                review_receipt_plan,
                review_state_verify_plan,
                plan: pr_call.plan_argv(),
                issue_number: args.issue,
                issue_plan,
                mirror_issue: args.mirror_issue,
                lenses: args.lenses.clone(),
                planned_review_threads: thread_specs.len(),
                target_plan,
                thread_plan,
                submit_plan,
            },
            format,
            |p| {
                if let Some(guard) = p.guard_plan.as_ref() {
                    println!("would verify pull request: {plan}", plan = guard.join(" "));
                }
                if let Some(guard) = p.pending_review_guard_plan.as_ref() {
                    println!(
                        "would verify pending review ownership: {plan}",
                        plan = guard.join(" ")
                    );
                }
                if let Some(plan) = p.review_state_plan.as_ref() {
                    println!(
                        "would read review transaction state: {plan}",
                        plan = plan.join(" ")
                    );
                }
                if let Some(plan) = p.review_receipt_plan.as_ref() {
                    println!(
                        "would append immutable review receipt when absent: {plan}",
                        plan = plan.join(" ")
                    );
                }
                if let Some(plan) = p.review_state_verify_plan.as_ref() {
                    println!(
                        "would verify review transaction state: {plan}",
                        plan = plan.join(" ")
                    );
                }
                if let Some(target) = p.target_plan.as_ref() {
                    println!(
                        "would look up pull request node: {plan}",
                        plan = target.join(" ")
                    );
                }
                let verb = if p.submitted_review {
                    "would submit review event"
                } else {
                    "would post review outcome"
                };
                println!("{verb}: {plan}", plan = p.plan.join(" "));
                for thread_plan in &p.thread_plan {
                    println!(
                        "would create review thread: {plan}",
                        plan = thread_plan.join(" ")
                    );
                }
                if let Some(submit_plan) = p.submit_plan.as_ref() {
                    println!("would publish review: {plan}", plan = submit_plan.join(" "));
                }
                if let Some(issue_plan) = p.issue_plan.as_ref() {
                    println!(
                        "would mirror issue activity: {plan}",
                        plan = issue_plan.join(" ")
                    );
                }
            },
        ));
    }

    // GitHub posts review outcomes through the issue-comments API, which accepts
    // both issues and pull requests (every PR is an issue, but not every issue
    // is a PR). Verify `<id>` is actually a pull request first, so a typo'd or
    // non-PR number can't silently post a review outcome onto an unrelated
    // issue. GitLab's `glab mr note` already fails on a non-MR id, so this guard
    // is GitHub-only.
    if ctx.provider == Provider::GitHub {
        ensure_github_pull_request(runner, &ctx, id)?;
        if args.submit_review && thread_specs.is_empty() {
            ensure_no_viewer_pending_github_review(
                runner,
                &ctx,
                id,
                expected_review_head.expect("validated native review head"),
            )?;
        }
    }

    // GitLab only: probe which `mr note` form this `glab` build supports
    // (create+resolvable / create-only / bare). For GitHub / Local the form is
    // unused, so skip the probe and pass the default.
    let glab_form = if ctx.provider == Provider::GitLab {
        glab_note_form(runner)
    } else {
        GlabNoteForm::CreateResolvable
    };
    // `review_skipped_idempotent` is the explicit "no review event was posted
    // because every finding was already threaded" signal, set only by the threads
    // branch (where `submit_github_review_with_threads` returns `review_url:
    // None`). The summary-only branch always posts, so an empty URL there means
    // "provider URL not parsed", not "skipped" — the two must not be conflated.
    let (pr_comment_url, review_threads, threads_skipped_idempotent, review_skipped_idempotent) =
        if thread_specs.is_empty() {
            let pr_call = build_review_post_call(&ctx, id, &args, &body, body_present, glab_form);
            let pr_output = runner.run(&pr_call).map_err(|err| {
                if args.submit_review && ctx.provider == Provider::GitHub {
                    map_github_native_review_submit_error(args.decision, err)
                } else {
                    err
                }
            })?;
            (
                first_url(&pr_output.stdout).unwrap_or_default(),
                Vec::new(),
                0,
                false,
            )
        } else {
            let submission = submit_github_review_with_threads(
                runner,
                &ctx,
                GithubReviewThreadSubmissionRequest {
                    number: id,
                    decision: args.decision,
                    expected_head: expected_review_head.expect("validated native review head"),
                    body: body_present.then_some(body.as_str()),
                    specs: &thread_specs,
                    route_lenses: &args.lenses,
                },
            )?;
            let review_skipped_idempotent = !submission.submitted;
            (
                submission.review_url.unwrap_or_default(),
                submission.created,
                submission.skipped,
                review_skipped_idempotent,
            )
        };

    // When the review was skipped entirely as an idempotent no-op there is no
    // new PR activity to mirror, so skip the issue breadcrumb too.
    let issue_comment_url =
        if let Some(issue_number) = mirror_issue.filter(|_| !review_skipped_idempotent) {
            // The mirror body's user-controlled content (lenses) was already
            // validated up front against MIRROR_URL_PENDING; the only difference
            // here is the provider-returned `pr_comment_url`, which is never
            // user-controlled, so it needs no re-validation after the post.
            let mirror_body = build_issue_mirror_body(
                ctx.provider,
                id,
                args.decision,
                &args.lenses,
                &pr_comment_url,
            );
            let issue_call = build_issue_comment_call(&ctx, issue_number, &mirror_body);
            let issue_output = runner.run(&issue_call)?;
            first_url(&issue_output.stdout)
        } else {
            None
        };

    Ok(emit_success(
        schema_version(),
        PrReviewPayload {
            provider: ctx.provider.as_str(),
            number: id,
            decision: args.decision.as_str(),
            submitted_review: args.submit_review && !review_skipped_idempotent,
            head_sha: expected_review_head.map(str::to_string),
            pr_comment_url,
            issue_number: args.issue,
            issue_comment_url,
            mirrored: args.mirror_issue,
            lenses: args.lenses,
            review_threads,
            threads_skipped_idempotent,
        },
        format,
        render_text,
    ))
}

fn run_validate_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrReviewValidateArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    if args.check_diff && ctx.provider != Provider::GitHub {
        return Err(ForgeError::provider_unsupported(
            schema_err(),
            "pr review validate --check-diff is GitHub-only in v1",
            None,
        ));
    }
    let check_diff_id = if args.check_diff {
        let id = args.id.ok_or_else(review_validate_id_required_err)?;
        let _ = github_owner_name(&ctx)?;
        Some(id)
    } else {
        None
    };

    let thread_specs = if let Some(path) = args.thread_file.as_deref() {
        read_review_thread_specs(path)?
    } else {
        Vec::new()
    };
    let body = pr_comment::read_body_with_file_flag(
        args.comment.as_deref(),
        args.comment_file.as_deref(),
        "--comment-file",
    )?;
    let body_present = !body.trim().is_empty();
    if body_present {
        no_local_path(&body, "review comment")?;
        no_escaped_control_markdown(&body)?;
    }

    let mut diff_plan = None;
    let diff_checked = args.check_diff && !global.dry_run;
    if let Some(id) = check_diff_id {
        let call = build_github_pr_files_call(&ctx, id)?;
        if global.dry_run {
            diff_plan = Some(call.plan_argv());
        } else {
            validate_review_threads_against_github_diff(runner, &ctx, id, &thread_specs)?;
        }
    }

    let payload = PrReviewValidatePayload {
        provider: ctx.provider.as_str(),
        number: args.id,
        check_diff: args.check_diff,
        diff_plan,
        comment: ReviewCommentValidation {
            present: body_present,
            bytes: body.len(),
            lines: body.lines().count(),
        },
        review_threads: ReviewThreadValidation {
            count: thread_specs.len(),
            diff_checked,
            specs: thread_specs
                .iter()
                .enumerate()
                .map(|(idx, spec)| ReviewThreadSpecPreview {
                    index: idx + 1,
                    path: spec.path.clone(),
                    line: spec.line,
                    side: spec.side.as_str(),
                    start_line: spec.start_line,
                    start_side: spec.start_side.map(ReviewThreadDiffSide::as_str),
                    subject_type: spec.subject_type.as_str(),
                    body_bytes: spec.body.len(),
                })
                .collect(),
        },
    };

    Ok(emit_success(
        schema_version_for(BINARY, "pr.review.validate", 1),
        payload,
        format,
        |p| {
            println!(
                "review validation ok: provider={provider} comment_present={comment_present} review_threads={threads} diff_checked={diff_checked}",
                provider = p.provider,
                comment_present = p.comment.present,
                threads = p.review_threads.count,
                diff_checked = p.review_threads.diff_checked,
            );
        },
    ))
}

fn validate_args_with_parent_fallbacks(
    parent: &PrReviewArgs,
    mut validate_args: PrReviewValidateArgs,
) -> PrReviewValidateArgs {
    if validate_args.id.is_none() {
        validate_args.id = parent.id;
    }
    if validate_args.comment.is_none() && validate_args.comment_file.is_none() {
        validate_args.comment = parent.comment.clone();
        validate_args.comment_file = parent.comment_file.clone();
    }
    if validate_args.thread_file.is_none() {
        validate_args.thread_file = parent.thread_file.clone();
    }
    validate_args
}

/// Build the primary "post the review" backend call for the chosen mode: a
/// native GitHub review submission when `--submit-review` is set (the
/// `#pullrequestreview-` object), otherwise the outcome-comment post. Used by
/// both the dry-run plan and the live run so they never diverge. `glab_form` is
/// only consulted on the GitLab comment path (native submission is GitHub-only,
/// guarded earlier in `run_with`). Native submission binds the provider
/// mutation to the caller-reviewed `--expected-head`.
fn build_review_post_call(
    ctx: &ProviderContext,
    id: u64,
    args: &PrReviewArgs,
    body: &str,
    body_present: bool,
    glab_form: GlabNoteForm,
) -> BackendCall {
    if args.submit_review {
        let event = args.decision.to_github_event();
        // A body-less APPROVE omits the body field entirely (GitHub allows it).
        let body_opt = body_present.then_some(body);
        BackendCall::new(
            BackendProgram::Gh,
            github_review_submit_argv(
                ctx,
                id,
                event,
                args.expected_head
                    .as_deref()
                    .expect("validated native review head"),
                body_opt,
            ),
        )
    } else {
        build_pr_comment_call(ctx, id, body, glab_form)
    }
}

fn read_review_thread_specs(path: &str) -> Result<Vec<PreparedReviewThreadSpec>, ForgeError> {
    let raw = read_review_thread_file_bounded(path)?;
    parse_review_thread_specs(&raw)
}

fn read_review_thread_file_bounded(path: &str) -> Result<String, ForgeError> {
    let mut raw = String::new();
    let read_limit = REVIEW_THREAD_FILE_MAX_BYTES + 1;
    if path == "-" {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock().take(read_limit);
        reader.read_to_string(&mut raw).map_err(|e| {
            ForgeError::software(
                schema_err(),
                "failed to read --thread-file from stdin",
                Some(e.to_string()),
            )
        })?;
        return Ok(raw);
    }

    let file = fs::File::open(path).map_err(|e| {
        ForgeError::software(
            schema_err(),
            format!("failed to read --thread-file '{path}'"),
            Some(e.to_string()),
        )
    })?;
    let mut reader = file.take(read_limit);
    reader.read_to_string(&mut raw).map_err(|e| {
        ForgeError::software(
            schema_err(),
            format!("failed to read --thread-file '{path}'"),
            Some(e.to_string()),
        )
    })?;
    Ok(raw)
}

fn parse_review_thread_specs(raw: &str) -> Result<Vec<PreparedReviewThreadSpec>, ForgeError> {
    if raw.len() > REVIEW_THREAD_FILE_MAX_BYTES as usize {
        return Err(review_thread_spec_err(format!(
            "--thread-file must be at most {REVIEW_THREAD_FILE_MAX_BYTES} bytes"
        )));
    }
    let specs: Vec<ReviewThreadSpec> = serde_json::from_str(raw).map_err(|e| {
        ForgeError::validation(
            schema_err(),
            "invalid_review_thread_spec",
            "--thread-file must be a JSON array of review thread specs",
            Some(e.to_string()),
        )
    })?;
    if specs.is_empty() {
        return Err(review_thread_spec_err(
            "--thread-file must contain at least one review thread spec",
        ));
    }
    if specs.len() > REVIEW_THREAD_MAX_COUNT {
        return Err(review_thread_spec_err(format!(
            "--thread-file must contain at most {REVIEW_THREAD_MAX_COUNT} review thread specs"
        )));
    }
    specs
        .into_iter()
        .enumerate()
        .map(|(idx, spec)| prepare_review_thread_spec(idx + 1, spec))
        .collect()
}

fn prepare_review_thread_spec(
    index: usize,
    spec: ReviewThreadSpec,
) -> Result<PreparedReviewThreadSpec, ForgeError> {
    let path = spec.path.trim().to_string();
    if path.is_empty() {
        return Err(review_thread_spec_err(format!(
            "thread spec #{index} needs path and body"
        )));
    }
    if path.len() > REVIEW_THREAD_PATH_MAX_BYTES {
        return Err(review_thread_spec_err(format!(
            "thread spec #{index} path must be at most {REVIEW_THREAD_PATH_MAX_BYTES} bytes"
        )));
    }
    if spec.body.trim().is_empty() {
        return Err(review_thread_spec_err(format!(
            "thread spec #{index} needs path and body"
        )));
    }
    if spec.body.len() > REVIEW_THREAD_BODY_MAX_BYTES {
        return Err(review_thread_spec_err(format!(
            "thread spec #{index} body must be at most {REVIEW_THREAD_BODY_MAX_BYTES} bytes"
        )));
    }
    no_local_path(&path, "review thread path")?;
    no_local_path(&spec.body, "review thread body")?;
    no_escaped_control_markdown(&path)?;
    no_escaped_control_markdown(&spec.body)?;

    let side = spec.side.unwrap_or_default();
    let subject_type = spec.subject_type.unwrap_or(if spec.line.is_some() {
        ReviewThreadSubjectType::Line
    } else {
        ReviewThreadSubjectType::File
    });
    if matches!(subject_type, ReviewThreadSubjectType::Line) && spec.line.is_none() {
        return Err(review_thread_spec_err(format!(
            "thread spec #{index} needs line for a LINE review thread"
        )));
    }
    if matches!(subject_type, ReviewThreadSubjectType::Line) && spec.line == Some(0) {
        return Err(review_thread_spec_err(format!(
            "thread spec #{index} line must be greater than zero"
        )));
    }
    if matches!(subject_type, ReviewThreadSubjectType::File) && spec.line.is_some() {
        return Err(review_thread_spec_err(format!(
            "thread spec #{index} uses subjectType FILE and must omit line"
        )));
    }
    if spec.start_line == Some(0) {
        return Err(review_thread_spec_err(format!(
            "thread spec #{index} startLine must be greater than zero"
        )));
    }
    if spec.start_line.is_some() && spec.line.is_none() {
        return Err(review_thread_spec_err(format!(
            "thread spec #{index} uses startLine and must also include line"
        )));
    }
    if spec.start_side.is_some() && spec.start_line.is_none() {
        return Err(review_thread_spec_err(format!(
            "thread spec #{index} uses startSide and must also include startLine"
        )));
    }
    let start_side = spec.start_line.map(|_| spec.start_side.unwrap_or(side));

    Ok(PreparedReviewThreadSpec {
        path,
        body: spec.body,
        line: spec.line,
        side,
        start_line: spec.start_line,
        start_side,
        subject_type,
    })
}

fn review_thread_spec_err(message: impl Into<String>) -> ForgeError {
    ForgeError::validation(
        schema_err(),
        "invalid_review_thread_spec",
        message.into(),
        Some(
            "expected JSON array entries like {\"path\":\"src/lib.rs\",\"line\":42,\"side\":\"RIGHT\",\"body\":\"...\"}; omit line or set subjectType=FILE for file-level threads".to_string(),
        ),
    )
}

fn validate_review_threads_against_github_diff<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    number: u64,
    specs: &[PreparedReviewThreadSpec],
) -> Result<(), ForgeError> {
    let files_output = runner.run(&build_github_pr_files_call(ctx, number)?)?;
    let files = parse_github_pr_files(&files_output.stdout)?;
    let by_path = files
        .iter()
        .map(|file| (file.filename.as_str(), file))
        .collect::<HashMap<_, _>>();

    for (idx, spec) in specs.iter().enumerate() {
        let index = idx + 1;
        let Some(file) = by_path.get(spec.path.as_str()) else {
            return Err(review_thread_file_not_changed_err(index, spec));
        };
        if matches!(spec.subject_type, ReviewThreadSubjectType::File) {
            continue;
        }

        let patch = file.patch.as_deref().unwrap_or_default();
        let hunks = commentable_hunks_by_side(patch);
        if let Some(line) = spec.line
            && find_commentable_line(&hunks, spec.side, line).is_none()
        {
            return Err(review_thread_line_not_in_diff_err(
                index, spec, line, spec.side,
            ));
        }
        if let Some(start_line) = spec.start_line {
            let start_side = spec.start_side.unwrap_or(spec.side);
            if find_commentable_line(&hunks, start_side, start_line).is_none() {
                return Err(review_thread_line_not_in_diff_err(
                    index, spec, start_line, start_side,
                ));
            }
            let end_line = spec.line.expect("start_line requires line");
            if !range_is_commentable_in_single_hunk(
                &hunks, start_side, start_line, spec.side, end_line,
            ) {
                return Err(review_thread_range_not_in_diff_err(
                    index, spec, start_line, start_side, end_line, spec.side,
                ));
            }
        }
    }
    Ok(())
}

fn parse_github_pr_files(raw: &str) -> Result<Vec<GitHubPrFile>, ForgeError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if let Ok(files) = serde_json::from_str::<Vec<GitHubPrFile>>(trimmed) {
        return Ok(files);
    }

    let mut files = Vec::new();
    for (idx, line) in trimmed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        let line = line.trim();
        if line.starts_with('[') {
            let mut page = serde_json::from_str::<Vec<GitHubPrFile>>(line).map_err(|e| {
                ForgeError::software(
                    schema_err(),
                    "github pull request files response is invalid JSON",
                    Some(format!("line={}; error={e}", idx + 1)),
                )
            })?;
            files.append(&mut page);
        } else {
            let file = serde_json::from_str::<GitHubPrFile>(line).map_err(|e| {
                ForgeError::software(
                    schema_err(),
                    "github pull request files response is invalid JSON",
                    Some(format!("line={}; error={e}", idx + 1)),
                )
            })?;
            files.push(file);
        }
    }
    Ok(files)
}

#[derive(Debug, Default)]
struct CommentableHunk {
    left_positions: HashMap<u32, usize>,
    right_positions: HashMap<u32, usize>,
}

impl CommentableHunk {
    fn position(&self, side: ReviewThreadDiffSide, line: u32) -> Option<usize> {
        match side {
            ReviewThreadDiffSide::Left => self.left_positions.get(&line).copied(),
            ReviewThreadDiffSide::Right => self.right_positions.get(&line).copied(),
        }
    }
}

fn commentable_hunks_by_side(patch: &str) -> Vec<CommentableHunk> {
    let mut hunks = Vec::new();
    let mut current_hunk = None::<CommentableHunk>;
    let mut old_line = None::<u32>;
    let mut new_line = None::<u32>;
    let mut diff_position = 0usize;

    for line in patch.lines() {
        if line.starts_with("@@") {
            if let Some(hunk) = current_hunk.take() {
                hunks.push(hunk);
            }
            if let Some((old_start, new_start)) = parse_hunk_header(line) {
                current_hunk = Some(CommentableHunk::default());
                old_line = Some(old_start);
                new_line = Some(new_start);
                diff_position = 0;
            } else {
                old_line = None;
                new_line = None;
            }
            continue;
        }

        let (Some(old_current), Some(new_current)) = (old_line, new_line) else {
            continue;
        };
        let Some(hunk) = current_hunk.as_mut() else {
            continue;
        };
        match line.as_bytes().first().copied() {
            Some(b' ') => {
                // GitHub review threads use LEFT for deletions and RIGHT for
                // additions or unchanged context lines.
                hunk.right_positions.insert(new_current, diff_position);
                old_line = old_current.checked_add(1);
                new_line = new_current.checked_add(1);
                diff_position += 1;
            }
            Some(b'-') => {
                hunk.left_positions.insert(old_current, diff_position);
                old_line = old_current.checked_add(1);
                diff_position += 1;
            }
            Some(b'+') => {
                hunk.right_positions.insert(new_current, diff_position);
                new_line = new_current.checked_add(1);
                diff_position += 1;
            }
            Some(b'\\') => {}
            _ => {}
        }
    }

    if let Some(hunk) = current_hunk {
        hunks.push(hunk);
    }
    hunks
}

fn find_commentable_line(
    hunks: &[CommentableHunk],
    side: ReviewThreadDiffSide,
    line: u32,
) -> Option<(usize, usize)> {
    hunks
        .iter()
        .enumerate()
        .find_map(|(hunk_index, hunk)| hunk.position(side, line).map(|pos| (hunk_index, pos)))
}

fn range_is_commentable_in_single_hunk(
    hunks: &[CommentableHunk],
    start_side: ReviewThreadDiffSide,
    start_line: u32,
    end_side: ReviewThreadDiffSide,
    end_line: u32,
) -> bool {
    hunks.iter().any(|hunk| {
        let Some(start_pos) = hunk.position(start_side, start_line) else {
            return false;
        };
        let Some(end_pos) = hunk.position(end_side, end_line) else {
            return false;
        };
        start_pos <= end_pos
    })
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let mut parts = line.split_whitespace();
    (parts.next()? == "@@").then_some(())?;
    let old_span = parts.next()?.strip_prefix('-')?;
    let new_span = parts.next()?.strip_prefix('+')?;
    Some((parse_hunk_start(old_span)?, parse_hunk_start(new_span)?))
}

fn parse_hunk_start(span: &str) -> Option<u32> {
    span.split(',').next()?.parse().ok()
}

fn review_thread_file_not_changed_err(index: usize, spec: &PreparedReviewThreadSpec) -> ForgeError {
    ForgeError::validation(
        schema_err(),
        "review_thread_file_not_changed",
        format!(
            "thread spec #{index} path '{path}' is not present in the pull request diff",
            path = spec.path
        ),
        Some(format!(
            "thread_spec_index={index}; thread_spec_path={path}; suggestion=omit --thread-file for this finding or use a file-level thread on a changed file",
            path = spec.path,
        )),
    )
}

fn review_thread_line_not_in_diff_err(
    index: usize,
    spec: &PreparedReviewThreadSpec,
    line: u32,
    side: ReviewThreadDiffSide,
) -> ForgeError {
    ForgeError::validation(
        schema_err(),
        "review_thread_line_not_in_diff",
        format!(
            "thread spec #{index} line {line} on {side} side is not commentable in the pull request diff for '{path}'",
            side = side.as_str(),
            path = spec.path,
        ),
        Some(format!(
            "thread_spec_index={index}; thread_spec_path={path}; thread_spec_line={line}; thread_spec_side={side}; suggestion=rerun with a line from the changed diff hunk, omit line for a file-level thread, or keep the finding in the review summary body",
            path = spec.path,
            side = side.as_str(),
        )),
    )
}

fn review_thread_range_not_in_diff_err(
    index: usize,
    spec: &PreparedReviewThreadSpec,
    start_line: u32,
    start_side: ReviewThreadDiffSide,
    end_line: u32,
    end_side: ReviewThreadDiffSide,
) -> ForgeError {
    ForgeError::validation(
        schema_err(),
        "review_thread_range_not_in_diff",
        format!(
            "thread spec #{index} range {start_line} on {start_side} side to {end_line} on {end_side} side is not a valid single-hunk pull request diff range",
            start_side = start_side.as_str(),
            end_side = end_side.as_str(),
        ),
        Some(format!(
            "thread_spec_index={index}; thread_spec_path={path}; thread_spec_start_line={start_line}; thread_spec_start_side={start_side}; thread_spec_line={end_line}; thread_spec_side={end_side}; suggestion=keep ranged review threads inside one changed diff hunk with start before end, use a single-line thread, or keep the finding in the review summary body",
            path = spec.path,
            start_side = start_side.as_str(),
            end_side = end_side.as_str(),
        )),
    )
}

/// Result of the GitHub native-review-with-threads submission. A recovered
/// completed transaction retains its original `review_url` while `submitted`
/// remains false so callers do not duplicate downstream issue activity.
struct GithubReviewSubmission {
    review_url: Option<String>,
    submitted: bool,
    created: Vec<CreatedReviewThread>,
    skipped: usize,
}

struct GithubReviewThreadSubmissionRequest<'a> {
    number: u64,
    decision: PrReviewDecision,
    expected_head: &'a str,
    body: Option<&'a str>,
    specs: &'a [PreparedReviewThreadSpec],
    route_lenses: &'a [String],
}

fn submit_github_review_with_threads<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    request: GithubReviewThreadSubmissionRequest<'_>,
) -> Result<GithubReviewSubmission, ForgeError> {
    let GithubReviewThreadSubmissionRequest {
        number,
        decision,
        expected_head,
        body,
        specs,
        route_lenses,
    } = request;
    let (owner, name) = github_owner_name(ctx)?;
    let target_output = runner.run(&build_github_review_target_call(ctx, owner, name, number))?;
    let target = parse_github_review_target(&target_output)?;

    // Cross-run idempotency: skip any finding that already has a live
    // (non-resolved, non-outdated) thread on the current head, keyed on
    // (path, body). This never deletes or mutates a prior thread or review; it
    // only avoids creating a duplicate. An outdated match is not a live
    // duplicate (the anchor moved), so it is posted fresh; the stale copy is
    // dispositioned at the merge gate. Within-run order of surviving specs is
    // preserved.
    let existing =
        pr_review_threads::compute_fingerprints_for_pr(runner, ctx, &target.url, number)?;
    let live_fingerprints: HashSet<(String, String)> = existing
        .threads
        .iter()
        .filter(|thread| !thread.resolved && !thread.outdated)
        .map(|thread| {
            (
                thread.path.clone(),
                review_state::strip_owned_markers(&thread.body),
            )
        })
        .collect();
    let to_post: Vec<&PreparedReviewThreadSpec> = specs
        .iter()
        .filter(|spec| {
            !live_fingerprints.contains(&(
                spec.path.clone(),
                review_state::strip_owned_markers(&spec.body),
            ))
        })
        .collect();
    let skipped = specs.len() - to_post.len();

    let repository = format!("{owner}/{name}");
    let summary = review_state::strip_owned_markers(body.unwrap_or(""));

    // When every requested finding is already live, first check whether this
    // exact immutable command is a completed receipt-bound transaction. That
    // preserves the original review identity on an exact rerun; unrelated
    // duplicate suppression still returns the explicit skipped result.
    if to_post.is_empty() {
        let summary_digest = review_state::sha256_digest(summary.as_bytes());
        let manifest = specs
            .iter()
            .enumerate()
            .map(|(index, spec)| review_manifest_item(index, spec))
            .collect::<Vec<_>>();
        let review_run_id = review_state::compute_review_run_id(
            &repository,
            number,
            expected_head,
            0,
            route_lenses,
            decision.as_str(),
            &summary_digest,
            &manifest,
        )?;
        let receipt = review_state::ReviewRunReceipt {
            review_run_id: review_run_id.clone(),
            route_lenses: route_lenses.to_vec(),
            decision: decision.as_str().to_string(),
            expected_head: expected_head.to_string(),
            round: 0,
            summary_digest,
            inline_manifest: manifest.clone(),
        };
        let state = read_review_state_snapshot(runner, ctx, &repository, number)?;
        if receipt_is_recorded(&state.chain, &receipt)?
            && let Some(review_url) =
                find_submitted_review_run(runner, ctx, number, &target.url, &receipt)?
        {
            return Ok(GithubReviewSubmission {
                review_url: Some(review_url),
                submitted: false,
                created: Vec::new(),
                skipped,
            });
        }
        return Ok(GithubReviewSubmission {
            review_url: None,
            submitted: false,
            created: Vec::new(),
            skipped,
        });
    }

    let summary_digest = review_state::sha256_digest(summary.as_bytes());
    let manifest = to_post
        .iter()
        .copied()
        .enumerate()
        .map(|(index, spec)| review_manifest_item(index, spec))
        .collect::<Vec<_>>();
    let review_run_id = review_state::compute_review_run_id(
        &repository,
        number,
        expected_head,
        0,
        route_lenses,
        decision.as_str(),
        &summary_digest,
        &manifest,
    )?;
    let receipt = review_state::ReviewRunReceipt {
        review_run_id: review_run_id.clone(),
        route_lenses: route_lenses.to_vec(),
        decision: decision.as_str().to_string(),
        expected_head: expected_head.to_string(),
        round: 0,
        summary_digest,
        inline_manifest: manifest.clone(),
    };
    let state = read_review_state_snapshot(runner, ctx, &repository, number)?;
    if receipt_is_recorded(&state.chain, &receipt)?
        && let Some(review_url) =
            find_submitted_review_run(runner, ctx, number, &target.url, &receipt)?
    {
        return Ok(GithubReviewSubmission {
            review_url: Some(review_url),
            submitted: false,
            created: Vec::new(),
            skipped,
        });
    }

    let marked_body = marked_review_body(&summary, &review_run_id);
    let marked_specs = to_post
        .iter()
        .copied()
        .enumerate()
        .map(|(index, spec)| marked_review_thread_spec(spec, &review_run_id, &manifest[index]))
        .collect::<Vec<_>>();
    let guards = pr_reviews::compute_pending_guards_for_pr(runner, ctx, number, &target.url)?;
    if guards.head_sha != expected_head {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_head_changed",
            "the pull-request head changed before pending-review recovery",
            Some(format!(
                "expected_head={expected_head}; provider_head={}",
                guards.head_sha
            )),
        ));
    }
    let viewer_pending = guards
        .reviews
        .iter()
        .filter(|pending| pending.viewer_did_author)
        .collect::<Vec<_>>();
    if viewer_pending.len() > 1 {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_ambiguous",
            "multiple viewer-owned pending reviews prevent automatic recovery",
            Some(format!(
                "pr={number}; pending_review_count={}",
                viewer_pending.len()
            )),
        ));
    }

    let (pending, existing_count) = if let Some(existing) = viewer_pending.first() {
        let snapshot = pr_reviews::compute_pending_snapshot(runner, ctx, &existing.id)?
            .ok_or_else(|| {
                pending_transaction_incomplete(
                    &review_run_id,
                    Some(existing.id.as_str()),
                    "the pending review disappeared before recovery",
                    None,
                )
            })?;
        validate_receipt_bound_snapshot(
            &snapshot,
            number,
            expected_head,
            &summary,
            &review_run_id,
            &manifest,
        )?;
        ensure_review_run_receipt(runner, ctx, &repository, number, &state, &receipt)?;
        (
            GitHubPendingReview {
                review_id: snapshot.review_id,
                url: snapshot.review_url,
            },
            snapshot.inline_comments.len(),
        )
    } else {
        ensure_review_run_receipt(runner, ctx, &repository, number, &state, &receipt)?;
        let pending_output = runner
            .run(&build_github_pending_review_call(
                ctx,
                &target.pull_request_id,
                expected_head,
                Some(&marked_body),
            ))
            .map_err(|error| {
                pending_transaction_incomplete(
                    &review_run_id,
                    None,
                    "pending review creation did not return a confirmed result",
                    Some(error),
                )
            })?;
        let pending = parse_github_pending_review(&pending_output).map_err(|error| {
            pending_transaction_incomplete(
                &review_run_id,
                None,
                "pending review creation response was incomplete",
                Some(error),
            )
        })?;
        (pending, 0)
    };

    let mut review_threads = Vec::with_capacity(marked_specs.len() - existing_count);
    for (idx, spec) in marked_specs.iter().enumerate().skip(existing_count) {
        let output = match runner.run(&build_github_add_review_thread_call(
            ctx,
            &pending.review_id,
            spec,
        )) {
            Ok(output) => output,
            Err(err) => {
                let err = map_github_review_thread_error(idx + 1, spec, err);
                return Err(pending_transaction_incomplete(
                    &review_run_id,
                    Some(&pending.review_id),
                    "pending review retained after inline comment mutation failed",
                    Some(err),
                ));
            }
        };
        let thread = match parse_created_review_thread(&output, spec) {
            Ok(thread) => thread,
            Err(err) => {
                return Err(pending_transaction_incomplete(
                    &review_run_id,
                    Some(&pending.review_id),
                    "pending review retained because inline comment creation was not confirmed",
                    Some(err),
                ));
            }
        };
        review_threads.push(thread);
    }

    let final_snapshot = pr_reviews::compute_pending_snapshot(runner, ctx, &pending.review_id)?
        .ok_or_else(|| {
            pending_transaction_incomplete(
                &review_run_id,
                Some(&pending.review_id),
                "pending review disappeared before final submit",
                None,
            )
        })?;
    validate_receipt_bound_snapshot(
        &final_snapshot,
        number,
        expected_head,
        &summary,
        &review_run_id,
        &manifest,
    )?;
    if final_snapshot.inline_comments.len() != manifest.len() {
        return Err(pending_transaction_incomplete(
            &review_run_id,
            Some(&pending.review_id),
            "pending review manifest is not complete",
            Some(ForgeError::validation(
                schema_err(),
                "pending_review_manifest_mismatch",
                "the pending review is missing one or more receipt-bound inline comments",
                Some(format!(
                    "expected_comments={}; observed_comments={}",
                    manifest.len(),
                    final_snapshot.inline_comments.len()
                )),
            )),
        ));
    }

    let submit_output = match runner.run(&build_github_submit_review_call(
        ctx,
        &pending.review_id,
        decision.to_github_event(),
        Some(&marked_body),
    )) {
        Ok(output) => output,
        Err(err) => {
            let err = map_github_native_review_submit_error(decision, err);
            if let Some(review_url) =
                find_submitted_review_run(runner, ctx, number, &target.url, &receipt)?
            {
                return Ok(GithubReviewSubmission {
                    review_url: Some(review_url),
                    submitted: true,
                    created: review_threads,
                    skipped,
                });
            }
            return Err(pending_transaction_incomplete(
                &review_run_id,
                Some(&pending.review_id),
                "pending review submit result is unknown; content was preserved",
                Some(err),
            ));
        }
    };
    let review_url = parse_submitted_review_url(&submit_output).unwrap_or(pending.url);
    Ok(GithubReviewSubmission {
        review_url: Some(review_url),
        submitted: true,
        created: review_threads,
        skipped,
    })
}

fn review_manifest_item(
    index: usize,
    spec: &PreparedReviewThreadSpec,
) -> review_state::ReviewCommentManifestItem {
    review_state::ReviewCommentManifestItem {
        index,
        path: spec.path.clone(),
        line: spec.line,
        side: spec.side.as_str().to_string(),
        start_line: spec.start_line,
        start_side: spec.start_side.map(|side| side.as_str().to_string()),
        subject_type: spec.subject_type.as_str().to_string(),
        body_digest: review_state::sha256_digest(
            review_state::strip_owned_markers(&spec.body).as_bytes(),
        ),
    }
}

fn marked_review_body(body: &str, review_run_id: &str) -> String {
    let marker = review_state::review_run_marker(review_run_id);
    if body.is_empty() {
        marker
    } else {
        format!("{marker}\n{body}")
    }
}

fn marked_review_thread_spec(
    spec: &PreparedReviewThreadSpec,
    review_run_id: &str,
    manifest: &review_state::ReviewCommentManifestItem,
) -> PreparedReviewThreadSpec {
    let mut marked = spec.clone();
    marked.body = format!(
        "{}\n{}",
        review_state::finding_marker(review_run_id, &manifest.body_digest),
        review_state::strip_owned_markers(&spec.body)
    );
    marked
}

fn validate_receipt_bound_snapshot(
    snapshot: &pr_reviews::PendingReviewSnapshot,
    number: u64,
    expected_head: &str,
    expected_summary: &str,
    review_run_id: &str,
    manifest: &[review_state::ReviewCommentManifestItem],
) -> Result<(), ForgeError> {
    if snapshot.number != number
        || snapshot.head_sha != expected_head
        || snapshot.commit_sha.as_deref() != Some(expected_head)
    {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_head_changed",
            "the pending review no longer matches the receipt-bound pull-request head",
            Some(format!(
                "expected_pr={number}; provider_pr={}; expected_head={expected_head}; provider_head={}; provider_commit={}",
                snapshot.number,
                snapshot.head_sha,
                snapshot.commit_sha.as_deref().unwrap_or("<missing>")
            )),
        ));
    }
    if !snapshot.viewer_did_author {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_identity_mismatch",
            "the invoking GitHub identity is not the receipt-bound pending review author",
            Some(format!("review_id={}", snapshot.review_id)),
        ));
    }
    if snapshot.provenance != "receipt-bound"
        || snapshot.review_run_id.as_deref() != Some(review_run_id)
        || snapshot.semantic_body != expected_summary
    {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_manifest_mismatch",
            "the pending review body or provenance differs from the immutable receipt",
            Some(format!(
                "review_id={}; expected_run={review_run_id}; observed_run={}; provenance={}",
                snapshot.review_id,
                snapshot.review_run_id.as_deref().unwrap_or("<unmarked>"),
                snapshot.provenance
            )),
        ));
    }
    if snapshot.inline_comments.len() > manifest.len() {
        return Err(ForgeError::validation(
            schema_err(),
            "pending_review_manifest_mismatch",
            "the pending review contains more inline comments than the immutable receipt",
            Some(format!(
                "review_id={}; expected_comments={}; observed_comments={}",
                snapshot.review_id,
                manifest.len(),
                snapshot.inline_comments.len()
            )),
        ));
    }
    for (comment, expected) in snapshot.inline_comments.iter().zip(manifest) {
        if comment.review_run_id.as_deref() != Some(review_run_id)
            || comment.path != expected.path
            || comment.line != expected.line
            || comment.diff_side.as_deref() != Some(expected.side.as_str())
            || comment.start_line != expected.start_line
            || comment.start_diff_side != expected.start_side
            || comment.subject_type != expected.subject_type
            || comment.body_digest != expected.body_digest
        {
            return Err(ForgeError::validation(
                schema_err(),
                "pending_review_manifest_mismatch",
                "an existing inline comment differs from the immutable receipt",
                Some(format!(
                    "review_id={}; comment_id={}; manifest_index={}",
                    snapshot.review_id, comment.id, expected.index
                )),
            ));
        }
    }
    Ok(())
}

fn pending_transaction_incomplete(
    review_run_id: &str,
    pending_review_id: Option<&str>,
    message: &str,
    cause: Option<ForgeError>,
) -> ForgeError {
    let mut detail = vec![
        format!("review_run_id={review_run_id}"),
        format!(
            "pending_review_id={}",
            pending_review_id.unwrap_or("<unknown>")
        ),
        "recovery=rerun the identical pr review command or inspect the pending review".to_string(),
    ];
    if let Some(cause) = cause {
        detail.push(format!("cause_kind={}", cause.kind()));
        detail.push(format!("cause_message={}", cause.message()));
        if let Some(cause_detail) = cause.detail().filter(|value| !value.is_empty()) {
            detail.push(format!("cause_detail={cause_detail}"));
        }
    }
    ForgeError::validation(
        schema_err(),
        "pending_review_transaction_incomplete",
        message,
        Some(detail.join("; ")),
    )
}

fn ensure_review_run_receipt<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    repository: &str,
    number: u64,
    state: &ReviewStateSnapshot,
    receipt: &review_state::ReviewRunReceipt,
) -> Result<(), ForgeError> {
    if receipt_is_recorded(&state.chain, receipt)? {
        return Ok(());
    }
    let record = review_state::ReviewStateRecord::new(
        repository,
        number,
        &receipt.expected_head,
        state.chain.records.len() as u64,
        state.chain.tip_digest.clone(),
        review_state::ReviewStatePayload::ReviewRunReceipt {
            receipt: receipt.clone(),
        },
    )?;
    let marker = record.marker()?;
    let append_result = runner.run(&build_issue_comment_call(ctx, number, &marker));
    let observed = read_review_state_after(
        runner,
        ctx,
        repository,
        number,
        state.end_cursor.as_deref(),
        Some(&state.viewer_login),
        state.trusted_comments.clone(),
    )?;
    if receipt_is_recorded(&observed.chain, receipt)? {
        return Ok(());
    }
    Err(match append_result {
        Ok(_) => ForgeError::validation(
            schema_err(),
            "review_state_conflict",
            "the review-run receipt was not visible after provider write",
            Some(format!(
                "review_run_id={}; record_digest={}",
                receipt.review_run_id, record.record_digest
            )),
        ),
        Err(error) => error,
    })
}

fn receipt_is_recorded(
    chain: &review_state::ReviewStateChain,
    expected: &review_state::ReviewRunReceipt,
) -> Result<bool, ForgeError> {
    let mut found = false;
    for record in &chain.records {
        let review_state::ReviewStatePayload::ReviewRunReceipt { receipt } = &record.payload else {
            continue;
        };
        if receipt.review_run_id != expected.review_run_id {
            continue;
        }
        if receipt != expected {
            return Err(ForgeError::validation(
                schema_err(),
                "review_state_conflict",
                "a review-run id is bound to different immutable receipt content",
                Some(format!("review_run_id={}", expected.review_run_id)),
            ));
        }
        found = true;
    }
    Ok(found)
}

pub(crate) fn read_review_state_chain<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    repository: &str,
    number: u64,
) -> Result<review_state::ReviewStateChain, ForgeError> {
    Ok(read_review_state_snapshot(runner, ctx, repository, number)?.chain)
}

#[derive(Debug, Clone)]
pub(crate) struct ReviewLoopStateView {
    pub chain: review_state::ReviewStateChain,
    pub tip_created_at: Option<String>,
}

pub(crate) fn read_review_loop_state_view<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    repository: &str,
    number: u64,
) -> Result<ReviewLoopStateView, ForgeError> {
    let snapshot = read_review_state_snapshot(runner, ctx, repository, number)?;
    let tip_created_at = snapshot.chain.tip_digest.as_deref().and_then(|tip| {
        snapshot.trusted_comments.iter().find_map(|comment| {
            review_state::parse_state_marker(&comment.body)
                .ok()
                .flatten()
                .filter(|record| record.record_digest == tip)
                .map(|_| comment.created_at.clone())
        })
    });
    Ok(ReviewLoopStateView {
        chain: snapshot.chain,
        tip_created_at,
    })
}

pub(crate) fn append_review_loop_state<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    repository: &str,
    number: u64,
    expected_head: &str,
    expected_tip: Option<&str>,
    state: review_state::ReviewLoopState,
) -> Result<review_state::ReviewStateChain, ForgeError> {
    let before = read_review_state_snapshot(runner, ctx, repository, number)?;
    if before.chain.tip_digest.as_deref() != expected_tip {
        return Err(ForgeError::validation(
            schema_err(),
            "review_state_conflict",
            "the review-state tip changed before append",
            Some(format!(
                "expected_tip={}; provider_tip={}",
                expected_tip.unwrap_or("<genesis>"),
                before.chain.tip_digest.as_deref().unwrap_or("<genesis>")
            )),
        ));
    }
    let record = review_state::ReviewStateRecord::new(
        repository,
        number,
        expected_head,
        before.chain.records.len() as u64,
        before.chain.tip_digest.clone(),
        review_state::ReviewStatePayload::ReviewLoop { state },
    )?;
    let marker = record.marker()?;
    let append_result = runner.run(&build_issue_comment_call(ctx, number, &marker));
    let after = read_review_state_snapshot(runner, ctx, repository, number)?;
    if after
        .chain
        .records
        .iter()
        .any(|observed| observed.record_digest == record.record_digest)
    {
        return Ok(after.chain);
    }
    Err(match append_result {
        Ok(_) => ForgeError::validation(
            schema_err(),
            "review_state_conflict",
            "the review-loop state was not visible after provider write",
            Some(format!("record_digest={}", record.record_digest)),
        ),
        Err(error) => error,
    })
}

#[derive(Debug, Clone)]
struct ReviewStateSnapshot {
    chain: review_state::ReviewStateChain,
    trusted_comments: Vec<ReviewStateComment>,
    viewer_login: String,
    end_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReviewStateComment {
    body: String,
    created_at: String,
}

struct ReviewStatePage {
    trusted_comments: Vec<ReviewStateComment>,
    viewer_login: String,
    has_next_page: bool,
    end_cursor: Option<String>,
}

fn read_review_state_snapshot<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    repository: &str,
    number: u64,
) -> Result<ReviewStateSnapshot, ForgeError> {
    read_review_state_after(runner, ctx, repository, number, None, None, Vec::new())
}

fn read_review_state_after<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    repository: &str,
    number: u64,
    initial_cursor: Option<&str>,
    expected_viewer: Option<&str>,
    mut trusted_comments: Vec<ReviewStateComment>,
) -> Result<ReviewStateSnapshot, ForgeError> {
    let mut cursor = initial_cursor.map(str::to_string);
    let mut seen_cursors = BTreeSet::new();
    let mut viewer_login = expected_viewer.map(str::to_string);

    for page_index in 0..MAX_REVIEW_STATE_PAGES {
        let output = runner.run(&build_github_review_state_comments_page_call(
            ctx,
            repository,
            number,
            cursor.as_deref(),
        ))?;
        let page = parse_review_state_page(&output)?;
        if viewer_login
            .as_deref()
            .is_some_and(|viewer| viewer != page.viewer_login)
        {
            return Err(ForgeError::validation(
                schema_err(),
                "review_state_conflict",
                "the authenticated viewer changed while reading review transaction state",
                None,
            ));
        }
        viewer_login.get_or_insert(page.viewer_login);
        trusted_comments.extend(page.trusted_comments);
        if !page.has_next_page {
            let chain = review_state::parse_chain(
                trusted_comments.iter().map(|comment| comment.body.as_str()),
                repository,
                number,
            )?;
            return Ok(ReviewStateSnapshot {
                chain,
                trusted_comments,
                viewer_login: viewer_login.unwrap_or_default(),
                end_cursor: page.end_cursor,
            });
        }
        let next = page.end_cursor.ok_or_else(|| {
            ForgeError::validation(
                schema_err(),
                "review_state_snapshot_incomplete",
                "review-state comment pagination is missing endCursor",
                Some(format!("page={}", page_index + 1)),
            )
        })?;
        if !seen_cursors.insert(next.clone()) {
            return Err(ForgeError::validation(
                schema_err(),
                "review_state_snapshot_incomplete",
                "review-state comment pagination repeated a cursor",
                Some(format!("page={}; cursor={next}", page_index + 1)),
            ));
        }
        cursor = Some(next);
    }
    Err(ForgeError::validation(
        schema_err(),
        "review_state_snapshot_incomplete",
        "review-state comment pagination exceeded the safety page limit",
        Some(format!("max_pages={MAX_REVIEW_STATE_PAGES}")),
    ))
}

fn parse_review_state_page(
    output: &crate::backend::BackendSuccess,
) -> Result<ReviewStatePage, ForgeError> {
    if output.stdout.len() > MAX_REVIEW_STATE_PAGE_BYTES {
        return Err(ForgeError::validation(
            schema_err(),
            "review_state_snapshot_incomplete",
            "a review-state comment page exceeds the safety byte limit",
            Some(format!(
                "page_bytes={}; max_bytes={MAX_REVIEW_STATE_PAGE_BYTES}",
                output.stdout.len()
            )),
        ));
    }
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|error| {
        ForgeError::software(
            schema_err(),
            "review-state comment response is invalid JSON",
            Some(error.to_string()),
        )
    })?;
    if value
        .get("errors")
        .and_then(|errors| errors.as_array())
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(ForgeError::validation(
            schema_err(),
            "review_state_snapshot_incomplete",
            "GitHub returned partial review-state comment data",
            None,
        ));
    }
    let viewer_login = value
        .pointer("/data/viewer/login")
        .and_then(|item| item.as_str())
        .filter(|item| !item.is_empty())
        .ok_or_else(|| {
            ForgeError::validation(
                schema_err(),
                "review_state_snapshot_incomplete",
                "review-state comments are missing the authenticated viewer",
                None,
            )
        })?
        .to_string();
    let comments = value
        .pointer("/data/repository/pullRequest/comments")
        .ok_or_else(|| {
            ForgeError::validation(
                schema_err(),
                "review_state_snapshot_incomplete",
                "review-state response is missing pull-request comments",
                None,
            )
        })?;
    let nodes = comments
        .get("nodes")
        .and_then(|item| item.as_array())
        .ok_or_else(|| {
            ForgeError::validation(
                schema_err(),
                "review_state_snapshot_incomplete",
                "review-state response is missing comment nodes",
                None,
            )
        })?;
    let trusted_comments = nodes
        .iter()
        .filter(|node| {
            let author = node.pointer("/author/login").and_then(|item| item.as_str());
            let association = node.get("authorAssociation").and_then(|item| item.as_str());
            author.is_some_and(|author| github_comment_author_is_viewer(author, &viewer_login))
                || matches!(association, Some("OWNER" | "MEMBER" | "COLLABORATOR"))
        })
        .filter_map(|node| {
            Some(ReviewStateComment {
                body: node.get("body")?.as_str()?.to_string(),
                created_at: node
                    .get("createdAt")
                    .and_then(|item| item.as_str())
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect();
    let page_info = comments.get("pageInfo").ok_or_else(|| {
        ForgeError::validation(
            schema_err(),
            "review_state_snapshot_incomplete",
            "review-state response is missing pageInfo",
            None,
        )
    })?;
    let has_next_page = page_info
        .get("hasNextPage")
        .and_then(|item| item.as_bool())
        .ok_or_else(|| {
            ForgeError::validation(
                schema_err(),
                "review_state_snapshot_incomplete",
                "review-state pageInfo is missing hasNextPage",
                None,
            )
        })?;
    let end_cursor = page_info
        .get("endCursor")
        .and_then(|item| item.as_str())
        .filter(|item| !item.is_empty())
        .map(str::to_string);
    Ok(ReviewStatePage {
        trusted_comments,
        viewer_login,
        has_next_page,
        end_cursor,
    })
}

fn github_comment_author_is_viewer(author: &str, viewer: &str) -> bool {
    author == viewer
        || viewer
            .strip_suffix("[bot]")
            .is_some_and(|canonical| !canonical.is_empty() && author == canonical)
}

fn build_github_review_state_comments_call(
    ctx: &ProviderContext,
    repository: &str,
    number: u64,
) -> BackendCall {
    build_github_review_state_comments_page_call(ctx, repository, number, None)
}

fn build_github_review_state_comments_page_call(
    ctx: &ProviderContext,
    repository: &str,
    number: u64,
    after: Option<&str>,
) -> BackendCall {
    let (owner, name) = repository.split_once('/').unwrap_or((repository, ""));
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from("-f"),
        OsString::from(format!("query={GITHUB_REVIEW_STATE_COMMENTS_QUERY}")),
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

fn find_submitted_review_run<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    number: u64,
    pr_url: &str,
    receipt: &review_state::ReviewRunReceipt,
) -> Result<Option<String>, ForgeError> {
    let reviews = pr_reviews::compute_for_pr(runner, ctx, number, pr_url)?;
    let expected_state = match receipt.decision.as_str() {
        "comments-only" => "COMMENTED",
        "approve" => "APPROVED",
        "request-changes" => "CHANGES_REQUESTED",
        other => {
            return Err(ForgeError::validation(
                schema_err(),
                "review_state_conflict",
                "the immutable review receipt has an unknown decision",
                Some(format!("decision={other}")),
            ));
        }
    };
    let mut matches = reviews
        .current_head_reviews
        .iter()
        .chain(reviews.stale_reviews.iter())
        .filter(|review| {
            is_submitted_review_candidate(
                review,
                &reviews.viewer_login,
                &receipt.review_run_id,
                &receipt.expected_head,
                expected_state,
            )
        });
    let found = matches.next().map(|review| review.url.clone());
    if matches.next().is_some() {
        return Err(ForgeError::validation(
            schema_err(),
            "review_state_conflict",
            "multiple submitted reviews claim the same review-run id",
            Some(format!("review_run_id={}", receipt.review_run_id)),
        ));
    }
    if found.is_some() {
        validate_submitted_review_manifest(
            runner,
            ctx,
            pr_url,
            number,
            &reviews.viewer_login,
            &receipt.review_run_id,
            &receipt.inline_manifest,
        )?;
    }
    Ok(found)
}

fn is_submitted_review_candidate(
    review: &pr_reviews::NativeReviewSummary,
    viewer_login: &str,
    review_run_id: &str,
    expected_head: &str,
    expected_state: &str,
) -> bool {
    !viewer_login.is_empty()
        && review.author == viewer_login
        && review.commit_sha == expected_head
        && review.state == expected_state
        && !review.summary_truncated
        && review_state::parse_review_run_id(&review.summary).as_deref() == Some(review_run_id)
}

fn validate_submitted_review_manifest<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    pr_url: &str,
    number: u64,
    viewer_login: &str,
    review_run_id: &str,
    manifest: &[review_state::ReviewCommentManifestItem],
) -> Result<(), ForgeError> {
    let threads = pr_review_threads::compute_for_pr(runner, ctx, pr_url, number)?;
    for expected in manifest {
        let matched = threads.threads.iter().any(|thread| {
            let marker = review_state::parse_finding_marker(&thread.body);
            thread.author == viewer_login
                && marker.as_ref().is_some_and(|(run_id, digest)| {
                    run_id == review_run_id && digest == &expected.body_digest
                })
                && thread.path == expected.path
                && thread.line == expected.line
                && thread.diff_side.as_deref() == Some(expected.side.as_str())
                && thread.start_line == expected.start_line
                && thread.start_diff_side == expected.start_side
                && thread.subject_type.as_deref() == Some(expected.subject_type.as_str())
        });
        if !matched {
            return Err(ForgeError::validation(
                schema_err(),
                "review_state_conflict",
                "a submitted review marker does not have the complete receipt-bound inline manifest",
                Some(format!(
                    "review_run_id={review_run_id}; manifest_index={}",
                    expected.index
                )),
            ));
        }
    }
    Ok(())
}

fn map_github_review_thread_error(
    index: usize,
    spec: &PreparedReviewThreadSpec,
    err: ForgeError,
) -> ForgeError {
    let raw_detail = err.detail().unwrap_or_default();
    if !is_http_unprocessable_entity(raw_detail) {
        return err;
    }

    let detail = [
        "GitHub rejected the review thread mutation with HTTP 422.".to_string(),
        format!("thread_spec_index={index}"),
        format!("thread_spec_path={}", spec.path),
        spec.line
            .map(|line| format!("thread_spec_line={line}"))
            .unwrap_or_else(|| "thread_spec_line=".to_string()),
        format!("thread_spec_side={}", spec.side.as_str()),
        format!("thread_spec_subject_type={}", spec.subject_type.as_str()),
        "suggestion=run pr review validate <id> --check-diff before posting, use a changed diff line or valid same-hunk range, omit line for a file-level thread, or keep the finding in the review summary body".to_string(),
        format!("raw_backend_error_kind={}", err.kind()),
        format!("raw_backend_error_message={}", err.message()),
        format!("raw_backend_error_detail={raw_detail}"),
    ]
    .join("; ");

    ForgeError::runtime_failure(
        schema_err(),
        "github_review_thread_rejected",
        format!(
            "github rejected review thread creation for thread spec #{index}; check that the requested path and line are commentable in the pull request diff"
        ),
        Some(detail),
    )
}

fn map_github_native_review_submit_error(
    decision: PrReviewDecision,
    err: ForgeError,
) -> ForgeError {
    let detail = err.detail().unwrap_or_default();
    if !is_http_unprocessable_entity(detail) {
        return err;
    }

    let raw_detail = detail.trim();
    let mut detail_parts = vec![
        "GitHub rejected the native review submission with HTTP 422.".to_string(),
        "GitHub App bot identities can comment on pull requests but may not be eligible to approve them as reviewers.".to_string(),
        "Suggested next action: confirm the PR is ready for review, switch reviewer identity, or omit --submit-review to post an outcome comment with submitted_review=false.".to_string(),
        format!("raw_backend_error_kind={kind}", kind = err.kind()),
        format!("raw_backend_error_message={message}", message = err.message()),
    ];
    if !raw_detail.is_empty() {
        detail_parts.push(format!("raw_backend_error_detail={raw_detail}"));
    }

    ForgeError::runtime_failure(
        schema_err(),
        "github_native_review_rejected",
        format!(
            "github rejected native {decision} review submission; retry with an eligible reviewer identity or omit --submit-review for an outcome comment fallback",
            decision = decision.as_str(),
        ),
        Some(detail_parts.join("; ")),
    )
}

fn github_owner_name(ctx: &ProviderContext) -> Result<(&str, &str), ForgeError> {
    let Some(repo) = ctx.repo.as_deref() else {
        return Err(ForgeError::validation(
            schema_err(),
            "repo_required",
            "github native review submission requires --repo owner/name or a recognised GitHub remote",
            None,
        ));
    };
    repo.split_once('/').ok_or_else(|| {
        ForgeError::validation(
            schema_err(),
            "repo_required",
            "github native review submission requires a repo slug shaped as owner/name",
            Some(format!("repo={repo}")),
        )
    })
}

fn build_github_review_target_call(
    ctx: &ProviderContext,
    owner: &str,
    name: &str,
    number: u64,
) -> BackendCall {
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from("-f"),
        OsString::from(format!("query={GITHUB_REVIEW_TARGET_QUERY}")),
        OsString::from("-f"),
        OsString::from(format!("owner={owner}")),
        OsString::from("-f"),
        OsString::from(format!("name={name}")),
        OsString::from("-F"),
        OsString::from(format!("number={number}")),
    ]);
    BackendCall::new(BackendProgram::Gh, argv)
}

fn build_github_pr_files_call(
    ctx: &ProviderContext,
    number: u64,
) -> Result<BackendCall, ForgeError> {
    let (owner, name) = github_owner_name(ctx)?;
    let endpoint = format!("repos/{owner}/{name}/pulls/{number}/files");
    let mut argv = vec![OsString::from("api"), OsString::from(endpoint)];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from("--paginate"),
        OsString::from("--jq"),
        OsString::from(".[] | {filename, patch}"),
    ]);
    Ok(BackendCall::new(BackendProgram::Gh, argv))
}

fn build_github_pending_review_call(
    ctx: &ProviderContext,
    pull_request_id: &str,
    expected_head: &str,
    body: Option<&str>,
) -> BackendCall {
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from("-f"),
        OsString::from(format!("query={GITHUB_ADD_PENDING_REVIEW_MUTATION}")),
        OsString::from("-f"),
        OsString::from(format!("pullRequestId={pull_request_id}")),
        OsString::from("-f"),
        OsString::from(format!("commitOID={expected_head}")),
    ]);
    if let Some(body) = body {
        argv.extend([OsString::from("-f"), OsString::from(format!("body={body}"))]);
    }
    BackendCall::new(BackendProgram::Gh, argv)
}

fn build_github_add_review_thread_call(
    ctx: &ProviderContext,
    review_id: &str,
    spec: &PreparedReviewThreadSpec,
) -> BackendCall {
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from("-f"),
        OsString::from(format!("query={GITHUB_ADD_REVIEW_THREAD_MUTATION}")),
        OsString::from("-f"),
        OsString::from(format!("reviewId={review_id}")),
        OsString::from("-f"),
        OsString::from(format!("path={path}", path = spec.path)),
        OsString::from("-f"),
        OsString::from(format!("body={body}", body = spec.body)),
        OsString::from("-f"),
        OsString::from(format!("side={side}", side = spec.side.as_str())),
        OsString::from("-f"),
        OsString::from(format!(
            "subjectType={subject_type}",
            subject_type = spec.subject_type.as_str()
        )),
    ]);
    if let Some(line) = spec.line {
        argv.extend([OsString::from("-F"), OsString::from(format!("line={line}"))]);
    }
    if let Some(start_line) = spec.start_line {
        argv.extend([
            OsString::from("-F"),
            OsString::from(format!("startLine={start_line}")),
        ]);
    }
    if let Some(start_side) = spec.start_side {
        argv.extend([
            OsString::from("-f"),
            OsString::from(format!("startSide={}", start_side.as_str())),
        ]);
    }
    BackendCall::new(BackendProgram::Gh, argv)
}

pub(crate) fn build_github_submit_review_call(
    ctx: &ProviderContext,
    review_id: &str,
    event: &str,
    body: Option<&str>,
) -> BackendCall {
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from("-f"),
        OsString::from(format!("query={GITHUB_SUBMIT_REVIEW_MUTATION}")),
        OsString::from("-f"),
        OsString::from(format!("reviewId={review_id}")),
        OsString::from("-f"),
        OsString::from(format!("event={event}")),
    ]);
    if let Some(body) = body {
        argv.extend([OsString::from("-f"), OsString::from(format!("body={body}"))]);
    }
    BackendCall::new(BackendProgram::Gh, argv)
}

pub(crate) fn build_github_delete_pending_review_call(
    ctx: &ProviderContext,
    review_id: &str,
) -> BackendCall {
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from("-f"),
        OsString::from(format!("query={GITHUB_DELETE_PENDING_REVIEW_MUTATION}")),
        OsString::from("-f"),
        OsString::from(format!("reviewId={review_id}")),
    ]);
    BackendCall::new(BackendProgram::Gh, argv)
}

fn parse_github_review_target(
    output: &crate::backend::BackendSuccess,
) -> Result<GitHubReviewTarget, ForgeError> {
    let value = parse_graphql_json(output, "pull request lookup response")?;
    Ok(GitHubReviewTarget {
        pull_request_id: required_pointer_str(&value, "/data/repository/pullRequest/id")?,
        url: required_pointer_str(&value, "/data/repository/pullRequest/url")?,
    })
}

fn parse_github_pending_review(
    output: &crate::backend::BackendSuccess,
) -> Result<GitHubPendingReview, ForgeError> {
    let value = parse_graphql_json(output, "pending review response")?;
    Ok(GitHubPendingReview {
        review_id: required_pointer_str(&value, "/data/addPullRequestReview/pullRequestReview/id")?,
        url: required_pointer_str(&value, "/data/addPullRequestReview/pullRequestReview/url")?,
    })
}

fn parse_created_review_thread(
    output: &crate::backend::BackendSuccess,
    spec: &PreparedReviewThreadSpec,
) -> Result<CreatedReviewThread, ForgeError> {
    let value = parse_graphql_json(output, "review thread response")?;
    let line =
        optional_pointer_u32(&value, "/data/addPullRequestReviewThread/thread/line").or(spec.line);
    Ok(CreatedReviewThread {
        id: required_pointer_str(&value, "/data/addPullRequestReviewThread/thread/id")?,
        url: optional_pointer_str(
            &value,
            "/data/addPullRequestReviewThread/thread/comments/nodes/0/url",
        )
        .unwrap_or_default(),
        path: optional_pointer_str(&value, "/data/addPullRequestReviewThread/thread/path")
            .unwrap_or_else(|| spec.path.clone()),
        line,
        subject_type: optional_pointer_str(
            &value,
            "/data/addPullRequestReviewThread/thread/subjectType",
        )
        .unwrap_or_else(|| spec.subject_type.as_str().to_string()),
    })
}

pub(crate) fn parse_submitted_review_url(
    output: &crate::backend::BackendSuccess,
) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).ok()?;
    optional_pointer_str(
        &value,
        "/data/submitPullRequestReview/pullRequestReview/url",
    )
}

fn parse_graphql_json(
    output: &crate::backend::BackendSuccess,
    label: &str,
) -> Result<serde_json::Value, ForgeError> {
    serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            format!("github {label} is invalid JSON"),
            Some(e.to_string()),
        )
    })
}

fn required_pointer_str(value: &serde_json::Value, pointer: &str) -> Result<String, ForgeError> {
    optional_pointer_str(value, pointer).ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "github review-thread response is missing an expected field",
            Some(format!("missing={pointer}; response={value:?}")),
        )
    })
}

fn optional_pointer_str(value: &serde_json::Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn optional_pointer_u32(value: &serde_json::Value, pointer: &str) -> Option<u32> {
    value
        .pointer(pointer)
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
}

fn build_pr_comment_call(
    ctx: &ProviderContext,
    id: u64,
    body: &str,
    glab_form: GlabNoteForm,
) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub => github_issue_comment_argv(ctx, id, body),
        Provider::GitLab => gitlab_review_note_argv(id, body, glab_form),
        Provider::Local => vec![
            OsString::from("issue"),
            OsString::from("comment"),
            OsString::from(id.to_string()),
            OsString::from("--body"),
            OsString::from(body),
        ],
    };
    if ctx.provider == Provider::GitLab {
        ctx.push_repo_override(&mut argv);
    }
    BackendCall::new(program, argv)
}

/// GitLab review-outcome note argv, selected by the probed [`GlabNoteForm`]:
///
/// - `CreateResolvable` → `mr note create … --resolvable=false` (modern glab):
///   a non-resolvable status note that does not register as an unresolved MR
///   discussion blocking `forge-cli pr merge`'s GitLab gate.
/// - `Create` → `mr note create … --message` (`create` exists, no
///   `--resolvable`): the note stays resolvable, but `create` reliably accepts
///   `--message`.
/// - `BareNote` → `mr note <id> --message` (no `create` subcommand): the only
///   form such builds support.
fn gitlab_review_note_argv(id: u64, body: &str, form: GlabNoteForm) -> Vec<OsString> {
    let mut argv = vec![OsString::from("mr"), OsString::from("note")];
    if matches!(form, GlabNoteForm::Create | GlabNoteForm::CreateResolvable) {
        argv.push(OsString::from("create"));
    }
    argv.push(OsString::from(id.to_string()));
    argv.push(OsString::from("--message"));
    argv.push(OsString::from(body));
    if matches!(form, GlabNoteForm::CreateResolvable) {
        argv.push(OsString::from("--resolvable=false"));
    }
    argv
}

fn build_issue_comment_call(ctx: &ProviderContext, id: u64, body: &str) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub => github_issue_comment_argv(ctx, id, body),
        Provider::GitLab => vec![
            OsString::from("issue"),
            OsString::from("note"),
            OsString::from(id.to_string()),
            OsString::from("--message"),
            OsString::from(body),
        ],
        Provider::Local => vec![
            OsString::from("issue"),
            OsString::from("comment"),
            OsString::from(id.to_string()),
            OsString::from("--body"),
            OsString::from(body),
        ],
    };
    if ctx.provider == Provider::GitLab {
        ctx.push_repo_override(&mut argv);
    }
    BackendCall::new(program, argv)
}

fn github_issue_comment_argv(ctx: &ProviderContext, id: u64, body: &str) -> Vec<OsString> {
    let endpoint = ctx
        .repo
        .as_deref()
        .map(|repo| format!("repos/{repo}/issues/{id}/comments"))
        .unwrap_or_else(|| format!("repos/{{owner}}/{{repo}}/issues/{id}/comments"));
    let mut argv = vec![OsString::from("api")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from(endpoint),
        OsString::from("--method"),
        OsString::from("POST"),
        OsString::from("--raw-field"),
        OsString::from(format!("body={body}")),
        OsString::from("--jq"),
        OsString::from(".html_url"),
    ]);
    argv
}

/// `gh api repos/{repo}/pulls/{id}/reviews --method POST [--raw-field body=…]
/// --raw-field event=<EVENT> --jq .html_url` — submit a native GitHub pull
/// request review. The returned `html_url` is the `#pullrequestreview-<id>`
/// object. `body` is `None` for a body-less APPROVE, which omits the field
/// (GitHub rejects an empty body only for COMMENT / REQUEST_CHANGES, already
/// guarded in `run_with`). The review is attributed to whatever identity the
/// inherited `gh` token carries, so a reviewer-bot token yields a bot-authored
/// review. Mirrors [`github_issue_comment_argv`]'s endpoint shape (repo in the
/// path, hostname pushed for GitHub Enterprise).
fn github_review_submit_argv(
    ctx: &ProviderContext,
    id: u64,
    event: &str,
    expected_head: &str,
    body: Option<&str>,
) -> Vec<OsString> {
    let endpoint = ctx
        .repo
        .as_deref()
        .map(|repo| format!("repos/{repo}/pulls/{id}/reviews"))
        .unwrap_or_else(|| format!("repos/{{owner}}/{{repo}}/pulls/{id}/reviews"));
    let mut argv = vec![OsString::from("api")];
    ctx.push_github_api_hostname(&mut argv);
    argv.push(OsString::from(endpoint));
    argv.extend([OsString::from("--method"), OsString::from("POST")]);
    argv.extend([
        OsString::from("--raw-field"),
        OsString::from(format!("commit_id={expected_head}")),
    ]);
    if let Some(body) = body {
        argv.extend([
            OsString::from("--raw-field"),
            OsString::from(format!("body={body}")),
        ]);
    }
    argv.extend([
        OsString::from("--raw-field"),
        OsString::from(format!("event={event}")),
        OsString::from("--jq"),
        OsString::from(".html_url"),
    ]);
    argv
}

fn build_issue_mirror_body(
    provider: Provider,
    pr_number: u64,
    decision: PrReviewDecision,
    lenses: &[String],
    pr_comment_url: &str,
) -> String {
    let lenses = if lenses.is_empty() {
        "unspecified".to_string()
    } else {
        lenses.join(", ")
    };
    // GitLab Markdown references merge requests as `!<iid>` and issues as
    // `#<iid>`; GitHub uses `#<number>` for pull requests. Pick the
    // provider-correct sigil so the mirror links to the merge request, not an
    // unrelated issue with the same number.
    let pr_ref = match provider {
        Provider::GitLab => format!("!{pr_number}"),
        _ => format!("#{pr_number}"),
    };
    format!(
        "PR review outcome posted.\n\n- pr: {pr_ref}\n- decision: {decision}\n- lenses: {lenses}\n- review_url={pr_comment_url}\n",
        decision = decision.as_str(),
    )
}

/// Validation error raised when `--mirror-issue` is requested without the
/// `--issue <ISSUE_NUMBER>` it mirrors into. Raised up front, before any
/// backend mutation, so the failure can never leave a posted PR comment with no
/// mirror.
fn issue_required_err() -> ForgeError {
    ForgeError::validation(
        schema_err(),
        "issue_required",
        "--mirror-issue requires --issue <ISSUE_NUMBER>",
        None,
    )
}

fn review_id_required_err() -> ForgeError {
    ForgeError::validation(
        schema_err(),
        "id_required",
        "pr review requires a pull request id; use `pr review validate` for validation-only preflight",
        None,
    )
}

fn review_validate_id_required_err() -> ForgeError {
    ForgeError::validation(
        schema_err(),
        "review_validate_id_required",
        "pr review validate --check-diff requires a pull request id",
        None,
    )
}

/// Verify `<id>` resolves to a pull request on GitHub before posting a review
/// outcome through the issue-comments API. Uses `run_raw` so a `404 Not Found`
/// (the id is an issue, or does not exist) becomes a `DATA 65`
/// `id_not_pull_request` validation error. Auth and launch failures still
/// propagate as their own error kinds via `run_raw`; any other non-zero result
/// — rate limiting, a 5xx, a forbidden/SSO response, or a network error — could
/// have hit a perfectly valid PR, so it surfaces as a retryable `backend_error`
/// rather than a permanent `id_not_pull_request`.
fn ensure_github_pull_request<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    id: u64,
) -> Result<(), ForgeError> {
    let call = BackendCall::new(BackendProgram::Gh, github_pull_lookup_argv(ctx, id));
    let probe = runner.run_raw(&call)?;
    if probe.status_success {
        return Ok(());
    }
    let detail = probe.stderr.trim();
    if is_http_not_found(detail) {
        // GitHub returns 404 for an inaccessible *private* PR (an
        // under-scoped / un-SSO'd token) by design, so a 404 cannot be told
        // apart from a genuine non-PR / missing id at this endpoint. Map it to
        // the conservative `id_not_pull_request` (not a retryable backend error
        // — a token-scope problem will not pass on retry) and name all three
        // possibilities so the operator can tell which applies; the raw `gh`
        // stderr is carried in the detail.
        return Err(ForgeError::validation(
            schema_err(),
            "id_not_pull_request",
            format!(
                "#{id} could not be resolved as a pull request on github (HTTP 404); it is an issue, does not exist, or is a private PR the current token cannot access — refusing to post a review outcome"
            ),
            (!detail.is_empty()).then(|| detail.to_string()),
        ));
    }
    Err(ForgeError::backend_error(
        schema_err(),
        format!("failed to verify github pull request #{id} before posting the review outcome"),
        (!detail.is_empty()).then(|| detail.to_string()),
    ))
}

/// Fail before any native-review mutation when the authenticated GitHub viewer
/// already owns a pending review. GitHub otherwise reports an ambiguous HTTP
/// 422 whose prose is not stable enough for machine-readable recovery.
fn ensure_no_viewer_pending_github_review<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    id: u64,
    expected_head: &str,
) -> Result<(), ForgeError> {
    let repo = ctx.repo.as_deref().ok_or_else(|| {
        ForgeError::validation(
            schema_err(),
            "repo_required",
            "github native review preflight requires --repo owner/name or a recognised GitHub remote",
            None,
        )
    })?;
    let pr_url = format!("https://{host}/{repo}/pull/{id}", host = ctx.host);
    let snapshot = pr_reviews::compute_pending_guards_for_pr(runner, ctx, id, &pr_url)?;
    if snapshot.head_sha != expected_head {
        return Err(ForgeError::validation(
            schema_err(),
            "github_review_head_changed",
            "the provider PR head differs from the head supplied for native review submission",
            Some(format!(
                "expected_head={expected_head}; provider_head={provider_head}",
                provider_head = snapshot.head_sha,
            )),
        ));
    }
    let pending_review_count = snapshot
        .reviews
        .iter()
        .filter(|review| review.viewer_did_author)
        .count();
    if pending_review_count == 0 {
        return Ok(());
    }
    let deletable_pending_review_count = snapshot
        .reviews
        .iter()
        .filter(|review| review.viewer_did_author && review.viewer_can_delete)
        .count();

    Err(ForgeError::runtime_failure(
        schema_err(),
        "github_pending_review_exists",
        "github native review submission is blocked because the authenticated viewer already owns a pending review",
        Some(format!(
            "provider=github; pr={id}; head_sha={head}; pending_review_count={pending_review_count}; deletable_pending_review_count={deletable_pending_review_count}; suggestion=inspect pr reviews and delete only the exact viewer-owned pending review bound to the expected PR head before retrying",
            head = snapshot.head_sha,
        )),
    ))
}

/// True when `gh api` stderr indicates an HTTP 404 / Not Found — the only
/// failure class that proves `<id>` is not a pull request.
fn is_http_not_found(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("404") || lower.contains("not found")
}

fn is_http_unprocessable_entity(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("http 422") || lower.contains("unprocessable entity")
}

/// Resolve which GitLab review-note form this `glab` build supports with a
/// single `glab mr note create --help` probe, distinguishing all three version
/// classes:
///
/// - output advertises `--resolvable` → [`GlabNoteForm::CreateResolvable`].
/// - output names the `mr note create` subcommand (its own usage line) but has
///   no `--resolvable` → [`GlabNoteForm::Create`].
/// - neither (an absent `create` subcommand makes `glab` print the parent
///   `mr note` help, which names no `mr note create`) → [`GlabNoteForm::BareNote`].
///
/// A failed probe (e.g. `glab` missing) is treated as `BareNote`, the most
/// broadly-supported form. An absent subcommand still exits 0, so the decision
/// is made from the help text, not the exit status.
fn glab_note_form<R: BackendRunner>(runner: &R) -> GlabNoteForm {
    let call = BackendCall::new(
        BackendProgram::Glab,
        [
            OsString::from("mr"),
            OsString::from("note"),
            OsString::from("create"),
            OsString::from("--help"),
        ],
    );
    let text = match runner.run_raw(&call) {
        Ok(out) => format!("{}{}", out.stdout, out.stderr),
        Err(_) => return GlabNoteForm::BareNote,
    };
    if text.contains("--resolvable") {
        GlabNoteForm::CreateResolvable
    } else if text.contains("mr note create") {
        GlabNoteForm::Create
    } else {
        GlabNoteForm::BareNote
    }
}

/// `gh api repos/{repo}/pulls/{id} --jq .number` — the read used to confirm
/// `<id>` is a pull request. Mirrors [`github_issue_comment_argv`]'s endpoint
/// shape (repo embedded in the path, hostname pushed for GitHub Enterprise).
fn github_pull_lookup_argv(ctx: &ProviderContext, id: u64) -> Vec<OsString> {
    let endpoint = ctx
        .repo
        .as_deref()
        .map(|repo| format!("repos/{repo}/pulls/{id}"))
        .unwrap_or_else(|| format!("repos/{{owner}}/{{repo}}/pulls/{id}"));
    let mut argv = vec![OsString::from("api")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from(endpoint),
        OsString::from("--jq"),
        OsString::from(".number"),
    ]);
    argv
}

fn first_url(stdout: &str) -> Option<String> {
    stdout.split_whitespace().find_map(|token| {
        let url = token.trim_matches(|ch: char| {
            matches!(
                ch,
                '"' | '\'' | '`' | '<' | '>' | '(' | ')' | '[' | ']' | ','
            )
        });
        (url.starts_with("http://") || url.starts_with("https://") || url.starts_with("local://"))
            .then(|| url.to_string())
    })
}

fn schema_version() -> String {
    schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION)
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrReviewPayload) {
    let action = if payload.submitted_review {
        format!("submitted {decision} review", decision = payload.decision)
    } else {
        format!(
            "posted {decision} review outcome",
            decision = payload.decision
        )
    };
    if let Some(issue_url) = payload.issue_comment_url.as_deref() {
        println!(
            "{action} on {provider} #{number}: {pr_url}; mirrored issue activity: {issue_url}",
            provider = payload.provider,
            number = payload.number,
            pr_url = payload.pr_comment_url,
        );
    } else {
        println!(
            "{action} on {provider} #{number}: {pr_url}",
            provider = payload.provider,
            number = payload.number,
            pr_url = payload.pr_comment_url,
        );
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::provider::DetectionSource;

    struct ReceiptRunner {
        outputs: RefCell<Vec<crate::backend::BackendSuccess>>,
        calls: RefCell<Vec<String>>,
    }

    impl BackendRunner for ReceiptRunner {
        fn run(&self, call: &BackendCall) -> Result<crate::backend::BackendSuccess, ForgeError> {
            self.calls.borrow_mut().push(call.plan_argv().join(" "));
            Ok(self.outputs.borrow_mut().remove(0))
        }
    }

    fn ctx(repo: Option<&str>) -> ProviderContext {
        ProviderContext {
            provider: Provider::GitHub,
            host: "github.com".into(),
            source: DetectionSource::Flag,
            repo: repo.map(str::to_string),
        }
    }

    fn args(
        decision: PrReviewDecision,
        submit_review: bool,
        comment: Option<&str>,
    ) -> PrReviewArgs {
        PrReviewArgs {
            command: None,
            id: Some(44),
            decision,
            comment: comment.map(str::to_string),
            comment_file: None,
            lenses: Vec::new(),
            issue: None,
            mirror_issue: false,
            submit_review,
            expected_head: submit_review.then(|| "head-44".to_string()),
            thread_file: None,
        }
    }

    fn joined(call: &BackendCall) -> String {
        call.plan_argv().join(" ")
    }

    fn argv_joined(argv: &[OsString]) -> String {
        argv.iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn decision_maps_to_github_review_event() {
        assert_eq!(PrReviewDecision::CommentsOnly.to_github_event(), "COMMENT");
        assert_eq!(PrReviewDecision::Approve.to_github_event(), "APPROVE");
        assert_eq!(
            PrReviewDecision::RequestChanges.to_github_event(),
            "REQUEST_CHANGES"
        );
    }

    #[test]
    fn submit_argv_targets_reviews_endpoint_and_maps_event() {
        let argv = github_review_submit_argv(
            &ctx(Some("acme/widgets")),
            44,
            "REQUEST_CHANGES",
            "head-44",
            Some("nope"),
        );
        let joined = argv_joined(&argv);
        assert!(
            joined.contains("repos/acme/widgets/pulls/44/reviews"),
            "{joined}"
        );
        assert!(joined.contains("--method POST"), "{joined}");
        assert!(joined.contains("event=REQUEST_CHANGES"), "{joined}");
        assert!(joined.contains("commit_id=head-44"), "{joined}");
        assert!(joined.contains("body=nope"), "{joined}");
        assert!(joined.contains("--jq .html_url"), "{joined}");
    }

    #[test]
    fn submit_argv_omits_body_field_when_none() {
        let argv =
            github_review_submit_argv(&ctx(Some("acme/widgets")), 44, "APPROVE", "head-44", None);
        let joined = argv_joined(&argv);
        assert!(joined.contains("event=APPROVE"), "{joined}");
        assert!(
            !joined.contains("body="),
            "a body-less approve must omit the body field: {joined}"
        );
    }

    #[test]
    fn submit_argv_adds_hostname_for_enterprise_host() {
        let mut ctx = ctx(Some("acme/widgets"));
        ctx.host = "internal.ghe.com".into();
        let argv = github_review_submit_argv(&ctx, 44, "COMMENT", "head-44", Some("note"));
        let strs: Vec<String> = argv
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let pos = strs
            .iter()
            .position(|s| s == "--hostname")
            .expect("enterprise host must be passed to gh api");
        assert_eq!(strs[pos + 1], "internal.ghe.com");
    }

    #[test]
    fn build_review_post_call_uses_reviews_endpoint_when_submit_review() {
        let call = build_review_post_call(
            &ctx(Some("acme/widgets")),
            44,
            &args(PrReviewDecision::Approve, true, Some("lgtm")),
            "lgtm",
            true,
            GlabNoteForm::CreateResolvable,
        );
        assert_eq!(call.program, BackendProgram::Gh);
        let joined = joined(&call);
        assert!(joined.contains("pulls/44/reviews"), "{joined}");
        assert!(joined.contains("event=APPROVE"), "{joined}");
    }

    #[test]
    fn build_review_post_call_uses_issue_comment_when_not_submit_review() {
        let call = build_review_post_call(
            &ctx(Some("acme/widgets")),
            44,
            &args(PrReviewDecision::Approve, false, Some("lgtm")),
            "lgtm",
            true,
            GlabNoteForm::CreateResolvable,
        );
        let joined = joined(&call);
        assert!(joined.contains("issues/44/comments"), "{joined}");
        assert!(
            !joined.contains("/reviews"),
            "comment mode must not hit the reviews endpoint: {joined}"
        );
    }

    #[test]
    fn parse_review_thread_specs_defaults_line_thread() {
        let specs = parse_review_thread_specs(
            r#"[{"path":"src/lib.rs","line":42,"body":"Add regression coverage."}]"#,
        )
        .expect("valid line thread spec");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].path, "src/lib.rs");
        assert_eq!(specs[0].line, Some(42));
        assert_eq!(specs[0].side, ReviewThreadDiffSide::Right);
        assert_eq!(specs[0].subject_type, ReviewThreadSubjectType::Line);
    }

    #[test]
    fn parse_review_thread_specs_defaults_file_thread_without_line() {
        let specs = parse_review_thread_specs(
            r#"[{"path":"src/lib.rs","body":"File-level actionable finding."}]"#,
        )
        .expect("valid file thread spec");
        assert_eq!(specs[0].line, None);
        assert_eq!(specs[0].subject_type, ReviewThreadSubjectType::File);
    }

    #[test]
    fn parse_review_thread_specs_rejects_line_zero() {
        let err =
            parse_review_thread_specs(r#"[{"path":"src/lib.rs","line":0,"body":"Bad line."}]"#)
                .expect_err("line zero rejected");
        assert_eq!(err.kind(), "invalid_review_thread_spec");
    }

    #[test]
    fn add_review_thread_call_renders_file_subject_type() {
        let spec = parse_review_thread_specs(
            r#"[{"path":"src/lib.rs","body":"File-level actionable finding."}]"#,
        )
        .expect("valid file thread spec")
        .remove(0);
        let call = build_github_add_review_thread_call(&ctx(Some("acme/widgets")), "PRR_1", &spec);
        let joined = joined(&call);
        assert!(joined.contains("subjectType=FILE"), "{joined}");
        assert!(
            !joined.contains("line="),
            "file-level thread must not send a line variable: {joined}"
        );
    }

    #[test]
    fn submitted_review_recovery_rejects_a_foreign_marker() {
        let marker = review_state::review_run_marker("sha256:run");
        let mut review = pr_reviews::NativeReviewSummary {
            id: "PRR_foreign".to_string(),
            database_id: Some(7),
            url: "https://github.com/acme/widgets/pull/44#pullrequestreview-7".to_string(),
            author: "attacker".to_string(),
            state: "COMMENTED".to_string(),
            commit_sha: "head-44".to_string(),
            submitted_at: "2026-07-20T12:00:00Z".to_string(),
            summary: format!("Summary\n{marker}"),
            summary_truncated: false,
        };
        assert!(!is_submitted_review_candidate(
            &review,
            "review-bot",
            "sha256:run",
            "head-44",
            "COMMENTED",
        ));

        review.author = "review-bot".to_string();
        assert!(is_submitted_review_candidate(
            &review,
            "review-bot",
            "sha256:run",
            "head-44",
            "COMMENTED",
        ));
    }

    #[test]
    fn review_state_pages_ignore_unprivileged_markers_and_keep_cross_session_collaborators() {
        let output = crate::backend::BackendSuccess {
            stdout: serde_json::json!({
                "data": {
                    "viewer": {"login": "review-bot"},
                    "repository": {"pullRequest": {"comments": {
                        "nodes": [
                            {"author": {"login": "attacker"}, "authorAssociation": "NONE", "body": "<!-- forge-cli:review-state:v1 xyz -->", "createdAt": "2026-07-20T00:00:00Z"},
                            {"author": {"login": "review-bot"}, "authorAssociation": "NONE", "body": "ordinary comment", "createdAt": "2026-07-20T00:00:01Z"},
                            {"author": {"login": "maintainer"}, "authorAssociation": "MEMBER", "body": "cross-session comment", "createdAt": "2026-07-20T00:00:02Z"}
                        ],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }}}
                }
            })
            .to_string(),
            stderr: String::new(),
        };
        let page = parse_review_state_page(&output).expect("trusted page");
        assert_eq!(page.trusted_comments[0].body, "ordinary comment");
        assert_eq!(page.trusted_comments[1].body, "cross-session comment");
        assert!(
            review_state::parse_chain(
                page.trusted_comments
                    .iter()
                    .map(|comment| comment.body.as_str()),
                "acme/widgets",
                44,
            )
            .expect("foreign marker ignored")
            .records
            .is_empty()
        );

        let trusted_malformed = crate::backend::BackendSuccess {
            stdout: output
                .stdout
                .replace("ordinary comment", "<!-- forge-cli:review-state:v1 xyz -->"),
            stderr: String::new(),
        };
        let page = parse_review_state_page(&trusted_malformed).expect("trusted page");
        let error = review_state::parse_chain(
            page.trusted_comments
                .iter()
                .map(|comment| comment.body.as_str()),
            "acme/widgets",
            44,
        )
        .expect_err("a malformed viewer-owned marker must fail closed");
        assert_eq!(error.kind(), "review_state_conflict");
    }

    #[test]
    fn review_state_pages_match_github_app_viewer_without_trusting_lookalikes() {
        let output = crate::backend::BackendSuccess {
            stdout: serde_json::json!({
                "data": {
                    "viewer": {"login": "review-bot[bot]"},
                    "repository": {"pullRequest": {"comments": {
                        "nodes": [
                            {"author": {"login": "review-bot"}, "authorAssociation": "CONTRIBUTOR", "body": "app-owned", "createdAt": "2026-07-21T00:00:00Z"},
                            {"author": {"login": "review-bot-lookalike"}, "authorAssociation": "CONTRIBUTOR", "body": "suffix-lookalike", "createdAt": "2026-07-21T00:00:01Z"},
                            {"author": {"login": "review-bot[bot]-lookalike"}, "authorAssociation": "CONTRIBUTOR", "body": "bot-suffix-lookalike", "createdAt": "2026-07-21T00:00:02Z"},
                            {"author": {"login": "unrelated-contributor"}, "authorAssociation": "CONTRIBUTOR", "body": "generic-contributor", "createdAt": "2026-07-21T00:00:03Z"},
                            {"author": {"login": "repository-owner"}, "authorAssociation": "OWNER", "body": "owner", "createdAt": "2026-07-21T00:00:04Z"},
                            {"author": {"login": "repository-member"}, "authorAssociation": "MEMBER", "body": "member", "createdAt": "2026-07-21T00:00:05Z"},
                            {"author": {"login": "repository-collaborator"}, "authorAssociation": "COLLABORATOR", "body": "collaborator", "createdAt": "2026-07-21T00:00:06Z"}
                        ],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }}}
                }
            })
            .to_string(),
            stderr: String::new(),
        };

        let page = parse_review_state_page(&output).expect("trusted page");
        assert_eq!(
            page.trusted_comments
                .iter()
                .map(|comment| comment.body.as_str())
                .collect::<Vec<_>>(),
            ["app-owned", "owner", "member", "collaborator"]
        );
    }

    #[test]
    fn review_loop_append_rechecks_tip_and_reads_back_a_privileged_cross_session_record() {
        let observation = review_state::ReviewFindingObservation {
            fingerprint: "correctness:review-loop:typed-state".to_string(),
            root_cause_fingerprint: None,
            blocking: true,
            status: review_state::ReviewFindingStatus::Open,
            threads: vec!["PRRT_1".to_string()],
        };
        let state = review_state::observe_review_loop(None, "head-44", &[observation])
            .expect("genesis transition")
            .state;
        let record = review_state::ReviewStateRecord::new(
            "acme/widgets",
            44,
            "head-44",
            0,
            None,
            review_state::ReviewStatePayload::ReviewLoop {
                state: state.clone(),
            },
        )
        .expect("record");
        let marker = record.marker().expect("marker");
        let empty_snapshot = crate::backend::BackendSuccess {
            stdout: serde_json::json!({
                "data": {
                    "viewer": {"login": "next-session-bot"},
                    "repository": {"pullRequest": {"comments": {
                        "nodes": [],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }}}
                }
            })
            .to_string(),
            stderr: String::new(),
        };
        let appended_snapshot = crate::backend::BackendSuccess {
            stdout: serde_json::json!({
                "data": {
                    "viewer": {"login": "next-session-bot"},
                    "repository": {"pullRequest": {"comments": {
                        "nodes": [{
                            "author": {"login": "prior-session-bot"},
                            "authorAssociation": "COLLABORATOR",
                            "body": marker,
                            "createdAt": "2026-07-20T00:00:01Z"
                        }],
                        "pageInfo": {"hasNextPage": false, "endCursor": "tip"}
                    }}}
                }
            })
            .to_string(),
            stderr: String::new(),
        };
        let runner = ReceiptRunner {
            outputs: RefCell::new(vec![
                empty_snapshot,
                crate::backend::BackendSuccess {
                    stdout: "https://github.com/acme/widgets/issues/44#issuecomment-1".to_string(),
                    stderr: String::new(),
                },
                appended_snapshot,
            ]),
            calls: RefCell::new(Vec::new()),
        };

        let chain = append_review_loop_state(
            &runner,
            &ctx(Some("acme/widgets")),
            "acme/widgets",
            44,
            "head-44",
            None,
            state,
        )
        .expect("append and read back");

        assert_eq!(chain.records.len(), 1);
        assert_eq!(
            chain.tip_digest.as_deref(),
            Some(record.record_digest.as_str())
        );
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 3);
        assert!(calls[1].contains("issues/44/comments"), "{}", calls[1]);
    }

    #[test]
    fn receipt_append_verification_starts_after_the_prior_state_cursor() {
        let prior_receipt = review_state::ReviewRunReceipt {
            review_run_id: "sha256:prior".to_string(),
            route_lenses: Vec::new(),
            decision: "comments-only".to_string(),
            expected_head: "head-44".to_string(),
            round: 0,
            summary_digest: "sha256:prior-summary".to_string(),
            inline_manifest: Vec::new(),
        };
        let prior_record = review_state::ReviewStateRecord::new(
            "acme/widgets",
            44,
            "head-44",
            0,
            None,
            review_state::ReviewStatePayload::ReviewRunReceipt {
                receipt: prior_receipt,
            },
        )
        .expect("prior record");
        let prior_marker = prior_record.marker().expect("prior marker");
        let state = ReviewStateSnapshot {
            chain: review_state::parse_chain([prior_marker.as_str()], "acme/widgets", 44)
                .expect("prior chain"),
            trusted_comments: vec![ReviewStateComment {
                body: prior_marker,
                created_at: "2026-07-20T00:00:00Z".to_string(),
            }],
            viewer_login: "review-bot".to_string(),
            end_cursor: Some("state-tip".to_string()),
        };
        let receipt = review_state::ReviewRunReceipt {
            review_run_id: "sha256:next".to_string(),
            route_lenses: vec!["security".to_string()],
            decision: "comments-only".to_string(),
            expected_head: "head-44".to_string(),
            round: 1,
            summary_digest: "sha256:next-summary".to_string(),
            inline_manifest: Vec::new(),
        };
        let next_record = review_state::ReviewStateRecord::new(
            "acme/widgets",
            44,
            "head-44",
            1,
            state.chain.tip_digest.clone(),
            review_state::ReviewStatePayload::ReviewRunReceipt {
                receipt: receipt.clone(),
            },
        )
        .expect("next record");
        let next_marker = next_record.marker().expect("next marker");
        let runner = ReceiptRunner {
            outputs: RefCell::new(vec![
                crate::backend::BackendSuccess {
                    stdout: "https://github.com/acme/widgets/issues/44#issuecomment-2".to_string(),
                    stderr: String::new(),
                },
                crate::backend::BackendSuccess {
                    stdout: serde_json::json!({
                        "data": {
                            "viewer": {"login": "review-bot"},
                            "repository": {"pullRequest": {"comments": {
                                "nodes": [{
                                    "author": {"login": "review-bot"},
                                    "body": next_marker,
                                    "createdAt": "2026-07-20T00:00:01Z"
                                }],
                                "pageInfo": {"hasNextPage": false, "endCursor": "state-next"}
                            }}}
                        }
                    })
                    .to_string(),
                    stderr: String::new(),
                },
            ]),
            calls: RefCell::new(Vec::new()),
        };

        ensure_review_run_receipt(
            &runner,
            &ctx(Some("acme/widgets")),
            "acme/widgets",
            44,
            &state,
            &receipt,
        )
        .expect("receipt append verified from cursor tail");

        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].contains("issues/44/comments"), "{}", calls[0]);
        assert!(calls[1].contains("after=state-tip"), "{}", calls[1]);
    }
}
