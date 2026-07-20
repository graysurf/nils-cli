//! `pr review-threads resolve` atom — resolve a review thread, optionally
//! posting a reply first.
//!
//! Spec / ops: `cli.forge-cli.pr.review-threads.resolve.v1`. GitHub-first:
//! the thread node id (`PRRT_...` from the read surface) is the single handle
//! used for both the reply mutation (`addPullRequestReviewThreadReply`,
//! keyed by `pullRequestReviewThreadId`) and the resolve mutation
//! (`resolveReviewThread`, keyed by `threadId`). With `--note` / `--note-file`
//! the reply runs first, then the resolve; without a note only the resolve
//! runs. GitHub's `resolveReviewThread` is idempotent, so resolving an
//! already-resolved thread is success — never an error.
//!
//! GitLab and Local have no GitHub-shaped thread-mutation surface, so they
//! return a structured `provider_unsupported` error (GitHub-first in v1).

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, DryRunPayload};
use crate::cli::{BINARY, GlobalFlags, PrReviewThreadResolveArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_comment::read_body_with_file_flag;
use crate::ops::pr_review_threads;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;
use crate::validations::no_local_path;

const SCHEMA: &str = "pr.review-threads.resolve";
const SCHEMA_VERSION: u32 = 1;

/// GitHub mutation that posts a reply onto an existing review thread.
const GITHUB_REPLY_MUTATION: &str = "mutation($tid: ID!, $body: String!) { addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $tid, body: $body}) { comment { url } } }";

/// GitHub mutation that resolves a review thread. Idempotent: resolving an
/// already-resolved thread succeeds.
const GITHUB_RESOLVE_MUTATION: &str = "mutation($tid: ID!) { resolveReviewThread(input: {threadId: $tid}) { thread { isResolved } } }";

/// Envelope payload for `cli.forge-cli.pr.review-threads.resolve.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrReviewThreadResolvePayload {
    pub provider: &'static str,
    pub thread_id: String,
    pub resolved: bool,
    pub replied: bool,
}

pub fn run(
    global: &GlobalFlags,
    args: PrReviewThreadResolveArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrReviewThreadResolveArgs,
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

    // Resolve the optional reply note. An empty note (e.g. blank file) is
    // treated as "no note" so an accidental empty body doesn't post a comment.
    let note = read_body_with_file_flag(
        args.note.as_deref(),
        args.note_file.as_deref(),
        "--note-file",
    )?;
    let note = if note.trim().is_empty() {
        None
    } else {
        no_local_path(&note, "note")?;
        Some(note)
    };

    let reply_call = note
        .as_deref()
        .map(|body| build_reply_call(&ctx, &args.thread, body));
    let resolve_call = build_resolve_call(&ctx, &args.thread);

    if global.dry_run {
        // Emit the plan(s) and invoke nothing. The resolve call is always
        // planned; the reply call is planned only when a note is supplied.
        let mut plan: Vec<String> = Vec::new();
        if let Some(call) = &reply_call {
            plan.extend(call.plan_argv());
        }
        let resolve_plan = DryRunPayload::new(ctx.provider, &resolve_call);
        plan.extend(resolve_plan.plan.clone());
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            DryRunPayload {
                provider: ctx.provider.as_str(),
                plan,
                review_convergence: None,
            },
            format,
            |p| println!("would run: {plan}", plan = p.plan.join(" ")),
        ));
    }

    pr_review_threads::ensure_thread_belongs_to_pr(runner, &ctx, args.id, &args.thread)?;

    let replied = if let Some(call) = &reply_call {
        runner.run(call)?;
        true
    } else {
        false
    };
    // GitHub's resolveReviewThread is idempotent; a successful call means the
    // thread is resolved regardless of its prior state.
    runner.run(&resolve_call)?;

    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        PrReviewThreadResolvePayload {
            provider: ctx.provider.as_str(),
            thread_id: args.thread,
            resolved: true,
            replied,
        },
        format,
        render_text,
    ))
}

/// GitHub-first: GitLab and Local have no GitHub-shaped thread-mutation
/// surface, so they fail closed with a structured `provider_unsupported`
/// error before any backend call.
fn ensure_github(ctx: &ProviderContext) -> Result<(), ForgeError> {
    match ctx.provider {
        Provider::GitHub => Ok(()),
        Provider::GitLab | Provider::Local => Err(ForgeError::provider_unsupported(
            schema_err(),
            format!(
                "pr review-threads resolve is GitHub-only in v1 (provider: {provider})",
                provider = ctx.provider.as_str(),
            ),
            None,
        )),
    }
}

pub(crate) fn build_resolve_call(ctx: &ProviderContext, thread_id: &str) -> BackendCall {
    debug_assert!(matches!(ctx.provider, Provider::GitHub));
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    ctx.push_github_api_hostname(&mut argv);
    argv.extend([
        OsString::from("-f"),
        OsString::from(format!("query={GITHUB_RESOLVE_MUTATION}")),
        OsString::from("-f"),
        OsString::from(format!("tid={thread_id}")),
    ]);
    BackendCall::new(BackendProgram::Gh, argv)
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

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrReviewThreadResolvePayload) {
    let replied = if payload.replied {
        " (replied first)"
    } else {
        ""
    };
    println!(
        "resolved {provider} review thread {thread}{replied}",
        provider = payload.provider,
        thread = payload.thread_id,
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
            host: None,
            repo: Some("acme/widgets".into()),
            store_root: None,
            dry_run,
        }
    }

    fn args(
        thread: &str,
        note: Option<&str>,
        note_file: Option<&str>,
    ) -> PrReviewThreadResolveArgs {
        PrReviewThreadResolveArgs {
            id: 7,
            thread: thread.to_string(),
            note: note.map(str::to_string),
            note_file: note_file.map(str::to_string),
        }
    }

    #[test]
    fn build_resolve_call_uses_resolve_review_thread_mutation_with_tid() {
        let call = build_resolve_call(&ctx(Provider::GitHub), "PRRT_abc");
        let argv = call.plan_argv();
        assert_eq!(call.program, BackendProgram::Gh);
        assert!(argv.iter().any(|s| s == "graphql"));
        assert!(argv.iter().any(|s| s.contains("resolveReviewThread")));
        assert!(argv.iter().any(|s| s.contains("threadId")));
        assert!(argv.iter().any(|s| s == "tid=PRRT_abc"));
    }

    #[test]
    fn build_resolve_call_adds_hostname_for_enterprise_host() {
        let mut ctx = ctx(Provider::GitHub);
        ctx.host = "internal.ghe.com".into();
        let argv = build_resolve_call(&ctx, "PRRT_abc").plan_argv();
        let pos = argv
            .iter()
            .position(|s| s == "--hostname")
            .expect("enterprise host must be passed to gh api");
        assert_eq!(argv[pos + 1], "internal.ghe.com");
    }

    #[test]
    fn build_reply_call_uses_add_reply_mutation_with_tid_and_body() {
        let call = build_reply_call(&ctx(Provider::GitHub), "PRRT_abc", "ack");
        let argv = call.plan_argv();
        assert_eq!(call.program, BackendProgram::Gh);
        assert!(
            argv.iter()
                .any(|s| s.contains("addPullRequestReviewThreadReply"))
        );
        assert!(argv.iter().any(|s| s.contains("pullRequestReviewThreadId")));
        assert!(argv.iter().any(|s| s == "tid=PRRT_abc"));
        assert!(argv.iter().any(|s| s == "body=ack"));
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
                    r#"{{"id":"{id}","isResolved":false,"isOutdated":false,"path":"src/lib.rs","diffSide":"RIGHT","line":10,"originalLine":10,"originalStartLine":null,"startDiffSide":null,"startLine":null,"subjectType":"LINE","comments":{{"nodes":[{{"id":"PRRC_1","author":{{"login":"reviewer"}},"body":"finding","createdAt":"t","url":"https://github.com/acme/widgets/pull/7#discussion_r1"}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}"#
                )
            })
            .collect();
        BackendSuccess {
            stdout: format!(
                r#"{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-7","reviewThreads":{{"nodes":[{}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}"#,
                nodes.join(",")
            ),
            stderr: String::new(),
        }
    }

    #[test]
    fn run_with_resolve_only_runs_single_mutation() {
        let runner = ScriptedRunner::new(vec![
            pr_view_json(7),
            github_threads_json(&["PRRT_abc"]),
            BackendSuccess {
                stdout: r#"{"data":{"resolveReviewThread":{"thread":{"isResolved":true}}}}"#.into(),
                stderr: String::new(),
            },
        ]);
        let code = run_with(
            &runner,
            &global(ProviderFlag::Github, false),
            args("PRRT_abc", None, None),
            OutputFormat::Json,
            |_| Some("git@github.com:acme/widgets.git".into()),
        )
        .expect("resolve only");
        assert_eq!(code, exit::SUCCESS);
        let calls = runner.calls();
        assert_eq!(
            calls.len(),
            3,
            "view + thread validation run before the resolve mutation"
        );
        assert!(calls[2].1.iter().any(|s| s.contains("resolveReviewThread")));
    }

    #[test]
    fn run_with_resolve_rejects_thread_from_another_pr_before_mutating() {
        let runner =
            ScriptedRunner::new(vec![pr_view_json(7), github_threads_json(&["PRRT_other"])]);
        let err = run_with(
            &runner,
            &global(ProviderFlag::Github, false),
            args("PRRT_target", None, None),
            OutputFormat::Json,
            |_| Some("git@github.com:acme/widgets.git".into()),
        )
        .expect_err("thread does not belong to PR");
        assert_eq!(err.kind(), "review_thread_pr_mismatch");
        let calls = runner.calls();
        assert_eq!(calls.len(), 2, "must only view PR and list its threads");
        assert!(
            !calls
                .iter()
                .any(|(_, argv)| argv.iter().any(|s| s.contains("resolveReviewThread"))),
            "resolve mutation must not run when the thread is not on the PR"
        );
    }

    #[test]
    fn run_with_note_replies_before_resolving() {
        let runner = ScriptedRunner::new(vec![
            pr_view_json(7),
            github_threads_json(&["PRRT_abc"]),
            BackendSuccess {
                stdout: r#"{"data":{"addPullRequestReviewThreadReply":{"comment":{"url":"u"}}}}"#
                    .into(),
                stderr: String::new(),
            },
            BackendSuccess {
                stdout: r#"{"data":{"resolveReviewThread":{"thread":{"isResolved":true}}}}"#.into(),
                stderr: String::new(),
            },
        ]);
        let code = run_with(
            &runner,
            &global(ProviderFlag::Github, false),
            args("PRRT_abc", Some("done, accepted"), None),
            OutputFormat::Json,
            |_| Some("git@github.com:acme/widgets.git".into()),
        )
        .expect("reply then resolve");
        assert_eq!(code, exit::SUCCESS);
        let calls = runner.calls();
        assert_eq!(
            calls.len(),
            4,
            "view + thread validation run before reply then resolve"
        );
        assert!(
            calls[2]
                .1
                .iter()
                .any(|s| s.contains("addPullRequestReviewThreadReply"))
        );
        assert!(calls[3].1.iter().any(|s| s.contains("resolveReviewThread")));
    }

    #[test]
    fn run_with_dry_run_plans_nothing() {
        let runner = ScriptedRunner::new(vec![]);
        let code = run_with(
            &runner,
            &global(ProviderFlag::Github, true),
            args("PRRT_abc", Some("note"), None),
            OutputFormat::Json,
            |_| Some("git@github.com:acme/widgets.git".into()),
        )
        .expect("dry-run");
        assert_eq!(code, exit::SUCCESS);
        assert!(
            runner.calls().is_empty(),
            "dry-run must not invoke the backend"
        );
    }

    #[test]
    fn run_with_gitlab_is_provider_unsupported() {
        let runner = ScriptedRunner::new(vec![]);
        let err = run_with(
            &runner,
            &global(ProviderFlag::Gitlab, false),
            args("d_1", None, None),
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
            args("x", None, None),
            OutputFormat::Json,
            |_| None,
        )
        .expect_err("local unsupported");
        assert_eq!(err.kind(), "provider_unsupported");
        assert!(runner.calls().is_empty());
    }
}
