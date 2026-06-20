//! `pr review` atom.
//!
//! Spec / ops: `cli.forge-cli.pr.review.v1`. This is intentionally a posting
//! primitive, not a review-orchestration engine: callers pass an already
//! rendered review outcome, and forge-cli posts it to the PR/MR plus an
//! optional compact issue activity mirror.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, ProcessRunner};
use crate::cli::{BINARY, GlobalFlags, PrReviewArgs, PrReviewDecision};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_comment;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::validations::{no_escaped_control_markdown, no_local_path};

const SCHEMA: &str = "pr.review";
const SCHEMA_VERSION: u32 = 1;

/// Placeholder `review_url` used only to validate the generated issue mirror
/// body *before* the PR comment is posted. The real URL is provider-returned
/// (never user-controlled), so validating the user-controlled parts — the
/// `--lens` values embedded in the mirror — against this placeholder is
/// sufficient to catch a bad lens before any backend mutation.
const MIRROR_URL_PENDING: &str = "<pending>";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrReviewPayload {
    pub provider: &'static str,
    pub number: u64,
    pub decision: &'static str,
    pub pr_comment_url: String,
    pub issue_number: Option<u64>,
    pub issue_comment_url: Option<String>,
    pub mirrored: bool,
    pub lenses: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct PrReviewDryRunPayload {
    provider: &'static str,
    number: u64,
    decision: &'static str,
    plan: Vec<String>,
    issue_number: Option<u64>,
    issue_plan: Option<Vec<String>>,
    mirror_issue: bool,
    lenses: Vec<String>,
}

pub fn run(
    global: &GlobalFlags,
    args: PrReviewArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrReviewArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
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

    let body = pr_comment::read_body_with_file_flag(
        args.comment.as_deref(),
        args.comment_file.as_deref(),
        "--comment-file",
    )?;
    if body.trim().is_empty() {
        return Err(ForgeError::validation(
            schema_err(),
            "body_missing_summary",
            "review comment body is empty (supply --comment or --comment-file)",
            None,
        ));
    }
    no_local_path(&body, "review comment")?;
    no_escaped_control_markdown(&body)?;

    // Resolve and validate the issue-mirror inputs BEFORE any backend mutation
    // (validate-before-side-effect). The generated mirror body embeds
    // user-controlled `--lens` values, so it must hit the same `no_local_path`
    // and escaped-control guards the review body does — otherwise a bad lens
    // either leaks a local path to the provider issue or fails only after the
    // PR comment was already posted, leaving an outcome comment with no mirror.
    let mirror_issue = if args.mirror_issue {
        let issue_number = args.issue.ok_or_else(issue_required_err)?;
        let preview = build_issue_mirror_body(
            ctx.provider,
            args.id,
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

    let pr_call = build_pr_comment_call(&ctx, args.id, &body);

    if global.dry_run {
        let issue_plan = mirror_issue.map(|issue| {
            let mirror_body = build_issue_mirror_body(
                ctx.provider,
                args.id,
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
                number: args.id,
                decision: args.decision.as_str(),
                plan: pr_call.plan_argv(),
                issue_number: args.issue,
                issue_plan,
                mirror_issue: args.mirror_issue,
                lenses: args.lenses.clone(),
            },
            format,
            |p| {
                println!("would post review outcome: {plan}", plan = p.plan.join(" "));
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
        ensure_github_pull_request(runner, &ctx, args.id)?;
    }

    let pr_output = runner.run(&pr_call)?;
    let pr_comment_url = first_url(&pr_output.stdout).unwrap_or_default();

    let issue_comment_url = if let Some(issue_number) = mirror_issue {
        // The mirror body's user-controlled content (lenses) was already
        // validated up front against MIRROR_URL_PENDING; the only difference
        // here is the provider-returned `pr_comment_url`, which is never
        // user-controlled, so it needs no re-validation after the post.
        let mirror_body = build_issue_mirror_body(
            ctx.provider,
            args.id,
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
            number: args.id,
            decision: args.decision.as_str(),
            pr_comment_url,
            issue_number: args.issue,
            issue_comment_url,
            mirrored: args.mirror_issue,
            lenses: args.lenses,
        },
        format,
        render_text,
    ))
}

fn build_pr_comment_call(ctx: &ProviderContext, id: u64, body: &str) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub => github_issue_comment_argv(ctx, id, body),
        // `glab mr note create … --resolvable=false` posts the outcome as a
        // non-resolvable note. The bare `glab mr note <id>` form creates a
        // *resolvable* discussion, which `forge-cli pr merge`'s GitLab gate
        // counts as an unresolved review thread — so a comments-only / approve
        // `pr review` would otherwise block the next merge until a human
        // resolves it. A review *outcome* is a status note, not a blocking
        // thread, so it must be created non-resolvable.
        Provider::GitLab => vec![
            OsString::from("mr"),
            OsString::from("note"),
            OsString::from("create"),
            OsString::from(id.to_string()),
            OsString::from("--message"),
            OsString::from(body),
            OsString::from("--resolvable=false"),
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

/// Verify `<id>` resolves to a pull request on GitHub before posting a review
/// outcome through the issue-comments API. Uses `run_raw` so a `Not Found`
/// (the id is an issue or does not exist) becomes a `DATA 65` validation error
/// rather than a generic backend failure; auth / launch failures still
/// propagate as their own error kinds.
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
    Err(ForgeError::validation(
        schema_err(),
        "id_not_pull_request",
        format!(
            "#{id} is not a pull request on github; refusing to post a review outcome onto an issue or a missing number"
        ),
        (!detail.is_empty()).then(|| detail.to_string()),
    ))
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
    if let Some(issue_url) = payload.issue_comment_url.as_deref() {
        println!(
            "posted {decision} review outcome on {provider} #{number}: {pr_url}; mirrored issue activity: {issue_url}",
            decision = payload.decision,
            provider = payload.provider,
            number = payload.number,
            pr_url = payload.pr_comment_url,
        );
    } else {
        println!(
            "posted {decision} review outcome on {provider} #{number}: {pr_url}",
            decision = payload.decision,
            provider = payload.provider,
            number = payload.number,
            pr_url = payload.pr_comment_url,
        );
    }
}
