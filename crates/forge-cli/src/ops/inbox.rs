//! Cross-repo personal work inbox.
//!
//! `forge-cli inbox` is intentionally separate from repo-local lifecycle
//! commands. It can query more than one provider in a single invocation,
//! normalize items into one JSON contract, and report provider-local failures
//! without hiding successful results from another provider.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess, ProcessRunner};
use crate::cli::{BINARY, GlobalFlags, InboxCommand, InboxKindFlag, InboxNextArgs, InboxQueryArgs};
use crate::envelope::emit_success_with_warnings;
use crate::error::ForgeError;
use crate::provider::{Provider, classify_host, git_remote_url, parse_host};

const LIST_SCHEMA: &str = "inbox.list";
const STATUS_SCHEMA: &str = "inbox.status";
const NEXT_SCHEMA: &str = "inbox.next";
const SCHEMA_VERSION: u32 = 1;
const DEFAULT_QUERY_LIMIT: u32 = 30;
const GH_JSON_FIELDS: &str = "number,url,title,updatedAt,author,repository";

#[derive(Debug, Clone)]
struct ProviderTarget {
    provider: Provider,
    host: String,
}

#[derive(Debug, Clone)]
struct QueryConfig {
    reasons: Vec<InboxKindFlag>,
    query_limit: u32,
}

#[derive(Debug, Clone)]
struct ProviderSuccess {
    items: Vec<InboxItem>,
    limited: bool,
}

#[derive(Debug, Clone)]
struct InboxCollection {
    providers: Vec<InboxProviderStatus>,
    items: Vec<InboxItem>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InboxProviderStatus {
    pub provider: &'static str,
    pub host: String,
    pub ok: bool,
    pub item_count: usize,
    pub limited: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<InboxProviderError>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InboxProviderError {
    pub kind: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InboxItem {
    pub provider: &'static str,
    pub host: String,
    pub kind: String,
    pub reasons: Vec<String>,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub updated_at: String,
    pub author: Option<String>,
    pub source: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct InboxListPayload {
    providers: Vec<InboxProviderStatus>,
    limit: u32,
    items: Vec<InboxItem>,
}

#[derive(Debug, Clone, Serialize)]
struct InboxStatusPayload {
    providers: Vec<InboxProviderStatus>,
    limit: u32,
    item_count: usize,
    counts: Vec<InboxCount>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct InboxCount {
    provider: &'static str,
    host: String,
    kind: String,
    reason: String,
    count: usize,
    limited: bool,
}

#[derive(Debug, Clone, Serialize)]
struct InboxNextPayload {
    providers: Vec<InboxProviderStatus>,
    limit: u32,
    query_limit: u32,
    items: Vec<InboxItem>,
}

#[derive(Debug, Clone, Serialize)]
struct InboxDryRunPayload {
    providers: Vec<InboxDryRunProvider>,
    limit: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct InboxDryRunProvider {
    provider: &'static str,
    host: String,
    plans: Vec<Vec<String>>,
}

#[derive(Debug, Clone)]
struct ProviderQuery {
    reason: InboxKindFlag,
    source: &'static str,
    call: BackendCall,
}

#[derive(Debug, Clone)]
struct GitlabIdentity {
    id: String,
    username: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct ItemKey {
    provider: &'static str,
    host: String,
    repo: String,
    number: u64,
    url: String,
}

pub fn run(
    global: &GlobalFlags,
    command: InboxCommand,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    run_with(&runner, global, command, format)
}

pub fn run_with<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    command: InboxCommand,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    match command {
        InboxCommand::List(args) => run_list(runner, global, args, format),
        InboxCommand::Status(args) => run_status(runner, global, args, format),
        InboxCommand::Next(args) => run_next(runner, global, args, format),
    }
}

fn run_list<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    args: InboxQueryArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let targets = resolve_targets(global, &args.gitlab_host);
    let config = QueryConfig::new(args.kinds, args.limit.max(1));
    if global.dry_run {
        return Ok(emit_dry_run(
            schema_version_for(BINARY, LIST_SCHEMA, SCHEMA_VERSION),
            &targets,
            &config,
            args.limit.max(1),
            None,
            format,
        ));
    }

    let collection = collect_inbox(runner, &targets, &config)?;
    let payload = InboxListPayload {
        providers: collection.providers,
        limit: config.query_limit,
        items: collection.items,
    };
    Ok(emit_success_with_warnings(
        schema_version_for(BINARY, LIST_SCHEMA, SCHEMA_VERSION),
        payload,
        collection.warnings,
        format,
        render_list_text,
    ))
}

fn run_status<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    args: InboxQueryArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let targets = resolve_targets(global, &args.gitlab_host);
    let config = QueryConfig::new(args.kinds, args.limit.max(1));
    if global.dry_run {
        return Ok(emit_dry_run(
            schema_version_for(BINARY, STATUS_SCHEMA, SCHEMA_VERSION),
            &targets,
            &config,
            args.limit.max(1),
            None,
            format,
        ));
    }

    let collection = collect_inbox(runner, &targets, &config)?;
    let counts = summarize_counts(&collection.providers, &collection.items);
    let payload = InboxStatusPayload {
        providers: collection.providers,
        limit: config.query_limit,
        item_count: collection.items.len(),
        counts,
    };
    Ok(emit_success_with_warnings(
        schema_version_for(BINARY, STATUS_SCHEMA, SCHEMA_VERSION),
        payload,
        collection.warnings,
        format,
        render_status_text,
    ))
}

fn run_next<R: BackendRunner>(
    runner: &R,
    global: &GlobalFlags,
    args: InboxNextArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let targets = resolve_targets(global, &args.gitlab_host);
    let result_limit = args.limit.max(1);
    let query_limit = result_limit.max(DEFAULT_QUERY_LIMIT);
    let config = QueryConfig::new(args.kinds, query_limit);
    if global.dry_run {
        return Ok(emit_dry_run(
            schema_version_for(BINARY, NEXT_SCHEMA, SCHEMA_VERSION),
            &targets,
            &config,
            result_limit,
            Some(query_limit),
            format,
        ));
    }

    let mut collection = collect_inbox(runner, &targets, &config)?;
    collection.items.truncate(result_limit as usize);
    let payload = InboxNextPayload {
        providers: collection.providers,
        limit: result_limit,
        query_limit,
        items: collection.items,
    };
    Ok(emit_success_with_warnings(
        schema_version_for(BINARY, NEXT_SCHEMA, SCHEMA_VERSION),
        payload,
        collection.warnings,
        format,
        render_next_text,
    ))
}

impl QueryConfig {
    fn new(kinds: Vec<InboxKindFlag>, query_limit: u32) -> Self {
        let mut reasons = if kinds.is_empty() {
            vec![
                InboxKindFlag::Review,
                InboxKindFlag::Assigned,
                InboxKindFlag::Todo,
                InboxKindFlag::Authored,
            ]
        } else {
            kinds
        };
        reasons.sort_by_key(|r| reason_rank(r.as_str()));
        reasons.dedup();
        Self {
            reasons,
            query_limit,
        }
    }

    fn wants(&self, reason: InboxKindFlag) -> bool {
        self.reasons.contains(&reason)
    }
}

fn resolve_targets(global: &GlobalFlags, gitlab_host: &str) -> Vec<ProviderTarget> {
    match global.provider {
        Some(crate::cli::ProviderFlag::Github) => vec![ProviderTarget {
            provider: Provider::GitHub,
            host: github_host(global),
        }],
        Some(crate::cli::ProviderFlag::Gitlab) => vec![ProviderTarget {
            provider: Provider::GitLab,
            host: gitlab_host_for(global, gitlab_host),
        }],
        None => vec![
            ProviderTarget {
                provider: Provider::GitHub,
                host: github_host(global),
            },
            ProviderTarget {
                provider: Provider::GitLab,
                host: gitlab_host_for(global, gitlab_host),
            },
        ],
    }
}

fn github_host(global: &GlobalFlags) -> String {
    host_from_remote(global, Provider::GitHub).unwrap_or_else(|| "github.com".to_string())
}

fn gitlab_host_for(global: &GlobalFlags, explicit: &str) -> String {
    let trimmed = explicit.trim();
    if !trimmed.is_empty() && trimmed != "gitlab.com" {
        return trimmed.to_string();
    }
    host_from_remote(global, Provider::GitLab).unwrap_or_else(|| "gitlab.com".to_string())
}

fn host_from_remote(global: &GlobalFlags, provider: Provider) -> Option<String> {
    let url = git_remote_url(&global.remote)?;
    let host = parse_host(&url)?;
    if classify_host(&host) == Some(provider) {
        Some(host)
    } else {
        None
    }
}

fn collect_inbox<R: BackendRunner>(
    runner: &R,
    targets: &[ProviderTarget],
    config: &QueryConfig,
) -> Result<InboxCollection, ForgeError> {
    let mut providers = Vec::new();
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut failures = Vec::new();
    let mut successes = 0usize;

    for target in targets {
        match query_provider(runner, target, config) {
            Ok(success) => {
                successes += 1;
                let item_count = success.items.len();
                providers.push(InboxProviderStatus {
                    provider: target.provider.as_str(),
                    host: target.host.clone(),
                    ok: true,
                    item_count,
                    limited: success.limited,
                    error: None,
                });
                items.extend(success.items);
            }
            Err(err) => {
                let provider_error = InboxProviderError {
                    kind: err.kind(),
                    message: err.to_string(),
                };
                failures.push(format!(
                    "{} {}: {}",
                    target.provider.as_str(),
                    target.host,
                    provider_error.message
                ));
                warnings.push(format!(
                    "provider_failed: {} {}: {}",
                    target.provider.as_str(),
                    target.host,
                    provider_error.message
                ));
                providers.push(InboxProviderStatus {
                    provider: target.provider.as_str(),
                    host: target.host.clone(),
                    ok: false,
                    item_count: 0,
                    limited: false,
                    error: Some(provider_error),
                });
            }
        }
    }

    if successes == 0 {
        return Err(ForgeError::backend_error(
            schema_err(),
            "all selected inbox providers failed",
            Some(failures.join("; ")),
        ));
    }

    let mut items = dedupe_items(items);
    sort_items(&mut items);
    for provider in &mut providers {
        if provider.ok {
            provider.item_count = items
                .iter()
                .filter(|item| item.provider == provider.provider && item.host == provider.host)
                .count();
        }
    }

    Ok(InboxCollection {
        providers,
        items,
        warnings,
    })
}

fn query_provider<R: BackendRunner>(
    runner: &R,
    target: &ProviderTarget,
    config: &QueryConfig,
) -> Result<ProviderSuccess, ForgeError> {
    match target.provider {
        Provider::GitHub => query_github(runner, target, config),
        Provider::GitLab => query_gitlab(runner, target, config),
    }
}

fn query_github<R: BackendRunner>(
    runner: &R,
    target: &ProviderTarget,
    config: &QueryConfig,
) -> Result<ProviderSuccess, ForgeError> {
    let queries = github_queries(config);
    let mut items = Vec::new();
    let mut limited = false;
    for query in queries {
        let output = runner.run(&query.call)?;
        let parsed = parse_github_items(target, &query, &output)?;
        if parsed.len() as u32 >= config.query_limit {
            limited = true;
        }
        items.extend(parsed);
    }
    Ok(ProviderSuccess {
        items: dedupe_items(items),
        limited,
    })
}

fn query_gitlab<R: BackendRunner>(
    runner: &R,
    target: &ProviderTarget,
    config: &QueryConfig,
) -> Result<ProviderSuccess, ForgeError> {
    let identity = parse_gitlab_identity(&runner.run(&gitlab_identity_call(&target.host))?)?;
    let queries = gitlab_queries(&target.host, &identity, config);
    let mut items = Vec::new();
    let mut limited = false;
    for query in queries {
        let output = runner.run(&query.call)?;
        let parsed = parse_gitlab_items(target, &query, &output)?;
        if parsed.len() as u32 >= config.query_limit {
            limited = true;
        }
        items.extend(parsed);
    }
    Ok(ProviderSuccess {
        items: dedupe_items(items),
        limited,
    })
}

fn github_queries(config: &QueryConfig) -> Vec<ProviderQuery> {
    let mut queries = Vec::new();
    if config.wants(InboxKindFlag::Review) {
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Review,
            source: "github_search_prs",
            call: github_search_call("prs", "--review-requested", config.query_limit),
        });
    }
    if config.wants(InboxKindFlag::Assigned) {
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Assigned,
            source: "github_search_prs",
            call: github_search_call("prs", "--assignee", config.query_limit),
        });
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Assigned,
            source: "github_search_issues",
            call: github_search_call("issues", "--assignee", config.query_limit),
        });
    }
    if config.wants(InboxKindFlag::Authored) {
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Authored,
            source: "github_search_prs",
            call: github_search_call("prs", "--author", config.query_limit),
        });
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Authored,
            source: "github_search_issues",
            call: github_search_call("issues", "--author", config.query_limit),
        });
    }
    if config.wants(InboxKindFlag::Involved) {
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Involved,
            source: "github_search_prs",
            call: github_search_call("prs", "--involves", config.query_limit),
        });
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Involved,
            source: "github_search_issues",
            call: github_search_call("issues", "--involves", config.query_limit),
        });
    }
    queries
}

fn github_search_call(kind: &str, qualifier: &str, limit: u32) -> BackendCall {
    BackendCall::new(
        BackendProgram::Gh,
        [
            OsString::from("search"),
            OsString::from(kind),
            OsString::from(qualifier),
            OsString::from("@me"),
            OsString::from("--state"),
            OsString::from("open"),
            OsString::from("--sort"),
            OsString::from("updated"),
            OsString::from("--order"),
            OsString::from("desc"),
            OsString::from("--limit"),
            OsString::from(limit.to_string()),
            OsString::from("--json"),
            OsString::from(GH_JSON_FIELDS),
        ],
    )
}

fn gitlab_identity_call(host: &str) -> BackendCall {
    BackendCall::new(
        BackendProgram::Glab,
        [
            OsString::from("api"),
            OsString::from("user"),
            OsString::from("--hostname"),
            OsString::from(host),
        ],
    )
}

fn gitlab_queries(
    host: &str,
    identity: &GitlabIdentity,
    config: &QueryConfig,
) -> Vec<ProviderQuery> {
    let mut queries = Vec::new();
    if config.wants(InboxKindFlag::Assigned) {
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Assigned,
            source: "gitlab_merge_requests",
            call: gitlab_api_call(
                host,
                format!(
                    "merge_requests?scope=assigned_to_me&state=opened&order_by=updated_at&sort=desc&per_page={}",
                    config.query_limit
                ),
            ),
        });
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Assigned,
            source: "gitlab_issues",
            call: gitlab_api_call(
                host,
                format!(
                    "issues?scope=assigned_to_me&state=opened&order_by=updated_at&sort=desc&per_page={}",
                    config.query_limit
                ),
            ),
        });
    }
    if config.wants(InboxKindFlag::Review) {
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Review,
            source: "gitlab_merge_requests",
            call: gitlab_api_call(
                host,
                format!(
                    "merge_requests?reviewer_username={}&state=opened&order_by=updated_at&sort=desc&per_page={}",
                    identity.username, config.query_limit
                ),
            ),
        });
    }
    if config.wants(InboxKindFlag::Authored) {
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Authored,
            source: "gitlab_merge_requests",
            call: gitlab_api_call(
                host,
                format!(
                    "merge_requests?author_id={}&state=opened&order_by=updated_at&sort=desc&per_page={}",
                    identity.id, config.query_limit
                ),
            ),
        });
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Authored,
            source: "gitlab_issues",
            call: gitlab_api_call(
                host,
                format!(
                    "issues?author_id={}&state=opened&order_by=updated_at&sort=desc&per_page={}",
                    identity.id, config.query_limit
                ),
            ),
        });
    }
    if config.wants(InboxKindFlag::Todo) {
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Todo,
            source: "gitlab_todos",
            call: gitlab_api_call(
                host,
                format!(
                    "todos?state=pending&order_by=updated_at&sort=desc&per_page={}",
                    config.query_limit
                ),
            ),
        });
    }
    queries
}

fn gitlab_api_call(host: &str, path: String) -> BackendCall {
    BackendCall::new(
        BackendProgram::Glab,
        [
            OsString::from("api"),
            OsString::from("--hostname"),
            OsString::from(host),
            OsString::from(path),
        ],
    )
}

fn parse_github_items(
    target: &ProviderTarget,
    query: &ProviderQuery,
    output: &BackendSuccess,
) -> Result<Vec<InboxItem>, ForgeError> {
    let values = parse_array(output, "GitHub inbox JSON is invalid")?;
    values
        .iter()
        .map(|raw| {
            let number = required_u64(raw, "number")?;
            let url = required_str(raw, "url")?;
            let repo = github_repo(raw).unwrap_or_else(|| repo_from_url(&url));
            Ok(InboxItem {
                provider: Provider::GitHub.as_str(),
                host: target.host.clone(),
                kind: query.reason.as_str().to_string(),
                reasons: vec![query.reason.as_str().to_string()],
                repo,
                number,
                title: required_str(raw, "title")?,
                url,
                updated_at: optional_str(raw, "updatedAt").unwrap_or_default(),
                author: github_author(raw),
                source: query.source,
            })
        })
        .collect()
}

fn parse_gitlab_items(
    target: &ProviderTarget,
    query: &ProviderQuery,
    output: &BackendSuccess,
) -> Result<Vec<InboxItem>, ForgeError> {
    let values = parse_array(output, "GitLab inbox JSON is invalid")?;
    values
        .iter()
        .map(|raw| {
            if query.source == "gitlab_todos" {
                parse_gitlab_todo(target, query, raw)
            } else {
                parse_gitlab_work_item(target, query, raw)
            }
        })
        .collect()
}

fn parse_gitlab_work_item(
    target: &ProviderTarget,
    query: &ProviderQuery,
    raw: &serde_json::Value,
) -> Result<InboxItem, ForgeError> {
    let number = raw
        .get("iid")
        .and_then(|v| v.as_u64())
        .or_else(|| raw.get("id").and_then(|v| v.as_u64()))
        .ok_or_else(|| missing("iid"))?;
    let url = required_str(raw, "web_url")?;
    Ok(InboxItem {
        provider: Provider::GitLab.as_str(),
        host: target.host.clone(),
        kind: query.reason.as_str().to_string(),
        reasons: vec![query.reason.as_str().to_string()],
        repo: gitlab_repo(raw).unwrap_or_else(|| repo_from_url(&url)),
        number,
        title: required_str(raw, "title")?,
        url,
        updated_at: optional_str(raw, "updated_at").unwrap_or_default(),
        author: gitlab_author(raw),
        source: query.source,
    })
}

fn parse_gitlab_todo(
    target: &ProviderTarget,
    query: &ProviderQuery,
    raw: &serde_json::Value,
) -> Result<InboxItem, ForgeError> {
    let target_obj = raw.get("target").unwrap_or(raw);
    let url = optional_str(target_obj, "web_url")
        .or_else(|| optional_str(raw, "target_url"))
        .ok_or_else(|| missing("target.web_url"))?;
    let number = target_obj
        .get("iid")
        .and_then(|v| v.as_u64())
        .or_else(|| target_obj.get("id").and_then(|v| v.as_u64()))
        .or_else(|| raw.get("id").and_then(|v| v.as_u64()))
        .ok_or_else(|| missing("target.iid"))?;
    Ok(InboxItem {
        provider: Provider::GitLab.as_str(),
        host: target.host.clone(),
        kind: query.reason.as_str().to_string(),
        reasons: vec![query.reason.as_str().to_string()],
        repo: raw
            .get("project")
            .and_then(|p| optional_str(p, "path_with_namespace"))
            .or_else(|| gitlab_repo(target_obj))
            .unwrap_or_else(|| repo_from_url(&url)),
        number,
        title: optional_str(target_obj, "title")
            .or_else(|| optional_str(raw, "body"))
            .unwrap_or_else(|| "GitLab todo".to_string()),
        url,
        updated_at: optional_str(target_obj, "updated_at")
            .or_else(|| optional_str(raw, "updated_at"))
            .or_else(|| optional_str(raw, "created_at"))
            .unwrap_or_default(),
        author: target_obj
            .get("author")
            .and_then(gitlab_author_from_value)
            .or_else(|| raw.get("author").and_then(gitlab_author_from_value)),
        source: query.source,
    })
}

fn parse_gitlab_identity(output: &BackendSuccess) -> Result<GitlabIdentity, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "GitLab user JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    let id = value
        .get("id")
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .or_else(|| optional_str(&value, "id"))
        .ok_or_else(|| missing("id"))?;
    let username = required_str(&value, "username")?;
    Ok(GitlabIdentity { id, username })
}

fn parse_array(
    output: &BackendSuccess,
    message: &'static str,
) -> Result<Vec<serde_json::Value>, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim())
        .map_err(|e| ForgeError::software(schema_err(), message, Some(e.to_string())))?;
    value
        .as_array()
        .cloned()
        .ok_or_else(|| ForgeError::software(schema_err(), message, Some(format!("got: {value}"))))
}

fn github_repo(raw: &serde_json::Value) -> Option<String> {
    let repo = raw.get("repository")?;
    optional_str(repo, "nameWithOwner")
        .or_else(|| optional_str(repo, "fullName"))
        .or_else(|| optional_str(repo, "name_with_owner"))
        .or_else(|| {
            let owner = repo
                .get("owner")
                .and_then(|v| optional_str(v, "login").or_else(|| optional_str(v, "name")))?;
            let name = optional_str(repo, "name")?;
            Some(format!("{owner}/{name}"))
        })
}

fn github_author(raw: &serde_json::Value) -> Option<String> {
    raw.get("author")
        .and_then(|v| optional_str(v, "login").or_else(|| optional_str(v, "name")))
}

fn gitlab_repo(raw: &serde_json::Value) -> Option<String> {
    raw.get("project")
        .and_then(|p| optional_str(p, "path_with_namespace"))
        .or_else(|| {
            raw.get("references")
                .and_then(|r| optional_str(r, "full"))
                .map(|full| strip_gitlab_reference(&full))
        })
        .or_else(|| {
            raw.get("web_url")
                .and_then(|v| v.as_str())
                .map(repo_from_url)
        })
}

fn strip_gitlab_reference(full: &str) -> String {
    full.split_once('!')
        .map(|(repo, _)| repo)
        .or_else(|| full.split_once('#').map(|(repo, _)| repo))
        .unwrap_or(full)
        .to_string()
}

fn gitlab_author(raw: &serde_json::Value) -> Option<String> {
    raw.get("author").and_then(gitlab_author_from_value)
}

fn gitlab_author_from_value(raw: &serde_json::Value) -> Option<String> {
    optional_str(raw, "username").or_else(|| optional_str(raw, "name"))
}

fn repo_from_url(url: &str) -> String {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let path = without_scheme
        .split_once('/')
        .map(|(_, path)| path)
        .unwrap_or(without_scheme);
    let (path, gitlab_style) = path
        .split_once("/-/")
        .map(|(repo, _)| (repo, true))
        .unwrap_or((path, false));
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    let first = parts.next().unwrap_or("unknown");
    let second = parts.next().unwrap_or("unknown");
    if gitlab_style {
        path.to_string()
    } else {
        format!("{first}/{second}")
    }
}

fn required_str(raw: &serde_json::Value, key: &str) -> Result<String, ForgeError> {
    optional_str(raw, key).ok_or_else(|| missing(key))
}

fn optional_str(raw: &serde_json::Value, key: &str) -> Option<String> {
    raw.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn required_u64(raw: &serde_json::Value, key: &str) -> Result<u64, ForgeError> {
    raw.get(key)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| missing(key))
}

fn missing(key: &str) -> ForgeError {
    ForgeError::software(
        schema_err(),
        format!("missing required field '{key}' in inbox JSON"),
        None,
    )
}

fn dedupe_items(items: Vec<InboxItem>) -> Vec<InboxItem> {
    let mut map: HashMap<ItemKey, InboxItem> = HashMap::new();
    for item in items {
        let key = ItemKey {
            provider: item.provider,
            host: item.host.clone(),
            repo: item.repo.clone(),
            number: item.number,
            url: item.url.clone(),
        };
        match map.get_mut(&key) {
            Some(existing) => merge_item(existing, item),
            None => {
                map.insert(key, item);
            }
        }
    }
    map.into_values().collect()
}

fn merge_item(existing: &mut InboxItem, incoming: InboxItem) {
    let previous_primary_rank = reason_rank(existing.kind.as_str());
    let incoming_rank = reason_rank(incoming.kind.as_str());
    for reason in incoming.reasons {
        if !existing.reasons.iter().any(|r| r == &reason) {
            existing.reasons.push(reason);
        }
    }
    existing.reasons.sort_by_key(|r| reason_rank(r));
    if let Some(primary) = existing.reasons.first() {
        existing.kind = primary.clone();
    }
    if incoming_rank < previous_primary_rank {
        existing.source = incoming.source;
    }
    if incoming.updated_at > existing.updated_at {
        existing.updated_at = incoming.updated_at;
    }
    if existing.author.is_none() {
        existing.author = incoming.author;
    }
}

fn sort_items(items: &mut [InboxItem]) {
    items.sort_by(|a, b| {
        reason_rank(a.kind.as_str())
            .cmp(&reason_rank(b.kind.as_str()))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.provider.cmp(b.provider))
            .then_with(|| a.host.cmp(&b.host))
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.number.cmp(&b.number))
            .then_with(|| a.url.cmp(&b.url))
    });
}

fn reason_rank(reason: &str) -> u8 {
    match reason {
        "review" => 0,
        "assigned" => 1,
        "todo" => 2,
        "authored" => 3,
        "involved" => 4,
        _ => 9,
    }
}

fn summarize_counts(providers: &[InboxProviderStatus], items: &[InboxItem]) -> Vec<InboxCount> {
    let mut limited_by_provider: HashMap<(&'static str, &str), bool> = HashMap::new();
    for provider in providers {
        limited_by_provider.insert(
            (provider.provider, provider.host.as_str()),
            provider.limited,
        );
    }

    let mut counts: HashMap<(&'static str, String, String), usize> = HashMap::new();
    for item in items {
        for reason in &item.reasons {
            *counts
                .entry((item.provider, item.host.clone(), reason.clone()))
                .or_insert(0) += 1;
        }
    }

    let mut rows: Vec<InboxCount> = counts
        .into_iter()
        .map(|((provider, host, reason), count)| InboxCount {
            provider,
            host: host.clone(),
            kind: reason.clone(),
            reason,
            count,
            limited: *limited_by_provider
                .get(&(provider, host.as_str()))
                .unwrap_or(&false),
        })
        .collect();
    rows.sort_by(|a, b| {
        a.provider
            .cmp(b.provider)
            .then_with(|| a.host.cmp(&b.host))
            .then_with(|| reason_rank(a.reason.as_str()).cmp(&reason_rank(b.reason.as_str())))
            .then_with(|| {
                if a.count == b.count {
                    Ordering::Equal
                } else {
                    b.count.cmp(&a.count)
                }
            })
    });
    rows
}

fn emit_dry_run(
    schema_version: String,
    targets: &[ProviderTarget],
    config: &QueryConfig,
    limit: u32,
    query_limit: Option<u32>,
    format: OutputFormat,
) -> i32 {
    let providers = targets
        .iter()
        .map(|target| InboxDryRunProvider {
            provider: target.provider.as_str(),
            host: target.host.clone(),
            plans: dry_run_plans(target, config),
        })
        .collect();
    let payload = InboxDryRunPayload {
        providers,
        limit,
        query_limit,
    };
    emit_success_with_warnings(schema_version, payload, Vec::new(), format, |payload| {
        for provider in &payload.providers {
            for plan in &provider.plans {
                println!("would run: {}", plan.join(" "));
            }
        }
    })
}

fn dry_run_plans(target: &ProviderTarget, config: &QueryConfig) -> Vec<Vec<String>> {
    match target.provider {
        Provider::GitHub => github_queries(config)
            .into_iter()
            .map(|query| query.call.plan_argv())
            .collect(),
        Provider::GitLab => {
            let identity = GitlabIdentity {
                id: "<user_id>".to_string(),
                username: "<username>".to_string(),
            };
            let mut plans = vec![gitlab_identity_call(&target.host).plan_argv()];
            plans.extend(
                gitlab_queries(&target.host, &identity, config)
                    .into_iter()
                    .map(|query| query.call.plan_argv()),
            );
            plans
        }
    }
}

fn render_list_text(payload: &InboxListPayload) {
    render_items_text(&payload.items);
}

fn render_next_text(payload: &InboxNextPayload) {
    render_items_text(&payload.items);
}

fn render_items_text(items: &[InboxItem]) {
    for item in items {
        println!(
            "[{provider}:{kind}] {repo}#{number} {title} - {url}",
            provider = item.provider,
            kind = item.kind,
            repo = item.repo,
            number = item.number,
            title = item.title,
            url = item.url,
        );
    }
}

fn render_status_text(payload: &InboxStatusPayload) {
    for provider in &payload.providers {
        println!(
            "{provider}@{host}: {count} item(s){limited}",
            provider = provider.provider,
            host = provider.host,
            count = provider.item_count,
            limited = if provider.limited { " (limited)" } else { "" },
        );
    }
    for count in &payload.counts {
        println!(
            "  {reason}: {count}",
            reason = count.reason,
            count = count.count
        );
    }
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn inbox_provider_resolver_defaults_to_both_providers() {
        let global = GlobalFlags {
            format: None,
            remote: "missing-remote".into(),
            provider: None,
            repo: None,
            dry_run: false,
        };
        let targets = resolve_targets(&global, "gitlab.example.com");
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].provider, Provider::GitHub);
        assert_eq!(targets[1].provider, Provider::GitLab);
        assert_eq!(targets[1].host, "gitlab.example.com");
    }

    #[test]
    fn inbox_contract_dedupes_reasons_by_priority() {
        let item = InboxItem {
            provider: "github",
            host: "github.com".into(),
            kind: "assigned".into(),
            reasons: vec!["assigned".into()],
            repo: "acme/widgets".into(),
            number: 7,
            title: "demo".into(),
            url: "https://github.com/acme/widgets/pull/7".into(),
            updated_at: "2026-05-21T00:00:00Z".into(),
            author: Some("alice".into()),
            source: "github_search_prs",
        };
        let mut duplicate = item.clone();
        duplicate.kind = "review".into();
        duplicate.reasons = vec!["review".into()];
        duplicate.source = "github_review_search";
        duplicate.updated_at = "2026-05-22T00:00:00Z".into();

        let items = dedupe_items(vec![item, duplicate]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "review");
        assert_eq!(items[0].reasons, vec!["review", "assigned"]);
        assert_eq!(items[0].source, "github_review_search");
        assert_eq!(items[0].updated_at, "2026-05-22T00:00:00Z");
    }
}
