//! `pr review` atom.
//!
//! Spec / ops: `cli.forge-cli.pr.review.v1`. This is intentionally a posting
//! primitive, not a review-orchestration engine: callers pass an already
//! rendered review outcome, and forge-cli posts it to the PR/MR plus an
//! optional compact issue activity mirror.

use std::{collections::HashMap, ffi::OsString, fs, io::Read};

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::{Deserialize, Serialize};

use crate::backend::{BackendCall, BackendProgram, BackendRunner};
use crate::cli::{
    BINARY, GlobalFlags, PrReviewArgs, PrReviewCommand, PrReviewDecision, PrReviewValidateArgs,
};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_comment;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;
use crate::validations::{no_escaped_control_markdown, no_local_path};

const SCHEMA: &str = "pr.review";
const SCHEMA_VERSION: u32 = 1;
const REVIEW_THREAD_FILE_MAX_BYTES: u64 = 256 * 1024;
const REVIEW_THREAD_MAX_COUNT: usize = 50;
const REVIEW_THREAD_PATH_MAX_BYTES: usize = 1024;
const REVIEW_THREAD_BODY_MAX_BYTES: usize = 16 * 1024;

/// Placeholder `review_url` used only to validate the generated issue mirror
/// body *before* the PR comment is posted. The real URL is provider-returned
/// (never user-controlled), so validating the user-controlled parts — the
/// `--lens` values embedded in the mirror — against this placeholder is
/// sufficient to catch a bad lens before any backend mutation.
const MIRROR_URL_PENDING: &str = "<pending>";

const GITHUB_REVIEW_TARGET_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { id url } } }";
const GITHUB_ADD_PENDING_REVIEW_MUTATION: &str = "mutation($pullRequestId: ID!) { addPullRequestReview(input: {pullRequestId: $pullRequestId}) { pullRequestReview { id url } } }";
const GITHUB_ADD_REVIEW_THREAD_MUTATION: &str = "mutation($reviewId: ID!, $path: String!, $body: String!, $line: Int, $side: DiffSide!, $startLine: Int, $startSide: DiffSide, $subjectType: PullRequestReviewThreadSubjectType!) { addPullRequestReviewThread(input: {pullRequestReviewId: $reviewId, path: $path, body: $body, line: $line, side: $side, startLine: $startLine, startSide: $startSide, subjectType: $subjectType}) { thread { id path line subjectType comments(first: 1) { nodes { url } } } } }";
const GITHUB_SUBMIT_REVIEW_MUTATION: &str = "mutation($reviewId: ID!, $event: PullRequestReviewEvent!, $body: String) { submitPullRequestReview(input: {pullRequestReviewId: $reviewId, event: $event, body: $body}) { pullRequestReview { url } } }";
const GITHUB_DELETE_PENDING_REVIEW_MUTATION: &str = "mutation($reviewId: ID!) { deletePullRequestReview(input: {pullRequestReviewId: $reviewId}) { pullRequestReview { id url } } }";

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
    pub pr_comment_url: String,
    pub issue_number: Option<u64>,
    pub issue_comment_url: Option<String>,
    pub mirrored: bool,
    pub lenses: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_threads: Vec<CreatedReviewThread>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct PrReviewDryRunPayload {
    provider: &'static str,
    number: u64,
    decision: &'static str,
    /// `true` when the live run would submit a native review event
    /// (`--submit-review`); `false` for the outcome-comment form.
    submitted_review: bool,
    /// GitHub-only PR-existence guard read that runs before the live post.
    /// `None` on GitLab / Local. Surfaced so dry-run renders every backend
    /// command the live run performs.
    guard_plan: Option<Vec<String>>,
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
                build_github_pending_review_call(&ctx, "<pull-request-id>"),
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
                guard_plan,
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
    }

    // GitLab only: probe which `mr note` form this `glab` build supports
    // (create+resolvable / create-only / bare). For GitHub / Local the form is
    // unused, so skip the probe and pass the default.
    let glab_form = if ctx.provider == Provider::GitLab {
        glab_note_form(runner)
    } else {
        GlabNoteForm::CreateResolvable
    };
    let (pr_comment_url, review_threads) = if thread_specs.is_empty() {
        let pr_call = build_review_post_call(&ctx, id, &args, &body, body_present, glab_form);
        let pr_output = runner.run(&pr_call).map_err(|err| {
            if args.submit_review && ctx.provider == Provider::GitHub {
                map_github_native_review_submit_error(args.decision, err)
            } else {
                err
            }
        })?;
        (first_url(&pr_output.stdout).unwrap_or_default(), Vec::new())
    } else {
        submit_github_review_with_threads(
            runner,
            &ctx,
            id,
            args.decision,
            body_present.then_some(body.as_str()),
            &thread_specs,
        )?
    };

    let issue_comment_url = if let Some(issue_number) = mirror_issue {
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
            submitted_review: args.submit_review,
            pr_comment_url,
            issue_number: args.issue,
            issue_comment_url,
            mirrored: args.mirror_issue,
            lenses: args.lenses,
            review_threads,
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
/// guarded earlier in `run_with`).
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
            github_review_submit_argv(ctx, id, event, body_opt),
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

fn submit_github_review_with_threads<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    number: u64,
    decision: PrReviewDecision,
    body: Option<&str>,
    specs: &[PreparedReviewThreadSpec],
) -> Result<(String, Vec<CreatedReviewThread>), ForgeError> {
    let (owner, name) = github_owner_name(ctx)?;
    let target_output = runner.run(&build_github_review_target_call(ctx, owner, name, number))?;
    let target = parse_github_review_target(&target_output)?;
    let _target_url = target.url;

    let pending_output = runner.run(&build_github_pending_review_call(
        ctx,
        &target.pull_request_id,
    ))?;
    let pending = parse_github_pending_review(&pending_output)?;

    let mut review_threads = Vec::with_capacity(specs.len());
    for (idx, spec) in specs.iter().enumerate() {
        let output = match runner.run(&build_github_add_review_thread_call(
            ctx,
            &pending.review_id,
            spec,
        )) {
            Ok(output) => output,
            Err(err) => {
                let err = map_github_review_thread_error(idx + 1, spec, err);
                return Err(cleanup_pending_github_review(runner, ctx, &pending, err));
            }
        };
        let thread = match parse_created_review_thread(&output, spec) {
            Ok(thread) => thread,
            Err(err) => return Err(cleanup_pending_github_review(runner, ctx, &pending, err)),
        };
        review_threads.push(thread);
    }

    let submit_output = match runner.run(&build_github_submit_review_call(
        ctx,
        &pending.review_id,
        decision.to_github_event(),
        body,
    )) {
        Ok(output) => output,
        Err(err) => {
            let err = map_github_native_review_submit_error(decision, err);
            return Err(cleanup_pending_github_review(runner, ctx, &pending, err));
        }
    };
    let review_url = parse_submitted_review_url(&submit_output).unwrap_or(pending.url);
    Ok((review_url, review_threads))
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
            "pr review --thread-file requires --repo owner/name or a recognised GitHub remote",
            None,
        ));
    };
    repo.split_once('/').ok_or_else(|| {
        ForgeError::validation(
            schema_err(),
            "repo_required",
            "pr review --thread-file requires a repo slug shaped as owner/name",
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

fn build_github_pending_review_call(ctx: &ProviderContext, pull_request_id: &str) -> BackendCall {
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from("-f"),
        OsString::from(format!("query={GITHUB_ADD_PENDING_REVIEW_MUTATION}")),
        OsString::from("-f"),
        OsString::from(format!("pullRequestId={pull_request_id}")),
    ]);
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

fn build_github_submit_review_call(
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

fn build_github_delete_pending_review_call(ctx: &ProviderContext, review_id: &str) -> BackendCall {
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

fn cleanup_pending_github_review<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    pending: &GitHubPendingReview,
    cause: ForgeError,
) -> ForgeError {
    match runner.run(&build_github_delete_pending_review_call(
        ctx,
        &pending.review_id,
    )) {
        Ok(_) => cause,
        Err(cleanup_err) => with_cleanup_detail(cause, pending, cleanup_err),
    }
}

fn with_cleanup_detail(
    cause: ForgeError,
    pending: &GitHubPendingReview,
    cleanup_err: ForgeError,
) -> ForgeError {
    let cleanup_detail = format!(
        "pending_review_id={id}; pending_review_url={url}; cleanup_error_kind={cleanup_kind}; cleanup_error_message={cleanup_message}; cleanup_error_detail={cleanup_detail}",
        id = pending.review_id,
        url = pending.url,
        cleanup_kind = cleanup_err.kind(),
        cleanup_message = cleanup_err.message(),
        cleanup_detail = cleanup_err.detail().unwrap_or(""),
    );
    let merged_detail = match cause.detail() {
        Some(detail) if !detail.is_empty() => format!("{detail}; {cleanup_detail}"),
        _ => cleanup_detail,
    };
    match cause {
        ForgeError::NotImplemented {
            schema_version,
            message,
        } => ForgeError::not_implemented(schema_version, message),
        ForgeError::BackendUnavailable {
            schema_version,
            kind,
            message,
            ..
        } => ForgeError::unavailable(schema_version, kind, message, Some(merged_detail)),
        ForgeError::BackendError {
            schema_version,
            message,
            ..
        } => ForgeError::backend_error(schema_version, message, Some(merged_detail)),
        ForgeError::ProviderUnsupported {
            schema_version,
            message,
            ..
        } => ForgeError::provider_unsupported(schema_version, message, Some(merged_detail)),
        ForgeError::SoftwareError {
            schema_version,
            message,
            ..
        } => ForgeError::software(schema_version, message, Some(merged_detail)),
        ForgeError::Validation {
            schema_version,
            kind,
            message,
            ..
        } => ForgeError::validation(schema_version, kind, message, Some(merged_detail)),
        ForgeError::RuntimeFailure {
            schema_version,
            kind,
            message,
            ..
        } => ForgeError::runtime_failure(schema_version, kind, message, Some(merged_detail)),
    }
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

fn parse_submitted_review_url(output: &crate::backend::BackendSuccess) -> Option<String> {
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
    use super::*;
    use crate::provider::DetectionSource;

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
            Some("nope"),
        );
        let joined = argv_joined(&argv);
        assert!(
            joined.contains("repos/acme/widgets/pulls/44/reviews"),
            "{joined}"
        );
        assert!(joined.contains("--method POST"), "{joined}");
        assert!(joined.contains("event=REQUEST_CHANGES"), "{joined}");
        assert!(joined.contains("body=nope"), "{joined}");
        assert!(joined.contains("--jq .html_url"), "{joined}");
    }

    #[test]
    fn submit_argv_omits_body_field_when_none() {
        let argv = github_review_submit_argv(&ctx(Some("acme/widgets")), 44, "APPROVE", None);
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
        let argv = github_review_submit_argv(&ctx, 44, "COMMENT", Some("note"));
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
}
