//! `pr review-threads` atom — read-side review-thread state for a PR / MR.
//!
//! Spec / ops: `cli.forge-cli.pr.review-threads.v1`. Returns code-review
//! threads with their resolved state: the `reviewThreads` GraphQL connection
//! on GitHub (REST review comments carry no resolved bit), resolvable
//! discussions on GitLab. Issue-style comments stay on `pr comments`.
//!
//! Also hosts [`ensure_review_threads_resolved`], the `pr merge` lock-down
//! rule (rule 12): merging while unresolved threads exist fails closed with
//! `unresolved_review_threads` unless `--allow-unresolved-threads` is passed.
//! Bot reviewers post threads asynchronously after PR creation, so the gate
//! runs at merge time — the last action — rather than at creation.
//!
//! The local provider has no review-thread model: the atom returns an empty
//! thread list and the merge gate passes trivially.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{
    BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload, ProcessRunner,
};
use crate::cli::{BINARY, GlobalFlags, PrReviewThreadsArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_comments::{
    github_repo_slug_from_url, gitlab_host_from_url, gitlab_project_path_from_url,
    split_concatenated_arrays,
};
use crate::ops::pr_view;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};

const SCHEMA: &str = "pr.review-threads";
const SCHEMA_VERSION: u32 = 1;

/// GitHub exposes thread resolution only through GraphQL. `first: 100` is far
/// beyond practical thread counts for a single PR; the merge gate fails
/// closed on what one page returns.
const GITHUB_THREADS_QUERY: &str = "query($owner: String!, $name: String!, $pr: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $pr) { reviewThreads(first: 100) { nodes { isResolved isOutdated path comments(first: 1) { nodes { author { login } body createdAt url } } } } } } }";

/// One review thread, normalized across providers.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrReviewThreadSummary {
    pub resolved: bool,
    /// GitHub `isOutdated` (the anchored diff hunk changed); always false on
    /// GitLab. Outdated-but-unresolved threads still count as unresolved.
    pub outdated: bool,
    pub author: String,
    /// File the thread is anchored to; empty for non-inline threads.
    pub path: String,
    pub created_at: String,
    pub url: String,
    /// First comment of the thread.
    pub body: String,
}

/// Envelope payload for `cli.forge-cli.pr.review-threads.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrReviewThreadsPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub total: usize,
    pub unresolved: usize,
    pub threads: Vec<PrReviewThreadSummary>,
}

pub fn run(
    global: &GlobalFlags,
    args: PrReviewThreadsArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    if global.is_local() {
        let runner = crate::local::LocalRunner::from_global(global)?;
        return run_with(&runner, global, args, format, git_remote_url);
    }
    let runner = ProcessRunner;
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrReviewThreadsArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;

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
                total: 0,
                unresolved: 0,
                threads: Vec::new(),
            },
            format,
            render_text,
        ));
    }

    let threads_call = build_threads_call(&ctx, &view.url, view.number)?;

    if global.dry_run {
        let payload = DryRunPayload::new(ctx.provider, &threads_call);
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            payload,
            format,
            |p| println!("would run: {plan}", plan = p.plan.join(" ")),
        ));
    }

    let threads_output = runner.run(&threads_call)?;
    let threads = parse_threads(&ctx, &threads_output, &view.url)?;
    let unresolved = threads.iter().filter(|t| !t.resolved).count();

    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        PrReviewThreadsPayload {
            provider: ctx.provider.as_str(),
            number: view.number,
            url: view.url,
            total: threads.len(),
            unresolved,
            threads,
        },
        format,
        render_text,
    ))
}

/// `pr merge` lock-down rule 12. Fetches review threads for the PR/MR and
/// fails closed with `unresolved_review_threads` (DATA 65) when any thread is
/// unresolved. The local provider passes trivially (no thread model). Callers
/// bypass via `--allow-unresolved-threads`, which skips this call entirely.
pub fn ensure_review_threads_resolved<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    pr_url: &str,
    number: u64,
) -> Result<(), ForgeError> {
    if matches!(ctx.provider, Provider::Local) {
        return Ok(());
    }
    let call = build_threads_call(ctx, pr_url, number)?;
    let output = runner.run(&call)?;
    let threads = parse_threads(ctx, &output, pr_url)?;
    let unresolved: Vec<&PrReviewThreadSummary> = threads.iter().filter(|t| !t.resolved).collect();
    if unresolved.is_empty() {
        return Ok(());
    }
    let listing = unresolved
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
            n = unresolved.len(),
        ),
        Some(listing),
    ))
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
            Ok(BackendCall::new(
                BackendProgram::Gh,
                [
                    OsString::from("api"),
                    OsString::from("graphql"),
                    OsString::from("-f"),
                    OsString::from(format!("query={GITHUB_THREADS_QUERY}")),
                    OsString::from("-f"),
                    OsString::from(format!("owner={owner}")),
                    OsString::from("-f"),
                    OsString::from(format!("name={name}")),
                    OsString::from("-F"),
                    OsString::from(format!("pr={number}")),
                ],
            ))
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

fn parse_github_threads(output: &BackendSuccess) -> Result<Vec<PrReviewThreadSummary>, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "review-threads response is invalid JSON",
            Some(e.to_string()),
        )
    })?;
    let nodes = value
        .pointer("/data/repository/pullRequest/reviewThreads/nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                "review-threads response is missing reviewThreads nodes",
                Some(format!("stdout={:?}", output.stdout)),
            )
        })?;
    let mut out = Vec::new();
    for node in nodes {
        let first = node
            .pointer("/comments/nodes/0")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        out.push(PrReviewThreadSummary {
            resolved: node
                .get("isResolved")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            outdated: node
                .get("isOutdated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            author: first
                .pointer("/author/login")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            path: node
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            created_at: first
                .get("createdAt")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            url: first
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            body: first
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    Ok(out)
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
            out.push(PrReviewThreadSummary {
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
            });
        }
    }
    Ok(out)
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrReviewThreadsPayload) {
    println!(
        "{provider} #{number} ({total} review threads, {unresolved} unresolved)\n  {url}",
        provider = payload.provider,
        number = payload.number,
        total = payload.total,
        unresolved = payload.unresolved,
        url = payload.url,
    );
    for thread in &payload.threads {
        let body_first_line = thread.body.lines().next().unwrap_or("");
        let state = if thread.resolved {
            "resolved"
        } else {
            "UNRESOLVED"
        };
        let anchor = if thread.path.is_empty() {
            String::new()
        } else {
            format!(" @ {path}", path = thread.path)
        };
        println!(
            "  - [{state}] {author}{anchor}: {body}",
            author = thread.author,
            body = body_first_line,
        );
    }
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
            .map(|(resolved, author, path)| {
                format!(
                    r#"{{"isResolved":{resolved},"isOutdated":false,"path":"{path}","comments":{{"nodes":[{{"author":{{"login":"{author}"}},"body":"finding body\nsecond line","createdAt":"2026-06-11T04:49:36Z","url":"https://github.com/acme/widgets/pull/7#discussion_r1"}}]}}}}"#
                )
            })
            .collect();
        format!(
            r#"{{"data":{{"repository":{{"pullRequest":{{"reviewThreads":{{"nodes":[{nodes}]}}}}}}}}}}"#,
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
    fn parse_github_threads_errors_on_missing_nodes() {
        let output = BackendSuccess {
            stdout: r#"{"data":{"repository":{"pullRequest":null}}}"#.into(),
            stderr: String::new(),
        };
        let err = parse_github_threads(&output).expect_err("must fail");
        assert_eq!(err.kind(), "software_error");
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
        assert_eq!(
            threads[0].url,
            "https://gitlab.example.com/group/project/-/merge_requests/9#note_21"
        );
        assert!(threads[1].resolved);
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

    fn json_global() -> GlobalFlags {
        GlobalFlags {
            format: Some(OutputFormat::Json),
            remote: "origin".into(),
            provider: Some(crate::cli::ProviderFlag::Github),
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
        let args = PrReviewThreadsArgs { id: 7 };
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
    fn run_with_dry_run_plans_threads_call_after_view() {
        let view = BackendSuccess {
            stdout: r#"{"number":7,"url":"https://github.com/acme/widgets/pull/7","state":"OPEN","isDraft":false,"title":"demo","headRefName":"feat/x","baseRefName":"main","mergeable":"MERGEABLE","mergedAt":null,"labels":[]}"#.into(),
            stderr: String::new(),
        };
        let runner = ScriptedRunner::new(vec![view]);
        let mut global = json_global();
        global.dry_run = true;
        let args = PrReviewThreadsArgs { id: 7 };
        let code = run_with(&runner, &global, args, OutputFormat::Json, |_| {
            Some("git@github.com:acme/widgets.git".into())
        })
        .expect("run");
        assert_eq!(code, 0);
        // Only the view call ran; the threads call stayed a plan.
        assert_eq!(runner.calls().len(), 1);
    }
}
