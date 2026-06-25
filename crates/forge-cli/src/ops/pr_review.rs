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
    // comment body so a missing `--issue` fails fast with `issue_required`
    // rather than first blocking on stdin (`--comment-file -`) or surfacing a
    // file-read `software_error`. (`--mirror-issue` carries no clap
    // `requires = "issue"`, so this runtime guard is the only enforcement point.)
    let mirror_issue_number = if args.mirror_issue {
        Some(args.issue.ok_or_else(issue_required_err)?)
    } else {
        None
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

    if global.dry_run {
        // dry-run must not touch a backend, so it cannot probe `glab` capability;
        // render the preferred non-resolvable GitLab form (the live run may pick
        // a more compatible form on older `glab`).
        let pr_call = build_review_post_call(
            &ctx,
            &args,
            &body,
            body_present,
            GlabNoteForm::CreateResolvable,
        );
        // The GitHub PR-existence guard is a live backend read; surface it in the
        // dry-run plan so wrappers inspecting dry-run output see every call.
        let guard_plan = (ctx.provider == Provider::GitHub).then(|| {
            BackendCall::new(BackendProgram::Gh, github_pull_lookup_argv(&ctx, args.id)).plan_argv()
        });
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
                submitted_review: args.submit_review,
                guard_plan,
                plan: pr_call.plan_argv(),
                issue_number: args.issue,
                issue_plan,
                mirror_issue: args.mirror_issue,
                lenses: args.lenses.clone(),
            },
            format,
            |p| {
                if let Some(guard) = p.guard_plan.as_ref() {
                    println!("would verify pull request: {plan}", plan = guard.join(" "));
                }
                let verb = if p.submitted_review {
                    "would submit review event"
                } else {
                    "would post review outcome"
                };
                println!("{verb}: {plan}", plan = p.plan.join(" "));
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

    // GitLab only: probe which `mr note` form this `glab` build supports
    // (create+resolvable / create-only / bare). For GitHub / Local the form is
    // unused, so skip the probe and pass the default.
    let glab_form = if ctx.provider == Provider::GitLab {
        glab_note_form(runner)
    } else {
        GlabNoteForm::CreateResolvable
    };
    let pr_call = build_review_post_call(&ctx, &args, &body, body_present, glab_form);

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
            submitted_review: args.submit_review,
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

/// Build the primary "post the review" backend call for the chosen mode: a
/// native GitHub review submission when `--submit-review` is set (the
/// `#pullrequestreview-` object), otherwise the outcome-comment post. Used by
/// both the dry-run plan and the live run so they never diverge. `glab_form` is
/// only consulted on the GitLab comment path (native submission is GitHub-only,
/// guarded earlier in `run_with`).
fn build_review_post_call(
    ctx: &ProviderContext,
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
            github_review_submit_argv(ctx, args.id, event, body_opt),
        )
    } else {
        build_pr_comment_call(ctx, args.id, body, glab_form)
    }
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
            id: 44,
            decision,
            comment: comment.map(str::to_string),
            comment_file: None,
            lenses: Vec::new(),
            issue: None,
            mirror_issue: false,
            submit_review,
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
}
