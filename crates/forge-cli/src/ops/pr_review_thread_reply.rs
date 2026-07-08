//! `pr review-threads reply` atom — post a reply onto a review thread without
//! resolving it.
//!
//! Spec / ops: `cli.forge-cli.pr.review-threads.reply.v1`. GitHub-first: the
//! thread node id (`PRRT_...` from the read surface) keys the
//! `addPullRequestReviewThreadReply` mutation (`pullRequestReviewThreadId`).
//! Unlike `pr review-threads resolve`, this op never resolves — it only
//! appends the reply and surfaces the new comment url.
//!
//! GitLab and Local have no GitHub-shaped thread-mutation surface, so they
//! return a structured `provider_unsupported` error (GitHub-first in v1).

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, DryRunPayload};
use crate::cli::{BINARY, GlobalFlags, PrReviewThreadReplyArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_comment::read_body;
use crate::ops::pr_review_threads;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;
use crate::validations::no_local_path;

const SCHEMA: &str = "pr.review-threads.reply";
const SCHEMA_VERSION: u32 = 1;

/// GitHub mutation that posts a reply onto an existing review thread and
/// returns the new comment's url.
const GITHUB_REPLY_MUTATION: &str = "mutation($tid: ID!, $body: String!) { addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $tid, body: $body}) { comment { url } } }";

/// Envelope payload for `cli.forge-cli.pr.review-threads.reply.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrReviewThreadReplyPayload {
    pub provider: &'static str,
    pub thread_id: String,
    pub comment_url: String,
}

pub fn run(
    global: &GlobalFlags,
    args: PrReviewThreadReplyArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrReviewThreadReplyArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;

    ensure_github(&ctx)?;

    let body = read_body(args.body.as_deref(), args.body_file.as_deref())?;
    if body.trim().is_empty() {
        return Err(ForgeError::validation(
            schema_err(),
            "body_missing_summary",
            "reply body is empty (supply --body or --body-file)",
            None,
        ));
    }
    no_local_path(&body, "reply")?;

    let call = build_reply_call(&ctx, &args.thread, &body);

    if global.dry_run {
        let payload = DryRunPayload::new(ctx.provider, &call);
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            payload,
            format,
            |p| println!("would run: {plan}", plan = p.plan.join(" ")),
        ));
    }

    pr_review_threads::ensure_thread_belongs_to_pr(runner, &ctx, args.id, &args.thread)?;

    let output = runner.run(&call)?;
    let comment_url = parse_comment_url(&output.stdout);

    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        PrReviewThreadReplyPayload {
            provider: ctx.provider.as_str(),
            thread_id: args.thread,
            comment_url,
        },
        format,
        render_text,
    ))
}

/// GitHub-first: GitLab and Local fail closed with a structured
/// `provider_unsupported` error before any backend call.
fn ensure_github(ctx: &ProviderContext) -> Result<(), ForgeError> {
    match ctx.provider {
        Provider::GitHub => Ok(()),
        Provider::GitLab | Provider::Local => Err(ForgeError::provider_unsupported(
            schema_err(),
            format!(
                "pr review-threads reply is GitHub-only in v1 (provider: {provider})",
                provider = ctx.provider.as_str(),
            ),
            None,
        )),
    }
}

pub(crate) fn build_reply_call(ctx: &ProviderContext, thread_id: &str, body: &str) -> BackendCall {
    debug_assert!(matches!(ctx.provider, Provider::GitHub));
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from("-f"),
        OsString::from(format!("query={GITHUB_REPLY_MUTATION}")),
        OsString::from("-f"),
        OsString::from(format!("tid={thread_id}")),
        OsString::from("-f"),
        OsString::from(format!("body={body}")),
    ]);
    BackendCall::new(BackendProgram::Gh, argv)
}

/// Pull the new comment url out of the mutation response. Best-effort: an
/// absent url yields the empty string rather than an error, since the reply
/// itself succeeded.
fn parse_comment_url(stdout: &str) -> String {
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()
        .as_ref()
        .and_then(|v| v.pointer("/data/addPullRequestReviewThreadReply/comment/url"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrReviewThreadReplyPayload) {
    println!(
        "replied to {provider} review thread {thread}: {url}",
        provider = payload.provider,
        thread = payload.thread_id,
        url = payload.comment_url,
    );
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use nils_common::cli_contract::{OutputFormat, exit};
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::backend::{BackendOutput, BackendSuccess};
    use crate::cli::ProviderFlag;
    use crate::provider::DetectionSource;

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

    fn global(provider: ProviderFlag, dry_run: bool) -> GlobalFlags {
        GlobalFlags {
            format: Some(OutputFormat::Json),
            remote: "origin".into(),
            provider: Some(provider),
            repo: Some("acme/widgets".into()),
            store_root: None,
            dry_run,
        }
    }

    fn args(thread: &str, body: Option<&str>, body_file: Option<&str>) -> PrReviewThreadReplyArgs {
        PrReviewThreadReplyArgs {
            id: 7,
            thread: thread.to_string(),
            body: body.map(str::to_string),
            body_file: body_file.map(str::to_string),
        }
    }

    #[test]
    fn build_reply_call_uses_add_reply_mutation_with_tid_and_body() {
        let call = build_reply_call(&ctx(Provider::GitHub), "PRRT_abc", "ack");
        let argv = call.plan_argv();
        assert_eq!(call.program, BackendProgram::Gh);
        assert!(argv.iter().any(|s| s == "graphql"));
        assert!(
            argv.iter()
                .any(|s| s.contains("addPullRequestReviewThreadReply"))
        );
        assert!(argv.iter().any(|s| s.contains("pullRequestReviewThreadId")));
        assert!(argv.iter().any(|s| s == "tid=PRRT_abc"));
        assert!(argv.iter().any(|s| s == "body=ack"));
        // Reply must NOT resolve.
        assert!(!argv.iter().any(|s| s.contains("resolveReviewThread")));
    }

    #[test]
    fn build_reply_call_adds_hostname_for_enterprise_host() {
        let mut ctx = ctx(Provider::GitHub);
        ctx.host = "internal.ghe.com".into();
        let argv = build_reply_call(&ctx, "PRRT_abc", "ack").plan_argv();
        let pos = argv
            .iter()
            .position(|s| s == "--hostname")
            .expect("enterprise host must be passed to gh api");
        assert_eq!(argv[pos + 1], "internal.ghe.com");
    }

    fn pr_view_json(number: u64) -> BackendSuccess {
        BackendSuccess {
            stdout: format!(
                r#"{{"number":{number},"url":"https://github.com/acme/widgets/pull/{number}","state":"OPEN","isDraft":false,"title":"demo","headRefName":"feat/x","baseRefName":"main","mergeable":"MERGEABLE","mergedAt":null,"labels":[]}}"#
            ),
            stderr: String::new(),
        }
    }

    fn github_threads_json(ids: &[&str]) -> BackendSuccess {
        let nodes: Vec<String> = ids
            .iter()
            .map(|id| {
                format!(
                    r#"{{"id":"{id}","isResolved":false,"isOutdated":false,"path":"src/lib.rs","comments":{{"nodes":[{{"author":{{"login":"reviewer"}},"body":"finding","createdAt":"t","url":"https://github.com/acme/widgets/pull/7#discussion_r1"}}]}}}}"#
                )
            })
            .collect();
        BackendSuccess {
            stdout: format!(
                r#"{{"data":{{"repository":{{"pullRequest":{{"reviewThreads":{{"nodes":[{}]}}}}}}}}}}"#,
                nodes.join(",")
            ),
            stderr: String::new(),
        }
    }

    #[test]
    fn run_with_posts_single_reply_and_surfaces_comment_url() {
        let runner = ScriptedRunner::new(vec![
            pr_view_json(7),
            github_threads_json(&["PRRT_abc"]),
            BackendSuccess {
                stdout: r#"{"data":{"addPullRequestReviewThreadReply":{"comment":{"url":"https://github.com/acme/widgets/pull/7#discussion_r9"}}}}"#
                    .into(),
                stderr: String::new(),
            },
        ]);
        let code = run_with(
            &runner,
            &global(ProviderFlag::Github, false),
            args("PRRT_abc", Some("ack"), None),
            OutputFormat::Json,
            |_| Some("git@github.com:acme/widgets.git".into()),
        )
        .expect("reply");
        assert_eq!(code, exit::SUCCESS);
        let calls = runner.calls();
        assert_eq!(
            calls.len(),
            3,
            "view + thread validation run before the reply mutation"
        );
        assert!(
            calls[2]
                .1
                .iter()
                .any(|s| s.contains("addPullRequestReviewThreadReply"))
        );
    }

    #[test]
    fn run_with_reply_rejects_thread_from_another_pr_before_mutating() {
        let runner =
            ScriptedRunner::new(vec![pr_view_json(7), github_threads_json(&["PRRT_other"])]);
        let err = run_with(
            &runner,
            &global(ProviderFlag::Github, false),
            args("PRRT_target", Some("ack"), None),
            OutputFormat::Json,
            |_| Some("git@github.com:acme/widgets.git".into()),
        )
        .expect_err("thread does not belong to PR");
        assert_eq!(err.kind(), "review_thread_pr_mismatch");
        let calls = runner.calls();
        assert_eq!(calls.len(), 2, "must only view PR and list its threads");
        assert!(
            !calls.iter().any(|(_, argv)| argv
                .iter()
                .any(|s| s.contains("addPullRequestReviewThreadReply"))),
            "reply mutation must not run when the thread is not on the PR"
        );
    }

    #[test]
    fn run_with_rejects_empty_body() {
        let runner = ScriptedRunner::new(vec![]);
        let err = run_with(
            &runner,
            &global(ProviderFlag::Github, false),
            args("PRRT_abc", Some("   "), None),
            OutputFormat::Json,
            |_| Some("git@github.com:acme/widgets.git".into()),
        )
        .expect_err("empty body");
        assert_eq!(err.kind(), "body_missing_summary");
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn run_with_dry_run_plans_nothing() {
        let runner = ScriptedRunner::new(vec![]);
        let code = run_with(
            &runner,
            &global(ProviderFlag::Github, true),
            args("PRRT_abc", Some("ack"), None),
            OutputFormat::Json,
            |_| Some("git@github.com:acme/widgets.git".into()),
        )
        .expect("dry-run");
        assert_eq!(code, exit::SUCCESS);
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn run_with_gitlab_is_provider_unsupported() {
        let runner = ScriptedRunner::new(vec![]);
        let err = run_with(
            &runner,
            &global(ProviderFlag::Gitlab, false),
            args("d_1", Some("ack"), None),
            OutputFormat::Json,
            |_| Some("git@gitlab.com:acme/widgets.git".into()),
        )
        .expect_err("gitlab unsupported");
        assert_eq!(err.kind(), "provider_unsupported");
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn run_with_local_is_provider_unsupported() {
        let runner = ScriptedRunner::new(vec![]);
        let err = run_with(
            &runner,
            &global(ProviderFlag::Local, false),
            args("x", Some("ack"), None),
            OutputFormat::Json,
            |_| None,
        )
        .expect_err("local unsupported");
        assert_eq!(err.kind(), "provider_unsupported");
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn parse_comment_url_extracts_url_or_empty() {
        assert_eq!(
            parse_comment_url(
                r#"{"data":{"addPullRequestReviewThreadReply":{"comment":{"url":"u"}}}}"#
            ),
            "u"
        );
        assert_eq!(parse_comment_url("{}"), "");
        assert_eq!(parse_comment_url("not json"), "");
    }
}
