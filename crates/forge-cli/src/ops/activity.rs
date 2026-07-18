//! `activity` command group.
//!
//! Personal activity commands are GitHub-only in v1. The repo-scoped `feed`
//! command has explicit GitHub and GitLab mappings and keeps provider-specific
//! event vocabulary in `provider_event_type` and `details`.

use std::ffi::OsString;

use chrono::{DateTime, NaiveDate, Utc};
use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess};
use crate::cli::{
    ActivityCommand, ActivityCommitsArgs, ActivityEventsArgs, ActivityFeedArgs,
    ActivitySummaryArgs, BINARY, GlobalFlags,
};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::gitlab_api;
use crate::provider::{Provider, ProviderContext, detect, detect_unscoped, git_remote_url};
use crate::rate_limit::default_runner;

const COMMITS_SCHEMA: &str = "activity.commits";
const EVENTS_SCHEMA: &str = "activity.events";
const FEED_SCHEMA: &str = "activity.feed";
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

/// Normalized payload for `cli.forge-cli.activity.feed.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ActivityFeedPayload {
    pub provider: &'static str,
    pub host: String,
    pub repo: String,
    pub since: Option<String>,
    pub limit: u32,
    pub item_count: usize,
    pub limited: bool,
    pub items: Vec<ActivityFeedItem>,
}

/// One repository/project-scoped activity row.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ActivityFeedItem {
    pub id: String,
    pub external_id: String,
    pub provider_event_type: Option<String>,
    pub kind: String,
    pub action: String,
    pub repo: String,
    pub target_kind: Option<String>,
    pub target_ref: Option<String>,
    pub target_iid: Option<u64>,
    pub title: Option<String>,
    pub url: Option<String>,
    pub actor: Option<String>,
    pub occurred_at: String,
    pub summary: Option<String>,
    pub details: Option<serde_json::Value>,
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
    let runner = default_runner();
    run_with(&runner, global, command, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    command: ActivityCommand,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = if matches!(&command, ActivityCommand::Feed(_)) {
        detect(
            global.provider_hint(),
            &global.remote,
            global.repo.as_deref(),
            &remote_url_lookup,
        )?
    } else {
        detect_unscoped(
            global.provider_hint(),
            &global.remote,
            global.repo.as_deref(),
            &remote_url_lookup,
        )?
    };
    match ctx.provider {
        Provider::GitHub => run_github(runner, global, &ctx, command, format, &remote_url_lookup),
        Provider::GitLab => run_gitlab(runner, global, &ctx, command, format, &remote_url_lookup),
        Provider::Local => Err(provider_unsupported(&ctx, &command)),
    }
}

fn run_github<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    command: ActivityCommand,
    format: OutputFormat,
    remote_url_lookup: &F,
) -> Result<i32, ForgeError> {
    match command {
        ActivityCommand::Commits(args) => run_github_commits(runner, global, ctx, args, format),
        ActivityCommand::Events(args) => run_github_events(runner, global, ctx, args, format),
        ActivityCommand::Feed(args) => {
            run_github_feed(runner, global, ctx, args, format, remote_url_lookup)
        }
        ActivityCommand::Summary(args) => run_github_summary(runner, global, ctx, args, format),
    }
}

fn run_gitlab<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    command: ActivityCommand,
    format: OutputFormat,
    remote_url_lookup: &F,
) -> Result<i32, ForgeError> {
    match command {
        ActivityCommand::Feed(args) => {
            run_gitlab_feed(runner, global, ctx, args, format, remote_url_lookup)
        }
        _ => Err(provider_unsupported(ctx, &command)),
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

fn run_github_feed<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    args: ActivityFeedArgs,
    format: OutputFormat,
    remote_url_lookup: &F,
) -> Result<i32, ForgeError> {
    let limit = limit_for_provider(args.limit);
    validate_feed_since(args.since.as_deref())?;
    let repo = resolve_repo_slug(ctx, &global.remote, remote_url_lookup)?;
    let calls = build_github_feed_calls(ctx, &repo, args.since.as_deref(), limit);
    if global.dry_run {
        return Ok(emit_activity_dry_run(
            schema_ok(FEED_SCHEMA),
            ctx,
            calls,
            format,
        ));
    }

    let commits = runner.run(&calls[0])?;
    let repo_activity = runner.run(&calls[1])?;
    let payload = parse_github_feed_output(&commits, &repo_activity, repo, args.since, limit, ctx)?;
    Ok(emit_success(
        schema_ok(FEED_SCHEMA),
        payload,
        format,
        render_feed_text,
    ))
}

fn run_gitlab_feed<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    ctx: &ProviderContext,
    args: ActivityFeedArgs,
    format: OutputFormat,
    remote_url_lookup: &F,
) -> Result<i32, ForgeError> {
    let limit = limit_for_provider(args.limit);
    validate_feed_since(args.since.as_deref())?;
    let repo = resolve_repo_slug(ctx, &global.remote, remote_url_lookup)?;
    let calls = build_gitlab_feed_calls(ctx, &repo, args.since.as_deref(), limit);
    if global.dry_run {
        return Ok(emit_activity_dry_run(
            schema_ok(FEED_SCHEMA),
            ctx,
            calls,
            format,
        ));
    }

    let commits = runner.run(&calls[0])?;
    let events = runner.run(&calls[1])?;
    let payload = parse_gitlab_feed_output(&commits, &events, repo, args.since, limit, ctx)?;
    Ok(emit_success(
        schema_ok(FEED_SCHEMA),
        payload,
        format,
        render_feed_text,
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

fn build_github_feed_calls(
    ctx: &ProviderContext,
    repo: &str,
    since: Option<&str>,
    limit: u32,
) -> Vec<BackendCall> {
    let mut commits = vec![
        OsString::from("api"),
        OsString::from(format!("repos/{repo}/commits")),
    ];
    push_github_hostname(ctx, &mut commits);
    commits.extend([
        OsString::from("--method"),
        OsString::from("GET"),
        OsString::from("-f"),
        OsString::from(format!("per_page={limit}")),
    ]);
    if let Some(since) = since.filter(|s| !s.trim().is_empty()) {
        commits.push(OsString::from("-f"));
        commits.push(OsString::from(format!("since={}", since.trim())));
    }

    let mut repo_activity = vec![
        OsString::from("api"),
        OsString::from(format!("repos/{repo}/activity")),
    ];
    push_github_hostname(ctx, &mut repo_activity);
    repo_activity.extend([
        OsString::from("--method"),
        OsString::from("GET"),
        OsString::from("-f"),
        OsString::from(format!("per_page={limit}")),
        OsString::from("-f"),
        OsString::from(format!(
            "time_period={}",
            if since.is_some() { "year" } else { "quarter" }
        )),
    ]);

    vec![
        BackendCall::new(BackendProgram::Gh, commits),
        BackendCall::new(BackendProgram::Gh, repo_activity),
    ]
}

fn build_gitlab_feed_calls(
    ctx: &ProviderContext,
    repo: &str,
    since: Option<&str>,
    limit: u32,
) -> Vec<BackendCall> {
    let project = gitlab_api::encode_project_path(repo);
    let since_query = since
        .filter(|s| !s.trim().is_empty())
        .map(|s| format!("&since={}", gitlab_api::encode_query_value(s.trim())))
        .unwrap_or_default();
    let events_after = since
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            let date = s.trim().split('T').next().unwrap_or(s.trim());
            format!("&after={}", gitlab_api::encode_query_value(date))
        })
        .unwrap_or_default();
    vec![
        gitlab_api::api_call(
            &ctx.host,
            format!("projects/{project}/repository/commits?per_page={limit}{since_query}"),
        ),
        gitlab_api::api_call(
            &ctx.host,
            format!("projects/{project}/events?per_page={limit}&sort=desc{events_after}"),
        ),
    ]
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
        "activity feed is single-repo scoped: pass --repo owner/name or run inside a repo with a recognised forge remote",
        None,
    ))
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

fn parse_github_feed_output(
    commits_output: &BackendSuccess,
    repo_activity_output: &BackendSuccess,
    repo: String,
    since: Option<String>,
    limit: u32,
    ctx: &ProviderContext,
) -> Result<ActivityFeedPayload, ForgeError> {
    let commits = parse_feed_array(commits_output, "GitHub activity feed commits")?;
    let repo_activity = parse_feed_array(
        repo_activity_output,
        "GitHub activity feed repository activity",
    )?;
    let mut items = Vec::with_capacity(commits.len() + repo_activity.len());
    for raw in commits {
        items.push(parse_github_feed_commit(&raw, &repo, ctx)?);
    }
    for raw in repo_activity {
        items.push(parse_github_repo_activity(&raw, &repo, ctx)?);
    }
    filter_feed_since(&mut items, since.as_deref())?;
    let limited = truncate_feed_items(&mut items, limit)?;
    let item_count = items.len();
    Ok(ActivityFeedPayload {
        provider: ctx.provider.as_str(),
        host: ctx.host.clone(),
        repo,
        since,
        limit,
        item_count,
        limited,
        items,
    })
}

fn parse_gitlab_feed_output(
    commits_output: &BackendSuccess,
    events_output: &BackendSuccess,
    repo: String,
    since: Option<String>,
    limit: u32,
    ctx: &ProviderContext,
) -> Result<ActivityFeedPayload, ForgeError> {
    let commits = parse_feed_array(commits_output, "GitLab activity feed commits")?;
    let events = parse_feed_array(events_output, "GitLab activity feed events")?;
    let mut items = Vec::with_capacity(commits.len() + events.len());
    for raw in commits {
        items.push(parse_gitlab_feed_commit(&raw, &repo, ctx)?);
    }
    for raw in events {
        items.push(parse_gitlab_project_event(&raw, &repo, ctx)?);
    }
    filter_feed_since(&mut items, since.as_deref())?;
    let limited = truncate_feed_items(&mut items, limit)?;
    let item_count = items.len();
    Ok(ActivityFeedPayload {
        provider: ctx.provider.as_str(),
        host: ctx.host.clone(),
        repo,
        since,
        limit,
        item_count,
        limited,
        items,
    })
}

fn parse_feed_array(
    output: &BackendSuccess,
    label: &'static str,
) -> Result<Vec<serde_json::Value>, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            format!("{label} JSON is invalid"),
            Some(e.to_string()),
        )
    })?;
    value.as_array().cloned().ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            format!("{label} JSON is not an array"),
            Some(format!("got: {value}")),
        )
    })
}

fn parse_github_feed_commit(
    raw: &serde_json::Value,
    repo: &str,
    ctx: &ProviderContext,
) -> Result<ActivityFeedItem, ForgeError> {
    let commit = raw.get("commit").unwrap_or(raw);
    let sha = required_feed_str(raw, "sha", "GitHub")?;
    let occurred_at = nested_str(commit, "committer", "date")
        .or_else(|| nested_str(commit, "author", "date"))
        .ok_or_else(|| missing_feed("GitHub", "commit.committer.date"))?;
    let title = first_line(
        commit
            .get("message")
            .or_else(|| raw.get("message"))
            .and_then(|v| v.as_str()),
    );
    let actor = raw
        .pointer("/author/login")
        .or_else(|| commit.pointer("/author/name"))
        .or_else(|| commit.pointer("/committer/name"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let url = raw
        .get("html_url")
        .or_else(|| raw.get("web_url"))
        .or_else(|| raw.get("url"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let external_id = format!("commit:{repo}:{sha}");
    Ok(ActivityFeedItem {
        id: activity_id(ctx, &external_id),
        external_id,
        provider_event_type: Some("commit".to_string()),
        kind: "commit".to_string(),
        action: "committed".to_string(),
        repo: repo.to_string(),
        target_kind: Some("commit".to_string()),
        target_ref: Some(sha.clone()),
        target_iid: None,
        title: title.clone(),
        url,
        actor,
        occurred_at,
        summary: Some(format!("Committed {} in {repo}", short_sha(&sha))),
        details: Some(serde_json::json!({
            "sha": sha,
            "message": title,
        })),
    })
}

fn parse_github_repo_activity(
    raw: &serde_json::Value,
    repo: &str,
    ctx: &ProviderContext,
) -> Result<ActivityFeedItem, ForgeError> {
    let occurred_at = required_feed_str(raw, "pushed_at", "GitHub")?;
    let raw_ref = optional_str(raw, "ref");
    let push_type = optional_str(raw, "push_type").unwrap_or_else(|| "push".to_string());
    let target_kind = github_ref_kind(raw_ref.as_deref()).to_string();
    let action = map_github_repo_activity_action(&push_type).to_string();
    let kind = if target_kind == "ref" {
        "repository".to_string()
    } else {
        target_kind.clone()
    };
    let title = raw_ref.as_deref().and_then(short_ref_owned);
    let actor = raw
        .pointer("/pusher/login")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let before = optional_str(raw, "before");
    let after = optional_str(raw, "after");
    let external_id = format!(
        "repo-activity:{repo}:{push_type}:{}:{}:{}:{occurred_at}",
        raw_ref.as_deref().unwrap_or(""),
        before.as_deref().unwrap_or(""),
        after.as_deref().unwrap_or(""),
    );
    Ok(ActivityFeedItem {
        id: activity_id(ctx, &external_id),
        external_id,
        provider_event_type: Some(push_type.clone()),
        kind,
        action: action.clone(),
        repo: repo.to_string(),
        target_kind: Some(target_kind),
        target_ref: raw_ref.clone(),
        target_iid: None,
        title,
        url: None,
        actor,
        occurred_at,
        summary: Some(github_repo_activity_summary(
            &action,
            raw_ref.as_deref(),
            repo,
        )),
        details: Some(serde_json::json!({
            "ref": raw_ref,
            "before": before,
            "after": after,
            "push_type": push_type,
        })),
    })
}

fn parse_gitlab_feed_commit(
    raw: &serde_json::Value,
    repo: &str,
    ctx: &ProviderContext,
) -> Result<ActivityFeedItem, ForgeError> {
    let sha = required_feed_str(raw, "id", "GitLab")?;
    let occurred_at = optional_str(raw, "committed_date")
        .or_else(|| optional_str(raw, "created_at"))
        .or_else(|| optional_str(raw, "authored_date"))
        .ok_or_else(|| missing_feed("GitLab", "committed_date"))?;
    let title = first_line(
        raw.get("title")
            .or_else(|| raw.get("message"))
            .and_then(|v| v.as_str()),
    );
    let actor = optional_str(raw, "author_name").or_else(|| optional_str(raw, "committer_name"));
    let external_id = format!("commit:{repo}:{sha}");
    Ok(ActivityFeedItem {
        id: activity_id(ctx, &external_id),
        external_id,
        provider_event_type: Some("commit".to_string()),
        kind: "commit".to_string(),
        action: "committed".to_string(),
        repo: repo.to_string(),
        target_kind: Some("commit".to_string()),
        target_ref: Some(sha.clone()),
        target_iid: None,
        title: title.clone(),
        url: optional_str(raw, "web_url"),
        actor,
        occurred_at,
        summary: Some(format!("Committed {} in {repo}", short_sha(&sha))),
        details: Some(serde_json::json!({
            "sha": sha,
            "message": title,
        })),
    })
}

fn parse_gitlab_project_event(
    raw: &serde_json::Value,
    repo: &str,
    ctx: &ProviderContext,
) -> Result<ActivityFeedItem, ForgeError> {
    let event_id = required_feed_value_string(raw, "id", "GitLab")?;
    let occurred_at = required_feed_str(raw, "created_at", "GitLab")?;
    let action_name = optional_str(raw, "action_name").unwrap_or_else(|| "updated".to_string());
    let data = raw.get("push_data").or_else(|| raw.get("data"));
    let target_type = optional_str(raw, "target_type");
    let kind = map_gitlab_event_target_kind(target_type.as_deref(), &action_name, data);
    let action = map_gitlab_event_action(&action_name);
    let title = optional_str(raw, "target_title")
        .or_else(|| data.and_then(|d| optional_str(d, "commit_title")))
        .or_else(|| data.and_then(|d| optional_str(d, "ref")));
    let target_ref = data.and_then(|d| optional_str(d, "ref"));
    let actor = optional_str(raw, "author_username").or_else(|| {
        raw.pointer("/author/username")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    });
    let external_id = format!("event:{repo}:{event_id}");
    Ok(ActivityFeedItem {
        id: activity_id(ctx, &external_id),
        external_id,
        provider_event_type: Some(action_name.clone()),
        kind: kind.clone(),
        action: action.clone(),
        repo: repo.to_string(),
        target_kind: Some(kind.clone()),
        target_ref: target_ref.clone(),
        target_iid: raw.get("target_iid").and_then(|v| v.as_u64()),
        title: title.clone(),
        url: None,
        actor,
        occurred_at,
        summary: Some(gitlab_event_summary(&action, &kind, title.as_deref(), repo)),
        details: Some(serde_json::json!({
            "action_name": action_name,
            "target_type": target_type,
            "ref": target_ref,
            "commit_from": data.and_then(|d| optional_str(d, "commit_from")),
            "commit_to": data.and_then(|d| optional_str(d, "commit_to")),
        })),
    })
}

fn truncate_feed_items(items: &mut Vec<ActivityFeedItem>, limit: u32) -> Result<bool, ForgeError> {
    let mut keyed = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        let occurred_at = parse_feed_timestamp(&item.occurred_at, "activity feed occurred_at")?;
        keyed.push((occurred_at, item));
    }
    keyed.sort_by(|(occurred_at_a, item_a), (occurred_at_b, item_b)| {
        occurred_at_b
            .cmp(occurred_at_a)
            .then_with(|| item_b.id.cmp(&item_a.id))
    });
    let limited = reached_limit(keyed.len(), limit);
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    keyed.truncate(limit);
    *items = keyed.into_iter().map(|(_, item)| item).collect();
    Ok(limited)
}

fn filter_feed_since(
    items: &mut Vec<ActivityFeedItem>,
    since: Option<&str>,
) -> Result<(), ForgeError> {
    let Some(threshold) = since
        .filter(|value| !value.trim().is_empty())
        .map(parse_since_threshold)
        .transpose()?
    else {
        return Ok(());
    };
    let mut filtered = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        let occurred_at = parse_feed_timestamp(&item.occurred_at, "activity feed occurred_at")?;
        if occurred_at >= threshold {
            filtered.push(item);
        }
    }
    *items = filtered;
    Ok(())
}

fn validate_feed_since(since: Option<&str>) -> Result<(), ForgeError> {
    if let Some(since) = since.filter(|value| !value.trim().is_empty()) {
        parse_since_threshold(since)?;
    }
    Ok(())
}

fn parse_since_threshold(raw: &str) -> Result<DateTime<Utc>, ForgeError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid_since(raw, "value is empty"));
    }
    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let Some(start_of_day) = date.and_hms_opt(0, 0, 0) else {
            return Err(invalid_since(raw, "date is outside the supported range"));
        };
        return Ok(start_of_day.and_utc());
    }
    parse_feed_timestamp(trimmed, "activity feed --since").map_err(|err| {
        invalid_since(
            raw,
            &format!("expected YYYY-MM-DD or RFC3339 datetime ({err})"),
        )
    })
}

fn parse_feed_timestamp(raw: &str, label: &'static str) -> Result<DateTime<Utc>, ForgeError> {
    let trimmed = raw.trim();
    let candidate = if datetime_lacks_timezone(trimmed) {
        format!("{trimmed}Z")
    } else {
        trimmed.to_string()
    };
    DateTime::parse_from_rfc3339(&candidate)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            ForgeError::software(
                schema_err(),
                format!("invalid {label} timestamp"),
                Some(format!("value={raw}; {e}")),
            )
        })
}

fn datetime_lacks_timezone(raw: &str) -> bool {
    let Some((_, time)) = raw.split_once('T') else {
        return false;
    };
    !time.ends_with('Z') && !time.contains('+') && !time.contains('-')
}

fn invalid_since(raw: &str, detail: &str) -> ForgeError {
    ForgeError::validation(
        schema_err(),
        "invalid_since",
        "activity feed --since must be a YYYY-MM-DD date or RFC3339 datetime",
        Some(format!("value={raw}; {detail}")),
    )
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

fn required_feed_str(
    raw: &serde_json::Value,
    key: &str,
    provider: &'static str,
) -> Result<String, ForgeError> {
    optional_str(raw, key).ok_or_else(|| missing_feed(provider, key))
}

fn required_feed_value_string(
    raw: &serde_json::Value,
    key: &str,
    provider: &'static str,
) -> Result<String, ForgeError> {
    raw.get(key)
        .and_then(value_to_string)
        .ok_or_else(|| missing_feed(provider, key))
}

fn missing_feed(provider: &'static str, key: &str) -> ForgeError {
    ForgeError::software(
        schema_err(),
        format!("missing required field '{key}' in {provider} activity feed output"),
        None,
    )
}

fn optional_str(raw: &serde_json::Value, key: &str) -> Option<String> {
    raw.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn value_to_string(value: &serde_json::Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return (!s.is_empty()).then(|| s.to_string());
    }
    if let Some(n) = value.as_u64() {
        return Some(n.to_string());
    }
    if let Some(n) = value.as_i64() {
        return Some(n.to_string());
    }
    None
}

fn nested_str(raw: &serde_json::Value, parent: &str, key: &str) -> Option<String> {
    raw.get(parent)
        .and_then(|value| value.get(key))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn activity_id(ctx: &ProviderContext, external_id: &str) -> String {
    format!("{}:{}|{}", ctx.provider.as_str(), ctx.host, external_id)
}

fn first_line(value: Option<&str>) -> Option<String> {
    value
        .and_then(|s| s.split('\n').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn short_ref_owned(raw: &str) -> Option<String> {
    let s = raw
        .strip_prefix("refs/heads/")
        .or_else(|| raw.strip_prefix("refs/tags/"))
        .unwrap_or(raw)
        .trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn github_ref_kind(raw_ref: Option<&str>) -> &'static str {
    let Some(raw_ref) = raw_ref else {
        return "ref";
    };
    if raw_ref.starts_with("refs/tags/") {
        "tag"
    } else if raw_ref.starts_with("refs/heads/") {
        "branch"
    } else {
        "ref"
    }
}

fn map_github_repo_activity_action(push_type: &str) -> &'static str {
    match push_type {
        "force_push" => "force_pushed",
        "branch_creation" => "created",
        "branch_deletion" => "deleted",
        "pr_merge" | "merge_queue_merge" => "merged",
        _ => "pushed",
    }
}

fn github_repo_activity_summary(action: &str, raw_ref: Option<&str>, repo: &str) -> String {
    let target = raw_ref
        .and_then(short_ref_owned)
        .unwrap_or_else(|| "repository".to_string());
    match action {
        "created" => format!("Created {target} in {repo}"),
        "deleted" => format!("Deleted {target} in {repo}"),
        "force_pushed" => format!("Force-pushed {target} in {repo}"),
        "merged" => format!("Merged into {target} in {repo}"),
        _ => format!("Pushed {target} in {repo}"),
    }
}

fn map_gitlab_event_action(action: &str) -> String {
    let s = action.to_ascii_lowercase();
    match s.as_str() {
        "commented on" => "commented".to_string(),
        "pushed to" | "pushed" => "pushed".to_string(),
        "pushed new" => "created".to_string(),
        "deleted" => "deleted".to_string(),
        "opened" => "opened".to_string(),
        "closed" => "closed".to_string(),
        "merged" => "merged".to_string(),
        "reopened" => "reopened".to_string(),
        _ => {
            let normalized = s.split_whitespace().collect::<Vec<_>>().join("_");
            if normalized.is_empty() {
                "updated".to_string()
            } else {
                normalized
            }
        }
    }
}

fn map_gitlab_event_target_kind(
    target_type: Option<&str>,
    action: &str,
    data: Option<&serde_json::Value>,
) -> String {
    match target_type {
        Some("Issue") | Some("issue") => return "issue".to_string(),
        Some("MergeRequest") | Some("merge_request") => return "change_request".to_string(),
        Some("Note") | Some("note") => return "comment".to_string(),
        _ => {}
    }
    if data.and_then(|d| d.get("ref")).is_some() {
        return "push".to_string();
    }
    if action.to_ascii_lowercase().contains("push") {
        return "push".to_string();
    }
    "repository".to_string()
}

fn gitlab_event_summary(action: &str, kind: &str, title: Option<&str>, repo: &str) -> String {
    let target = title.filter(|s| !s.trim().is_empty()).unwrap_or(kind);
    format!("{} {target} in {repo}", action.replace('_', " "))
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

fn render_feed_text(payload: &ActivityFeedPayload) {
    println!(
        "{provider}@{host} {repo} feed: {count} result(s){since}{limited}",
        provider = payload.provider,
        host = payload.host,
        repo = payload.repo,
        count = payload.item_count,
        since = since_suffix(payload.since.as_deref()),
        limited = limited_suffix(payload.limited),
    );
    for item in &payload.items {
        println!(
            "{time} {repo} {action} {kind} {summary}",
            time = item.occurred_at,
            repo = item.repo,
            action = item.action,
            kind = item.kind,
            summary = one_line(
                item.summary
                    .as_deref()
                    .or(item.title.as_deref())
                    .unwrap_or("")
            ),
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
        ActivityCommand::Feed(_) => "feed",
        ActivityCommand::Summary(_) => "summary",
    }
}

fn provider_unsupported(ctx: &ProviderContext, command: &ActivityCommand) -> ForgeError {
    if matches!(command, ActivityCommand::Feed(_)) {
        return ForgeError::provider_unsupported(
            schema_err(),
            "activity feed is unsupported for this provider",
            Some(format!("provider={}", ctx.provider.as_str())),
        );
    }
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
            host: None,
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
