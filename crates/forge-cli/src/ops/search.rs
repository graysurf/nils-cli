//! `search` command group.
//!
//! Full-text query over live forge issues / PRs. Unlike `issue list` / `pr
//! list` (structured-field filters scoped to the current repo) and `inbox`
//! (the personal work queue), `search` delegates to the provider's free-text
//! search primitive (`gh search issues|prs`). The group is provider-shaped
//! from the start: GitHub is the v1 implementation target, while GitLab and
//! Local return explicit `provider_unsupported` errors until their backend
//! mappings exist — never a silent empty result.
//!
//! `search` is single-repo scoped: the repo slug comes from `--repo
//! owner/name` when present, otherwise it is derived from the detected forge
//! remote. The slug is always pushed as `--repo <slug>` so `gh search`
//! (global by default) stays bounded to one repository.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess, ProcessRunner};
use crate::cli::{BINARY, GlobalFlags, SearchCommand, SearchMatchField, SearchQueryArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};

const SCHEMA_VERSION: u32 = 1;
/// Upper bound `gh search` accepts for `--limit`.
const GITHUB_SEARCH_LIMIT: u32 = 1000;
/// `--json` field set requested from `gh search issues|prs`. `repository`
/// gives per-item fidelity; `isPullRequest` lets one parser tag `kind`.
const SEARCH_JSON_FIELDS: &str = "number,title,url,state,repository,isPullRequest";

/// Which `gh search` noun a request maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchKind {
    Issues,
    Prs,
}

impl SearchKind {
    /// The `gh search <noun>` subcommand.
    fn gh_noun(self) -> &'static str {
        match self {
            SearchKind::Issues => "issues",
            SearchKind::Prs => "prs",
        }
    }

    /// Schema-version stem (`search.issues` / `search.prs`).
    fn schema(self) -> &'static str {
        match self {
            SearchKind::Issues => "search.issues",
            SearchKind::Prs => "search.prs",
        }
    }

    /// `kind` to record when the provider does not report `isPullRequest`.
    fn default_item_kind(self) -> &'static str {
        match self {
            SearchKind::Issues => "issue",
            SearchKind::Prs => "pr",
        }
    }
}

/// Normalized payload for `cli.forge-cli.search.issues.v1` /
/// `cli.forge-cli.search.prs.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchPayload {
    pub provider: &'static str,
    pub host: String,
    pub repo: String,
    pub query: String,
    pub match_fields: Vec<String>,
    pub limit: u32,
    pub item_count: usize,
    pub limited: bool,
    pub items: Vec<SearchItem>,
}

/// One normalized search hit. Shared across all three `search` envelopes.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchItem {
    /// `"issue"` or `"pr"`.
    pub kind: String,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub state: String,
    pub repo: String,
    /// Field the hit matched on. Best-effort: `None` when the provider does
    /// not report which field matched (the `gh search` path never does).
    pub matched_field: Option<String>,
}

pub fn run(
    global: &GlobalFlags,
    command: SearchCommand,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    run_with(&runner, global, command, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    command: SearchCommand,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        &remote_url_lookup,
    )?;
    match ctx.provider {
        Provider::GitHub => {
            let repo_slug = resolve_repo_slug(&ctx, &global.remote, &remote_url_lookup)?;
            run_github(runner, global, &ctx, &repo_slug, command, format)
        }
        Provider::GitLab | Provider::Local => Err(provider_unsupported(&ctx, &command)),
    }
}

fn run_github<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    repo_slug: &str,
    command: SearchCommand,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    match command {
        SearchCommand::Issues(args) => run_github_search(
            runner,
            global,
            ctx,
            repo_slug,
            SearchKind::Issues,
            args,
            format,
        ),
        SearchCommand::Prs(args) => run_github_search(
            runner,
            global,
            ctx,
            repo_slug,
            SearchKind::Prs,
            args,
            format,
        ),
    }
}

fn run_github_search<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    repo_slug: &str,
    kind: SearchKind,
    args: SearchQueryArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let limit = limit_for_provider(args.limit);
    let call = build_github_search_call(kind, repo_slug, &args.query, &args.match_fields, limit);
    if global.dry_run {
        return Ok(emit_search_dry_run(
            schema_ok(kind.schema()),
            ctx,
            call,
            format,
        ));
    }
    let output = runner.run(&call)?;
    let payload = parse_search_output(
        &output,
        kind,
        args.query,
        &args.match_fields,
        repo_slug,
        limit,
        ctx,
    )?;
    let label = kind.gh_noun();
    Ok(emit_success(
        schema_ok(kind.schema()),
        payload,
        format,
        move |payload| render_search_text(label, payload),
    ))
}

fn build_github_search_call(
    kind: SearchKind,
    repo_slug: &str,
    query: &str,
    match_fields: &[SearchMatchField],
    limit: u32,
) -> BackendCall {
    let argv = vec![
        OsString::from("search"),
        OsString::from(kind.gh_noun()),
        OsString::from(query),
        OsString::from("--repo"),
        OsString::from(repo_slug),
        OsString::from("--match"),
        OsString::from(match_csv(match_fields)),
        OsString::from("--limit"),
        OsString::from(limit.to_string()),
        OsString::from("--json"),
        OsString::from(SEARCH_JSON_FIELDS),
    ];
    BackendCall::new(BackendProgram::Gh, argv)
}

fn match_csv(fields: &[SearchMatchField]) -> String {
    fields
        .iter()
        .map(|field| field.as_str())
        .collect::<Vec<_>>()
        .join(",")
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
        "search is single-repo scoped: pass --repo owner/name or run inside a repo with a recognised forge remote",
        None,
    ))
}

fn limit_for_provider(limit: u32) -> u32 {
    limit.clamp(1, GITHUB_SEARCH_LIMIT)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct SearchDryRunPayload {
    provider: &'static str,
    host: String,
    plan: Vec<String>,
}

fn emit_search_dry_run(
    schema_version: String,
    ctx: &ProviderContext,
    call: BackendCall,
    format: OutputFormat,
) -> i32 {
    let payload = SearchDryRunPayload {
        provider: ctx.provider.as_str(),
        host: ctx.host.clone(),
        plan: call.plan_argv(),
    };
    emit_success(schema_version, payload, format, |payload| {
        println!("would run: {}", payload.plan.join(" "));
    })
}

fn parse_search_output(
    output: &BackendSuccess,
    kind: SearchKind,
    query: String,
    match_fields: &[SearchMatchField],
    repo_slug: &str,
    limit: u32,
    ctx: &ProviderContext,
) -> Result<SearchPayload, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "GitHub search JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    let arr = value.as_array().ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "GitHub search JSON is not an array",
            Some(format!("got: {value}")),
        )
    })?;
    let items = arr
        .iter()
        .map(|raw| parse_search_item(raw, kind, repo_slug))
        .collect::<Result<Vec<_>, _>>()?;
    let item_count = items.len();
    Ok(SearchPayload {
        provider: ctx.provider.as_str(),
        host: ctx.host.clone(),
        repo: repo_slug.to_string(),
        query,
        match_fields: match_fields
            .iter()
            .map(|field| field.as_str().to_string())
            .collect(),
        limit,
        item_count,
        limited: reached_limit(item_count, limit),
        items,
    })
}

fn parse_search_item(
    raw: &serde_json::Value,
    kind: SearchKind,
    repo_slug: &str,
) -> Result<SearchItem, ForgeError> {
    let item_kind = match raw.get("isPullRequest").and_then(|v| v.as_bool()) {
        Some(true) => "pr",
        Some(false) => "issue",
        None => kind.default_item_kind(),
    }
    .to_string();
    Ok(SearchItem {
        kind: item_kind,
        number: raw
            .get("number")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| missing("number"))?,
        url: required_str(raw, "url")?,
        title: required_str(raw, "title")?,
        state: required_str(raw, "state")?,
        repo: raw
            .get("repository")
            .and_then(|repo| {
                repo.get("nameWithOwner")
                    .or_else(|| repo.get("full_name"))
                    .or_else(|| repo.get("name"))
            })
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| repo_slug.to_string()),
        matched_field: None,
    })
}

fn reached_limit(count: usize, limit: u32) -> bool {
    u32::try_from(count).is_ok_and(|count| count >= limit)
}

fn required_str(raw: &serde_json::Value, key: &str) -> Result<String, ForgeError> {
    raw.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing(key))
}

fn missing(key: &str) -> ForgeError {
    ForgeError::software(
        schema_err(),
        format!("missing required field '{key}' in GitHub search output"),
        None,
    )
}

fn render_search_text(label: &str, payload: &SearchPayload) {
    println!(
        "{provider}@{host} search {label} {repo} {query:?}: {count} result(s){limited}",
        provider = payload.provider,
        host = payload.host,
        repo = payload.repo,
        query = payload.query,
        count = payload.item_count,
        limited = limited_suffix(payload.limited),
    );
    for item in &payload.items {
        println!(
            "{kind} #{number} [{state}] {repo} {title} - {url}",
            kind = item.kind,
            number = item.number,
            state = item.state,
            repo = item.repo,
            title = one_line(&item.title),
            url = item.url,
        );
    }
}

fn limited_suffix(limited: bool) -> &'static str {
    if limited { " (limited)" } else { "" }
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn command_name(command: &SearchCommand) -> &'static str {
    match command {
        SearchCommand::Issues(_) => "issues",
        SearchCommand::Prs(_) => "prs",
    }
}

fn provider_unsupported(ctx: &ProviderContext, command: &SearchCommand) -> ForgeError {
    ForgeError::provider_unsupported(
        schema_err(),
        format!("search {} is GitHub-only in v1", command_name(command)),
        Some(format!("provider={}", ctx.provider.as_str())),
    )
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn schema_ok(schema: &str) -> String {
    schema_version_for(BINARY, schema, SCHEMA_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::argv_to_strings;
    use crate::cli::ProviderFlag;
    use crate::provider::DetectionSource;
    use pretty_assertions::assert_eq;

    struct FailingRunner;

    impl BackendRunner for FailingRunner {
        fn run(&self, _call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
            panic!("provider unsupported branches must not run backend")
        }
    }

    fn global(provider: ProviderFlag) -> GlobalFlags {
        GlobalFlags {
            format: Some(OutputFormat::Json),
            remote: "origin".into(),
            provider: Some(provider),
            repo: Some("acme/widget".into()),
            store_root: None,
            dry_run: false,
        }
    }

    fn issues(query: &str) -> SearchCommand {
        SearchCommand::Issues(SearchQueryArgs {
            query: query.into(),
            match_fields: vec![
                SearchMatchField::Title,
                SearchMatchField::Body,
                SearchMatchField::Comments,
            ],
            limit: 30,
        })
    }

    fn prs(query: &str) -> SearchCommand {
        SearchCommand::Prs(SearchQueryArgs {
            query: query.into(),
            match_fields: vec![SearchMatchField::Title],
            limit: 5,
        })
    }

    fn ctx() -> ProviderContext {
        ProviderContext {
            provider: Provider::GitHub,
            host: "github.com".into(),
            source: DetectionSource::Flag,
            repo: Some("acme/widget".into()),
        }
    }

    #[test]
    fn issues_call_builds_gh_search_issues_argv() {
        let call = build_github_search_call(
            SearchKind::Issues,
            "acme/widget",
            "flaky retry",
            &[
                SearchMatchField::Title,
                SearchMatchField::Body,
                SearchMatchField::Comments,
            ],
            30,
        );
        assert_eq!(call.program, BackendProgram::Gh);
        assert_eq!(
            argv_to_strings(&call.argv),
            [
                "search",
                "issues",
                "flaky retry",
                "--repo",
                "acme/widget",
                "--match",
                "title,body,comments",
                "--limit",
                "30",
                "--json",
                "number,title,url,state,repository,isPullRequest",
            ]
        );
    }

    #[test]
    fn prs_call_honours_narrowed_match_fields() {
        let call = build_github_search_call(
            SearchKind::Prs,
            "acme/widget",
            "cache",
            &[SearchMatchField::Title],
            5,
        );
        assert_eq!(
            argv_to_strings(&call.argv),
            [
                "search",
                "prs",
                "cache",
                "--repo",
                "acme/widget",
                "--match",
                "title",
                "--limit",
                "5",
                "--json",
                "number,title,url,state,repository,isPullRequest",
            ]
        );
    }

    #[test]
    fn parse_normalizes_issue_and_pr_items() {
        let output = BackendSuccess {
            stdout: r#"[
                {"number":7,"title":"body-only term here","url":"https://github.com/acme/widget/issues/7","state":"open","isPullRequest":false,"repository":{"nameWithOwner":"acme/widget"}},
                {"number":9,"title":"a pull request","url":"https://github.com/acme/widget/pull/9","state":"closed","isPullRequest":true,"repository":{"nameWithOwner":"acme/widget"}}
            ]"#
            .into(),
            stderr: String::new(),
        };
        let payload = parse_search_output(
            &output,
            SearchKind::Issues,
            "term".into(),
            &[SearchMatchField::Body],
            "acme/widget",
            30,
            &ctx(),
        )
        .expect("parse");
        assert_eq!(payload.item_count, 2);
        assert_eq!(payload.repo, "acme/widget");
        assert_eq!(payload.match_fields, ["body"]);
        assert!(!payload.limited);
        assert_eq!(payload.items[0].kind, "issue");
        assert_eq!(payload.items[0].number, 7);
        assert_eq!(payload.items[1].kind, "pr");
        assert_eq!(payload.items[1].state, "closed");
        assert_eq!(payload.items[0].matched_field, None);
    }

    #[test]
    fn parse_empty_array_is_well_formed_empty_payload() {
        let output = BackendSuccess {
            stdout: "[]".into(),
            stderr: String::new(),
        };
        let payload = parse_search_output(
            &output,
            SearchKind::Prs,
            "term".into(),
            &[SearchMatchField::Title],
            "acme/widget",
            5,
            &ctx(),
        )
        .expect("parse empty");
        assert_eq!(payload.item_count, 0);
        assert!(payload.items.is_empty());
        assert!(!payload.limited);
    }

    #[test]
    fn limit_is_clamped_to_search_ceiling() {
        assert_eq!(limit_for_provider(0), 1);
        assert_eq!(limit_for_provider(50_000), GITHUB_SEARCH_LIMIT);
        assert_eq!(limit_for_provider(30), 30);
    }

    #[test]
    fn resolve_repo_slug_prefers_explicit_repo() {
        let slug = resolve_repo_slug(&ctx(), "origin", &|_| None).expect("explicit repo");
        assert_eq!(slug, "acme/widget");
    }

    #[test]
    fn resolve_repo_slug_falls_back_to_remote_path() {
        let no_repo = ProviderContext {
            repo: None,
            ..ctx()
        };
        let slug = resolve_repo_slug(&no_repo, "origin", &|_| {
            Some("git@github.com:acme/widget.git".to_string())
        })
        .expect("derived repo");
        assert_eq!(slug, "acme/widget");
    }

    #[test]
    fn resolve_repo_slug_errors_without_repo_or_remote() {
        let no_repo = ProviderContext {
            repo: None,
            ..ctx()
        };
        let err = resolve_repo_slug(&no_repo, "origin", &|_| None).expect_err("no repo");
        assert_eq!(err.kind(), "repo_required");
    }

    #[test]
    fn dry_run_emits_search_plan_without_running_backend() {
        let mut global = global(ProviderFlag::Github);
        global.dry_run = true;
        let code = run_with(
            &FailingRunner,
            &global,
            issues("term"),
            OutputFormat::Text,
            |_| None,
        )
        .expect("dry-run should not execute backend");
        assert_eq!(code, 0);
    }

    #[test]
    fn gitlab_branch_is_explicitly_unsupported() {
        let err = run_with(
            &FailingRunner,
            &global(ProviderFlag::Gitlab),
            issues("term"),
            OutputFormat::Json,
            |_| None,
        )
        .expect_err("gitlab unsupported");
        assert_eq!(err.kind(), "provider_unsupported");
    }

    #[test]
    fn local_branch_is_explicitly_unsupported() {
        let err = run_with(
            &FailingRunner,
            &global(ProviderFlag::Local),
            prs("term"),
            OutputFormat::Json,
            |_| None,
        )
        .expect_err("local unsupported");
        assert_eq!(err.kind(), "provider_unsupported");
    }
}
