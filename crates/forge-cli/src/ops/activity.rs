//! `activity` command group.
//!
//! The command group is provider-shaped from the start: GitHub is the v1
//! implementation target, while GitLab and Local return explicit
//! `provider_unsupported` errors until their backend mappings exist.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess, ProcessRunner};
use crate::cli::{
    ActivityCommand, ActivityCommitsArgs, ActivityEventsArgs, ActivitySummaryArgs, BINARY,
    GlobalFlags,
};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};

const COMMITS_SCHEMA: &str = "activity.commits";
const EVENTS_SCHEMA: &str = "activity.events";
const SUMMARY_SCHEMA: &str = "activity.summary";
const SCHEMA_VERSION: u32 = 1;
const GITHUB_PAGE_LIMIT: u32 = 100;

const SUMMARY_GRAPHQL_QUERY: &str = r#"
query($login: String!, $from: DateTime, $maxRepositories: Int!) {
  user(login: $login) {
    login
    contributionsCollection(from: $from) {
      totalCommitContributions
      commitContributionsByRepository(maxRepositories: $maxRepositories) {
        repository {
          nameWithOwner
        }
        contributions(first: 100, orderBy: {field: OCCURRED_AT, direction: DESC}) {
          nodes {
            commitCount
            occurredAt
          }
        }
      }
    }
  }
}
"#;

/// Normalized payload for `cli.forge-cli.activity.commits.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ActivityCommitsPayload {
    pub provider: &'static str,
    pub host: String,
    pub user: String,
    pub since: Option<String>,
    pub limit: u32,
    pub item_count: usize,
    pub limited: bool,
    pub items: Vec<ActivityCommit>,
}

/// One commit row in an activity commits result.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ActivityCommit {
    pub repo: String,
    pub sha: String,
    pub url: String,
    pub message: Option<String>,
    pub authored_at: Option<String>,
    pub committed_at: Option<String>,
    pub author_name: Option<String>,
    pub author_email: Option<String>,
}

/// Normalized payload for `cli.forge-cli.activity.events.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ActivityEventsPayload {
    pub provider: &'static str,
    pub host: String,
    pub user: String,
    pub public_only: bool,
    pub limit: u32,
    pub item_count: usize,
    pub limited: bool,
    pub items: Vec<ActivityEvent>,
}

/// One event row in an activity events result.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ActivityEvent {
    pub id: String,
    pub event_type: String,
    pub repo: String,
    pub actor: Option<String>,
    pub public: Option<bool>,
    pub created_at: String,
    pub summary: Option<String>,
    pub url: Option<String>,
}

/// Normalized payload for `cli.forge-cli.activity.summary.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ActivitySummaryPayload {
    pub provider: &'static str,
    pub host: String,
    pub user: String,
    pub since: Option<String>,
    pub limit: u32,
    pub total_commit_contributions: u64,
    pub repository_count: usize,
    pub limited: bool,
    pub repositories: Vec<ActivitySummaryRepo>,
}

/// Per-repository commit contribution summary.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ActivitySummaryRepo {
    pub repo: String,
    pub commit_contributions: u64,
    pub latest_commit_at: Option<String>,
}

pub fn run(
    global: &GlobalFlags,
    command: ActivityCommand,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    run_with(&runner, global, command, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    command: ActivityCommand,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    match ctx.provider {
        Provider::GitHub => run_github(runner, global, &ctx, command, format),
        Provider::GitLab | Provider::Local => Err(provider_unsupported(&ctx, &command)),
    }
}

fn run_github<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    command: ActivityCommand,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    match command {
        ActivityCommand::Commits(args) => run_github_commits(runner, global, ctx, args, format),
        ActivityCommand::Events(args) => run_github_events(runner, global, ctx, args, format),
        ActivityCommand::Summary(args) => run_github_summary(runner, global, ctx, args, format),
    }
}

fn run_github_commits<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    args: ActivityCommitsArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let limit = limit_for_provider(args.limit);
    if global.dry_run {
        let user = dry_run_user(&args.user);
        let calls = dry_run_calls(
            ctx,
            &args.user,
            build_github_commits_call(ctx, &user, args.since.as_deref(), limit),
        );
        return Ok(emit_activity_dry_run(
            schema_ok(COMMITS_SCHEMA),
            ctx,
            calls,
            format,
        ));
    }

    let user = resolve_user(runner, ctx, &args.user)?;
    let call = build_github_commits_call(ctx, &user, args.since.as_deref(), limit);
    let output = runner.run(&call)?;
    let payload = parse_commits_output(&output, user, args.since, limit, ctx)?;
    Ok(emit_success(
        schema_ok(COMMITS_SCHEMA),
        payload,
        format,
        render_commits_text,
    ))
}

fn run_github_events<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    args: ActivityEventsArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let limit = limit_for_provider(args.limit);
    if global.dry_run {
        let user = dry_run_user(&args.user);
        let calls = dry_run_calls(
            ctx,
            &args.user,
            build_github_events_call(ctx, &user, args.public_only, limit),
        );
        return Ok(emit_activity_dry_run(
            schema_ok(EVENTS_SCHEMA),
            ctx,
            calls,
            format,
        ));
    }

    let user = resolve_user(runner, ctx, &args.user)?;
    let call = build_github_events_call(ctx, &user, args.public_only, limit);
    let output = runner.run(&call)?;
    let payload = parse_events_output(&output, user, args.public_only, limit, ctx)?;
    Ok(emit_success(
        schema_ok(EVENTS_SCHEMA),
        payload,
        format,
        render_events_text,
    ))
}

fn run_github_summary<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    args: ActivitySummaryArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let limit = limit_for_provider(args.limit);
    let since = args.since.as_deref().map(normalize_graphql_since);
    if global.dry_run {
        let user = dry_run_user(&args.user);
        let calls = dry_run_calls(
            ctx,
            &args.user,
            build_github_summary_call(ctx, &user, since.as_deref(), limit),
        );
        return Ok(emit_activity_dry_run(
            schema_ok(SUMMARY_SCHEMA),
            ctx,
            calls,
            format,
        ));
    }

    let user = resolve_user(runner, ctx, &args.user)?;
    let call = build_github_summary_call(ctx, &user, since.as_deref(), limit);
    let output = runner.run(&call)?;
    let payload = parse_summary_output(&output, user, args.since, limit, ctx)?;
    Ok(emit_success(
        schema_ok(SUMMARY_SCHEMA),
        payload,
        format,
        render_summary_text,
    ))
}

fn build_github_identity_call(ctx: &ProviderContext) -> BackendCall {
    let mut argv = vec![OsString::from("api"), OsString::from("user")];
    push_github_hostname(ctx, &mut argv);
    argv.push(OsString::from("--jq"));
    argv.push(OsString::from(".login"));
    BackendCall::new(BackendProgram::Gh, argv)
}

fn build_github_commits_call(
    ctx: &ProviderContext,
    user: &str,
    since: Option<&str>,
    limit: u32,
) -> BackendCall {
    let mut query = format!("author:{user}");
    if let Some(since) = since.filter(|s| !s.trim().is_empty()) {
        query.push_str(" author-date:>=");
        query.push_str(since.trim());
    }
    let mut argv = vec![OsString::from("api"), OsString::from("search/commits")];
    push_github_hostname(ctx, &mut argv);
    argv.extend([
        OsString::from("--method"),
        OsString::from("GET"),
        OsString::from("-f"),
        OsString::from(format!("q={query}")),
        OsString::from("-f"),
        OsString::from("sort=author-date"),
        OsString::from("-f"),
        OsString::from("order=desc"),
        OsString::from("-f"),
        OsString::from(format!("per_page={limit}")),
        OsString::from("--jq"),
        OsString::from(".items"),
    ]);
    BackendCall::new(BackendProgram::Gh, argv)
}

fn build_github_events_call(
    ctx: &ProviderContext,
    user: &str,
    public_only: bool,
    limit: u32,
) -> BackendCall {
    let endpoint = if public_only {
        format!("users/{user}/events/public")
    } else {
        format!("users/{user}/events")
    };
    let mut argv = vec![OsString::from("api"), OsString::from(endpoint)];
    push_github_hostname(ctx, &mut argv);
    argv.extend([
        OsString::from("--method"),
        OsString::from("GET"),
        OsString::from("-f"),
        OsString::from(format!("per_page={limit}")),
    ]);
    BackendCall::new(BackendProgram::Gh, argv)
}

fn build_github_summary_call(
    ctx: &ProviderContext,
    user: &str,
    since: Option<&str>,
    limit: u32,
) -> BackendCall {
    let mut argv = vec![OsString::from("api"), OsString::from("graphql")];
    push_github_hostname(ctx, &mut argv);
    argv.extend([
        OsString::from("-F"),
        OsString::from(format!("login={user}")),
        OsString::from("-F"),
        OsString::from(format!("maxRepositories={limit}")),
    ]);
    if let Some(since) = since {
        argv.push(OsString::from("-F"));
        argv.push(OsString::from(format!("from={since}")));
    }
    argv.extend([
        OsString::from("-f"),
        OsString::from(format!("query={SUMMARY_GRAPHQL_QUERY}")),
    ]);
    BackendCall::new(BackendProgram::Gh, argv)
}

fn push_github_hostname(ctx: &ProviderContext, argv: &mut Vec<OsString>) {
    if ctx.host != "github.com" {
        argv.push(OsString::from("--hostname"));
        argv.push(OsString::from(&ctx.host));
    }
}

fn resolve_user<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    user: &str,
) -> Result<String, ForgeError> {
    if user != "@me" {
        return Ok(user.to_string());
    }
    let output = runner.run(&build_github_identity_call(ctx))?;
    let login = output.stdout.trim();
    if login.is_empty() {
        return Err(ForgeError::software(
            schema_err(),
            "gh api user did not return a login for activity --user @me",
            None,
        ));
    }
    Ok(login.to_string())
}

fn dry_run_user(user: &str) -> String {
    if user == "@me" {
        "<resolved-login>".to_string()
    } else {
        user.to_string()
    }
}

fn dry_run_calls(
    ctx: &ProviderContext,
    requested_user: &str,
    data_call: BackendCall,
) -> Vec<BackendCall> {
    if requested_user == "@me" {
        vec![build_github_identity_call(ctx), data_call]
    } else {
        vec![data_call]
    }
}

fn limit_for_provider(limit: u32) -> u32 {
    limit.clamp(1, GITHUB_PAGE_LIMIT)
}

fn normalize_graphql_since(since: &str) -> String {
    let trimmed = since.trim();
    if trimmed.len() == 10
        && trimmed.as_bytes().get(4) == Some(&b'-')
        && trimmed.as_bytes().get(7) == Some(&b'-')
    {
        format!("{trimmed}T00:00:00Z")
    } else {
        trimmed.to_string()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct ActivityDryRunPayload {
    provider: &'static str,
    host: String,
    plan: Vec<String>,
    plans: Vec<Vec<String>>,
}

fn emit_activity_dry_run(
    schema_version: String,
    ctx: &ProviderContext,
    calls: Vec<BackendCall>,
    format: OutputFormat,
) -> i32 {
    let plans: Vec<Vec<String>> = calls.iter().map(BackendCall::plan_argv).collect();
    let payload = ActivityDryRunPayload {
        provider: ctx.provider.as_str(),
        host: ctx.host.clone(),
        plan: plans.last().cloned().unwrap_or_default(),
        plans,
    };
    emit_success(schema_version, payload, format, |payload| {
        for plan in &payload.plans {
            println!("would run: {}", plan.join(" "));
        }
    })
}

fn parse_commits_output(
    output: &BackendSuccess,
    user: String,
    since: Option<String>,
    limit: u32,
    ctx: &ProviderContext,
) -> Result<ActivityCommitsPayload, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "GitHub activity commits JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    let arr = search_items(&value).ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "GitHub activity commits JSON is not an array or search result object",
            Some(format!("got: {value}")),
        )
    })?;
    let items = arr
        .iter()
        .map(parse_commit_item)
        .collect::<Result<Vec<_>, _>>()?;
    let item_count = items.len();
    Ok(ActivityCommitsPayload {
        provider: ctx.provider.as_str(),
        host: ctx.host.clone(),
        user,
        since,
        limit,
        item_count,
        limited: reached_limit(item_count, limit),
        items,
    })
}

fn search_items(value: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    value
        .as_array()
        .or_else(|| value.get("items").and_then(|items| items.as_array()))
}

fn parse_commit_item(raw: &serde_json::Value) -> Result<ActivityCommit, ForgeError> {
    let commit = raw.get("commit").unwrap_or(raw);
    let author = commit.get("author");
    let committer = commit.get("committer");
    Ok(ActivityCommit {
        repo: raw
            .get("repository")
            .and_then(|repo| {
                repo.get("full_name")
                    .or_else(|| repo.get("nameWithOwner"))
                    .or_else(|| repo.get("name"))
            })
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| missing("repository.full_name"))?,
        sha: required_str(raw, "sha")?,
        url: raw
            .get("html_url")
            .or_else(|| raw.get("url"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| missing("html_url"))?,
        message: commit
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        authored_at: optional_nested_str(author, "date"),
        committed_at: optional_nested_str(committer, "date"),
        author_name: optional_nested_str(author, "name"),
        author_email: optional_nested_str(author, "email"),
    })
}

fn optional_nested_str(raw: Option<&serde_json::Value>, key: &str) -> Option<String> {
    raw.and_then(|value| value.get(key))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn parse_events_output(
    output: &BackendSuccess,
    user: String,
    public_only: bool,
    limit: u32,
    ctx: &ProviderContext,
) -> Result<ActivityEventsPayload, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "GitHub activity events JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    let arr = value.as_array().ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "GitHub activity events JSON is not an array",
            Some(format!("got: {value}")),
        )
    })?;
    let items = arr
        .iter()
        .map(parse_event_item)
        .collect::<Result<Vec<_>, _>>()?;
    let item_count = items.len();
    Ok(ActivityEventsPayload {
        provider: ctx.provider.as_str(),
        host: ctx.host.clone(),
        user,
        public_only,
        limit,
        item_count,
        limited: reached_limit(item_count, limit),
        items,
    })
}

fn parse_event_item(raw: &serde_json::Value) -> Result<ActivityEvent, ForgeError> {
    let event_type = required_str(raw, "type")?;
    Ok(ActivityEvent {
        id: required_str(raw, "id")?,
        event_type: event_type.clone(),
        repo: raw
            .get("repo")
            .and_then(|repo| repo.get("name"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| missing("repo.name"))?,
        actor: raw
            .get("actor")
            .and_then(|actor| actor.get("login"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        public: raw.get("public").and_then(|v| v.as_bool()),
        created_at: required_str(raw, "created_at")?,
        summary: event_summary(&event_type, raw),
        url: None,
    })
}

fn event_summary(event_type: &str, raw: &serde_json::Value) -> Option<String> {
    let payload = raw.get("payload")?;
    if let Some(action) = payload.get("action").and_then(|v| v.as_str()) {
        return Some(format!("{event_type} {action}"));
    }
    if event_type == "PushEvent" {
        let count = payload
            .get("commits")
            .and_then(|v| v.as_array())
            .map(Vec::len)
            .unwrap_or(0);
        if count > 0 {
            return Some(format!("{event_type} {count} commit(s)"));
        }
    }
    None
}

fn parse_summary_output(
    output: &BackendSuccess,
    user: String,
    since: Option<String>,
    limit: u32,
    ctx: &ProviderContext,
) -> Result<ActivitySummaryPayload, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "GitHub activity summary JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    let collection = value
        .pointer("/data/user/contributionsCollection")
        .ok_or_else(|| missing("data.user.contributionsCollection"))?;
    let total_commit_contributions = collection
        .get("totalCommitContributions")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| missing("totalCommitContributions"))?;
    let repos = collection
        .get("commitContributionsByRepository")
        .and_then(|v| v.as_array())
        .ok_or_else(|| missing("commitContributionsByRepository"))?;
    let repositories = repos
        .iter()
        .map(parse_summary_repo)
        .collect::<Result<Vec<_>, _>>()?;
    let repository_count = repositories.len();
    Ok(ActivitySummaryPayload {
        provider: ctx.provider.as_str(),
        host: ctx.host.clone(),
        user,
        since,
        limit,
        total_commit_contributions,
        repository_count,
        limited: reached_limit(repository_count, limit),
        repositories,
    })
}

fn reached_limit(count: usize, limit: u32) -> bool {
    u32::try_from(count).is_ok_and(|count| count >= limit)
}

fn parse_summary_repo(raw: &serde_json::Value) -> Result<ActivitySummaryRepo, ForgeError> {
    let repo = raw
        .pointer("/repository/nameWithOwner")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing("repository.nameWithOwner"))?;
    let nodes = raw
        .pointer("/contributions/nodes")
        .and_then(|v| v.as_array())
        .ok_or_else(|| missing("contributions.nodes"))?;
    let commit_contributions = nodes
        .iter()
        .filter_map(|node| node.get("commitCount").and_then(|v| v.as_u64()))
        .sum();
    let latest_commit_at = nodes
        .iter()
        .filter_map(|node| node.get("occurredAt").and_then(|v| v.as_str()))
        .max()
        .map(str::to_string);
    Ok(ActivitySummaryRepo {
        repo,
        commit_contributions,
        latest_commit_at,
    })
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
        format!("missing required field '{key}' in GitHub activity output"),
        None,
    )
}

fn render_commits_text(payload: &ActivityCommitsPayload) {
    println!(
        "{provider}@{host} {user} commits: {count} result(s){since}{limited}",
        provider = payload.provider,
        host = payload.host,
        user = payload.user,
        count = payload.item_count,
        since = since_suffix(payload.since.as_deref()),
        limited = limited_suffix(payload.limited),
    );
    for item in &payload.items {
        println!(
            "{time} {repo} {sha} {message} - {url}",
            time = item
                .authored_at
                .as_deref()
                .or(item.committed_at.as_deref())
                .unwrap_or("<unknown-time>"),
            repo = item.repo,
            sha = short_sha(&item.sha),
            message = one_line(item.message.as_deref().unwrap_or("<no message>")),
            url = item.url
        );
    }
}

fn render_events_text(payload: &ActivityEventsPayload) {
    println!(
        "{provider}@{host} {user} events: {count} result(s) public_only={public_only}{limited}",
        provider = payload.provider,
        host = payload.host,
        user = payload.user,
        count = payload.item_count,
        public_only = payload.public_only,
        limited = limited_suffix(payload.limited),
    );
    for item in &payload.items {
        println!(
            "{time} {repo} {kind} {visibility} {summary}",
            time = item.created_at,
            repo = item.repo,
            kind = item.event_type,
            visibility = event_visibility(item.public),
            summary = one_line(item.summary.as_deref().unwrap_or("")),
        );
    }
}

fn render_summary_text(payload: &ActivitySummaryPayload) {
    println!(
        "{provider}@{host} {user} summary: {total} commit contribution(s) across {repos} repositories{since}{limited}",
        provider = payload.provider,
        host = payload.host,
        user = payload.user,
        total = payload.total_commit_contributions,
        repos = payload.repository_count,
        since = since_suffix(payload.since.as_deref()),
        limited = limited_suffix(payload.limited),
    );
    for repo in &payload.repositories {
        println!(
            "{repo} {count} latest={latest}",
            repo = repo.repo,
            count = repo.commit_contributions,
            latest = repo.latest_commit_at.as_deref().unwrap_or("<unknown>"),
        );
    }
}

fn since_suffix(since: Option<&str>) -> String {
    since
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(" since {}", value.trim()))
        .unwrap_or_default()
}

fn limited_suffix(limited: bool) -> &'static str {
    if limited { " (limited)" } else { "" }
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn event_visibility(public: Option<bool>) -> &'static str {
    match public {
        Some(true) => "public",
        Some(false) => "private",
        None => "visibility=unknown",
    }
}

fn command_name(command: &ActivityCommand) -> &'static str {
    match command {
        ActivityCommand::Commits(_) => "commits",
        ActivityCommand::Events(_) => "events",
        ActivityCommand::Summary(_) => "summary",
    }
}

fn provider_unsupported(ctx: &ProviderContext, command: &ActivityCommand) -> ForgeError {
    ForgeError::provider_unsupported(
        schema_err(),
        format!("activity {} is GitHub-only in v1", command_name(command)),
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
    use crate::backend::{BackendSuccess, argv_to_strings};
    use crate::cli::{ActivityCommitsArgs, ActivityEventsArgs, ActivitySummaryArgs, ProviderFlag};
    use crate::provider::DetectionSource;
    use nils_common::cli_contract::{OutputFormat, exit};
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
            repo: None,
            store_root: None,
            dry_run: false,
        }
    }

    fn commits() -> ActivityCommand {
        ActivityCommand::Commits(ActivityCommitsArgs {
            user: "@me".into(),
            since: None,
            limit: 30,
        })
    }

    fn events() -> ActivityCommand {
        ActivityCommand::Events(ActivityEventsArgs {
            user: "@me".into(),
            limit: 30,
            public_only: false,
        })
    }

    fn summary() -> ActivityCommand {
        ActivityCommand::Summary(ActivitySummaryArgs {
            user: "@me".into(),
            since: None,
            limit: 25,
        })
    }

    fn ctx() -> ProviderContext {
        ProviderContext {
            provider: Provider::GitHub,
            host: "github.com".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    #[test]
    fn github_commits_call_uses_search_api_shape() {
        let call = build_github_commits_call(&ctx(), "alice", Some("2026-05-01"), 2);
        assert_eq!(call.program, BackendProgram::Gh);
        assert_eq!(
            argv_to_strings(&call.argv),
            [
                "api",
                "search/commits",
                "--method",
                "GET",
                "-f",
                "q=author:alice author-date:>=2026-05-01",
                "-f",
                "sort=author-date",
                "-f",
                "order=desc",
                "-f",
                "per_page=2",
                "--jq",
                ".items"
            ]
        );
    }

    #[test]
    fn github_dry_run_me_includes_identity_and_data_plans() {
        let mut global = global(ProviderFlag::Github);
        global.dry_run = true;
        let code = run_with(
            &FailingRunner,
            &global,
            commits(),
            OutputFormat::Text,
            |_| None,
        )
        .expect("dry-run should not execute backend");
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn gitlab_branch_is_explicitly_unsupported() {
        let err = run_with(
            &FailingRunner,
            &global(ProviderFlag::Gitlab),
            events(),
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
            summary(),
            OutputFormat::Json,
            |_| None,
        )
        .expect_err("local unsupported");
        assert_eq!(err.kind(), "provider_unsupported");
    }
}
