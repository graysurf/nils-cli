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

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess};
use crate::cli::{
    BINARY, GlobalFlags, SearchCommand, SearchMatchField, SearchQueryArgs, SearchRefsToArgs,
};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;

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
    let runner = default_runner();
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
        SearchCommand::RefsTo(args) => {
            run_github_refs_to(runner, global, ctx, repo_slug, args, format)
        }
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

const REFS_TO_SCHEMA: &str = "search.refs-to";

/// Cross-reference events that reference the target issue / PR. The inline
/// fragments cover the `issueOrPullRequest` union and the `source` of each
/// `CrossReferencedEvent` (itself an Issue or PullRequest).
const REFS_TO_GRAPHQL_QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!, $first: Int!) {
  repository(owner: $owner, name: $name) {
    issueOrPullRequest(number: $number) {
      __typename
      ... on Issue {
        timelineItems(itemTypes: [CROSS_REFERENCED_EVENT], first: $first) {
          nodes { ...xref }
        }
      }
      ... on PullRequest {
        timelineItems(itemTypes: [CROSS_REFERENCED_EVENT], first: $first) {
          nodes { ...xref }
        }
      }
    }
  }
}
fragment xref on CrossReferencedEvent {
  source {
    __typename
    ... on Issue { number url title state repository { nameWithOwner } }
    ... on PullRequest { number url title state repository { nameWithOwner } }
  }
}
"#;

/// Normalized payload for `cli.forge-cli.search.refs-to.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RefsToPayload {
    pub provider: &'static str,
    pub host: String,
    pub repo: String,
    pub reference_number: u64,
    pub limit: u32,
    pub item_count: usize,
    pub limited: bool,
    pub items: Vec<SearchItem>,
}

/// Parsed `<ref>` target: the repository owner / name and issue-or-PR number.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RefTarget {
    owner: String,
    name: String,
    number: u64,
}

fn run_github_refs_to<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    default_slug: &str,
    args: SearchRefsToArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let limit = limit_for_provider(args.limit);
    let target = parse_ref(&args.reference, Some(default_slug))?;
    let call = build_github_refs_to_call(ctx, &target, limit);
    if global.dry_run {
        return Ok(emit_search_dry_run(
            schema_ok(REFS_TO_SCHEMA),
            ctx,
            call,
            format,
        ));
    }
    let output = runner.run(&call)?;
    let payload = parse_refs_to_output(&output, &target, limit, ctx)?;
    Ok(emit_success(
        schema_ok(REFS_TO_SCHEMA),
        payload,
        format,
        render_refs_to_text,
    ))
}

fn build_github_refs_to_call(ctx: &ProviderContext, target: &RefTarget, limit: u32) -> BackendCall {
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    push_github_hostname(ctx, &mut argv);
    argv.extend([
        OsString::from("-F"),
        OsString::from(format!("owner={}", target.owner)),
        OsString::from("-F"),
        OsString::from(format!("name={}", target.name)),
        OsString::from("-F"),
        OsString::from(format!("number={}", target.number)),
        OsString::from("-F"),
        OsString::from(format!("first={limit}")),
        OsString::from("-f"),
        OsString::from(format!("query={REFS_TO_GRAPHQL_QUERY}")),
    ]);
    BackendCall::new(BackendProgram::Gh, argv)
}

fn push_github_hostname(ctx: &ProviderContext, argv: &mut Vec<OsString>) {
    if ctx.host != "github.com" {
        argv.push(OsString::from("--hostname"));
        argv.push(OsString::from(&ctx.host));
    }
}

/// Parse a `<ref>` into its `(owner, name, number)` target. Accepts a GitHub
/// URL (`https://github.com/owner/name/issues|pull/<n>`), `owner/name#<n>`, or
/// `#<n>` / `<n>` (which fall back to `default_slug`).
fn parse_ref(reference: &str, default_slug: Option<&str>) -> Result<RefTarget, ForgeError> {
    let raw = reference.trim();
    if let Some(rest) = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))
    {
        // host/owner/name/(issues|pull)/<number>[/...]
        let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() >= 5 && matches!(segments[3], "issues" | "pull") {
            let number = parse_ref_number(segments[4])?;
            return Ok(RefTarget {
                owner: segments[1].to_string(),
                name: segments[2].to_string(),
                number,
            });
        }
        return Err(ref_invalid(raw));
    }
    if let Some((repo_part, number_part)) = raw.split_once('#') {
        let number = parse_ref_number(number_part)?;
        if repo_part.is_empty() {
            let (owner, name) = split_slug_or_default(default_slug, raw)?;
            return Ok(RefTarget {
                owner,
                name,
                number,
            });
        }
        let (owner, name) = split_slug(repo_part).ok_or_else(|| ref_invalid(raw))?;
        return Ok(RefTarget {
            owner,
            name,
            number,
        });
    }
    if raw.chars().all(|c| c.is_ascii_digit()) && !raw.is_empty() {
        let number = parse_ref_number(raw)?;
        let (owner, name) = split_slug_or_default(default_slug, raw)?;
        return Ok(RefTarget {
            owner,
            name,
            number,
        });
    }
    Err(ref_invalid(raw))
}

fn parse_ref_number(value: &str) -> Result<u64, ForgeError> {
    value
        .trim()
        .parse::<u64>()
        .map_err(|_| ref_invalid(value))
        .and_then(|n| {
            if n == 0 {
                Err(ref_invalid(value))
            } else {
                Ok(n)
            }
        })
}

fn split_slug(slug: &str) -> Option<(String, String)> {
    let (owner, name) = slug.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some((owner.to_string(), name.to_string()))
}

fn split_slug_or_default(
    default_slug: Option<&str>,
    raw: &str,
) -> Result<(String, String), ForgeError> {
    default_slug
        .and_then(split_slug)
        .ok_or_else(|| ref_invalid(raw))
}

fn parse_refs_to_output(
    output: &BackendSuccess,
    target: &RefTarget,
    limit: u32,
    ctx: &ProviderContext,
) -> Result<RefsToPayload, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "GitHub refs-to GraphQL JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    let subject = value
        .pointer("/data/repository/issueOrPullRequest")
        .filter(|v| !v.is_null())
        .ok_or_else(|| {
            ForgeError::software(
                schema_err(),
                format!(
                    "ref {}/{}#{} not found or not visible",
                    target.owner, target.name, target.number
                ),
                None,
            )
        })?;
    let nodes = subject
        .pointer("/timelineItems/nodes")
        .and_then(|v| v.as_array())
        .ok_or_else(|| missing("timelineItems.nodes"))?;
    let items = nodes
        .iter()
        .filter_map(|node| node.get("source").filter(|s| !s.is_null()))
        .map(parse_refs_to_item)
        .collect::<Result<Vec<_>, _>>()?;
    let item_count = items.len();
    Ok(RefsToPayload {
        provider: ctx.provider.as_str(),
        host: ctx.host.clone(),
        repo: format!("{}/{}", target.owner, target.name),
        reference_number: target.number,
        limit,
        item_count,
        limited: reached_limit(item_count, limit),
        items,
    })
}

fn parse_refs_to_item(source: &serde_json::Value) -> Result<SearchItem, ForgeError> {
    let kind = match source.get("__typename").and_then(|v| v.as_str()) {
        Some("PullRequest") => "pr",
        Some("Issue") => "issue",
        _ => "issue",
    }
    .to_string();
    Ok(SearchItem {
        kind,
        number: source
            .get("number")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| missing("source.number"))?,
        url: required_str(source, "url")?,
        title: required_str(source, "title")?,
        state: required_str(source, "state")?.to_ascii_lowercase(),
        repo: source
            .pointer("/repository/nameWithOwner")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| missing("source.repository.nameWithOwner"))?,
        matched_field: None,
    })
}

fn render_refs_to_text(payload: &RefsToPayload) {
    println!(
        "{provider}@{host} search refs-to {repo}#{number}: {count} reference(s){limited}",
        provider = payload.provider,
        host = payload.host,
        repo = payload.repo,
        number = payload.reference_number,
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

fn ref_invalid(value: &str) -> ForgeError {
    ForgeError::validation(
        schema_err(),
        "ref_invalid",
        format!(
            "could not parse ref '{value}': expected a GitHub URL, owner/name#number, or #number"
        ),
        None,
    )
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
        SearchCommand::RefsTo(_) => "refs-to",
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

    #[test]
    fn parse_ref_handles_url_issue_pull_slug_and_bare_number() {
        let want = RefTarget {
            owner: "acme".into(),
            name: "widget".into(),
            number: 42,
        };
        assert_eq!(
            parse_ref("https://github.com/acme/widget/issues/42", None).unwrap(),
            want
        );
        assert_eq!(
            parse_ref("https://github.com/acme/widget/pull/42", None).unwrap(),
            want
        );
        assert_eq!(parse_ref("acme/widget#42", None).unwrap(), want);
        assert_eq!(parse_ref("#42", Some("acme/widget")).unwrap(), want);
        assert_eq!(parse_ref("42", Some("acme/widget")).unwrap(), want);
    }

    #[test]
    fn parse_ref_rejects_bad_refs_and_missing_context() {
        assert_eq!(
            parse_ref("#42", None).expect_err("no default slug").kind(),
            "ref_invalid"
        );
        assert_eq!(
            parse_ref("not-a-ref", Some("acme/widget"))
                .expect_err("garbage")
                .kind(),
            "ref_invalid"
        );
        assert_eq!(
            parse_ref("acme/widget#0", None)
                .expect_err("zero number")
                .kind(),
            "ref_invalid"
        );
    }

    #[test]
    fn refs_to_call_builds_graphql_argv() {
        let target = RefTarget {
            owner: "acme".into(),
            name: "widget".into(),
            number: 7,
        };
        let call = build_github_refs_to_call(&ctx(), &target, 30);
        assert_eq!(call.program, BackendProgram::Gh);
        let argv = argv_to_strings(&call.argv);
        assert_eq!(argv[0], "api");
        assert_eq!(argv[1], "graphql");
        assert!(argv.iter().any(|a| a == "owner=acme"));
        assert!(argv.iter().any(|a| a == "name=widget"));
        assert!(argv.iter().any(|a| a == "number=7"));
        assert!(argv.iter().any(|a| a == "first=30"));
        assert!(argv.iter().any(|a| a.starts_with("query=")));
        // No --hostname for github.com.
        assert!(!argv.iter().any(|a| a == "--hostname"));
    }

    #[test]
    fn refs_to_call_adds_hostname_for_enterprise_host() {
        let enterprise = ProviderContext {
            host: "internal.ghe.com".into(),
            ..ctx()
        };
        let target = RefTarget {
            owner: "acme".into(),
            name: "widget".into(),
            number: 7,
        };
        let argv = argv_to_strings(&build_github_refs_to_call(&enterprise, &target, 30).argv);
        assert!(argv.iter().any(|a| a == "--hostname"));
        assert!(argv.iter().any(|a| a == "internal.ghe.com"));
    }

    #[test]
    fn parse_refs_to_output_normalizes_cross_referencing_sources() {
        let output = BackendSuccess {
            stdout: r#"{"data":{"repository":{"issueOrPullRequest":{"__typename":"Issue","timelineItems":{"nodes":[
                {"source":{"__typename":"PullRequest","number":9,"url":"https://github.com/acme/widget/pull/9","title":"close the issue","state":"MERGED","repository":{"nameWithOwner":"acme/widget"}}},
                {"source":{"__typename":"Issue","number":11,"url":"https://github.com/acme/widget/issues/11","title":"a related issue","state":"OPEN","repository":{"nameWithOwner":"acme/widget"}}}
            ]}}}}}"#
            .into(),
            stderr: String::new(),
        };
        let target = RefTarget {
            owner: "acme".into(),
            name: "widget".into(),
            number: 7,
        };
        let payload = parse_refs_to_output(&output, &target, 30, &ctx()).expect("parse");
        assert_eq!(payload.repo, "acme/widget");
        assert_eq!(payload.reference_number, 7);
        assert_eq!(payload.item_count, 2);
        assert_eq!(payload.items[0].kind, "pr");
        assert_eq!(payload.items[0].number, 9);
        assert_eq!(payload.items[0].state, "merged");
        assert_eq!(payload.items[1].kind, "issue");
        assert_eq!(payload.items[1].state, "open");
    }

    #[test]
    fn parse_refs_to_output_errors_when_ref_missing() {
        let output = BackendSuccess {
            stdout: r#"{"data":{"repository":{"issueOrPullRequest":null}}}"#.into(),
            stderr: String::new(),
        };
        let target = RefTarget {
            owner: "acme".into(),
            name: "widget".into(),
            number: 999,
        };
        let err = parse_refs_to_output(&output, &target, 30, &ctx()).expect_err("missing ref");
        assert_eq!(err.kind(), "software_error");
    }

    fn refs_to(reference: &str) -> SearchCommand {
        SearchCommand::RefsTo(SearchRefsToArgs {
            reference: reference.into(),
            limit: 30,
        })
    }

    #[test]
    fn refs_to_local_branch_is_explicitly_unsupported() {
        let err = run_with(
            &FailingRunner,
            &global(ProviderFlag::Local),
            refs_to("#7"),
            OutputFormat::Json,
            |_| None,
        )
        .expect_err("local unsupported");
        assert_eq!(err.kind(), "provider_unsupported");
    }
}
