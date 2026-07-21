//! Bounded native HTTP adapter for named Forgejo providers.

mod client;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::cli::{BINARY, GlobalFlags, IssueListArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;

pub(crate) use self::client::ForgejoClient;

#[derive(Debug, Serialize)]
struct AuthStatusPayload {
    provider: String,
    host: String,
    user: Option<String>,
    scopes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RepoViewPayload {
    provider: String,
    owner: String,
    name: String,
    url: String,
    default_branch: String,
    merge_methods_allowed: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct IssueListPayload {
    provider: String,
    items: Vec<IssueListItem>,
}

#[derive(Debug, Serialize)]
struct IssueListItem {
    number: u64,
    url: String,
    state: &'static str,
    title: String,
    labels: Vec<String>,
    author: Option<String>,
    assignees: Vec<String>,
}

pub(crate) fn run_auth_status(
    global: &GlobalFlags,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let client = ForgejoClient::from_global(global)?;
    client.discover_version()?;
    let user = client.authenticated_user()?;
    let payload = AuthStatusPayload {
        provider: client.name().to_string(),
        host: client.authority().to_string(),
        user: Some(user),
        scopes: Vec::new(),
    };
    Ok(emit_success(
        schema_version_for(BINARY, "auth.status", 1),
        payload,
        format,
        |payload| {
            println!(
                "logged in to {} as {} ({})",
                payload.host,
                payload.user.as_deref().unwrap_or("<unknown>"),
                payload.provider
            )
        },
    ))
}

pub(crate) fn run_repo_view(global: &GlobalFlags, format: OutputFormat) -> Result<i32, ForgeError> {
    let client = ForgejoClient::from_global(global)?;
    let (owner, repo) = repo_parts(global)?;
    client.discover_version()?;
    let raw = client.repo(&owner, &repo)?;
    let mut methods = Vec::new();
    if bool_field(&raw, "allow_squash_merge") {
        methods.push("squash");
    }
    if bool_field(&raw, "allow_merge_commits") {
        methods.push("merge");
    }
    if bool_field(&raw, "allow_rebase") || bool_field(&raw, "allow_rebase_explicit") {
        methods.push("rebase");
    }
    let payload = RepoViewPayload {
        provider: client.name().to_string(),
        owner: raw
            .pointer("/owner/login")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| missing("repo view", "owner.login"))?,
        name: required_string(&raw, "repo view", "name")?,
        url: required_string(&raw, "repo view", "html_url")?,
        default_branch: required_string(&raw, "repo view", "default_branch")?,
        merge_methods_allowed: methods,
    };
    Ok(emit_success(
        schema_version_for(BINARY, "repo.view", 1),
        payload,
        format,
        |payload| {
            println!(
                "{}/{} (default: {}, methods: {})\nurl: {}",
                payload.owner,
                payload.name,
                payload.default_branch,
                payload.merge_methods_allowed.join(","),
                payload.url
            )
        },
    ))
}

pub(crate) fn run_issue_list(
    global: &GlobalFlags,
    args: &IssueListArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let client = ForgejoClient::from_global(global)?;
    let (owner, repo) = repo_parts(global)?;
    client.discover_version()?;

    let limit = args.limit.max(1) as usize;
    let mut items = Vec::new();
    let mut page = 1u32;
    loop {
        let rows = client.issues(&owner, &repo, args, page)?;
        let row_count = rows.len();
        for raw in rows {
            if raw
                .get("pull_request")
                .is_some_and(|value| !value.is_null())
            {
                continue;
            }
            items.push(parse_issue(&raw)?);
            if items.len() >= limit {
                break;
            }
        }
        if items.len() >= limit || row_count < client::ISSUE_PAGE_SIZE as usize {
            break;
        }
        page = page.checked_add(1).ok_or_else(|| {
            ForgeError::software(error_schema(), "Forgejo issue pagination overflowed", None)
        })?;
    }
    items.truncate(limit);

    let payload = IssueListPayload {
        provider: client.name().to_string(),
        items,
    };
    Ok(emit_success(
        schema_version_for(BINARY, "issue.list", 1),
        payload,
        format,
        |payload| {
            for item in &payload.items {
                let labels = if item.labels.is_empty() {
                    String::new()
                } else {
                    format!(" {{{}}}", item.labels.join(","))
                };
                println!(
                    "#{} [{}]{} {} ({}) — {}",
                    item.number,
                    item.state,
                    labels,
                    item.title,
                    item.author.as_deref().unwrap_or("<unknown>"),
                    item.url
                );
            }
        },
    ))
}

pub(crate) fn repo_parts(global: &GlobalFlags) -> Result<(String, String), ForgeError> {
    let value = global.repo.as_deref().ok_or_else(|| {
        ForgeError::validation(
            error_schema(),
            "repo_invalid",
            "named Forgejo operations require --repo owner/name",
            None,
        )
    })?;
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    if owner.is_empty()
        || repo.is_empty()
        || parts.next().is_some()
        || !valid_repo_component(owner)
        || !valid_repo_component(repo)
    {
        return Err(ForgeError::validation(
            error_schema(),
            "repo_invalid",
            "Forgejo repository must use owner/name with safe path components",
            None,
        ));
    }
    Ok((owner.to_string(), repo.to_string()))
}

fn valid_repo_component(value: &str) -> bool {
    value.len() <= 255
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && value != "."
        && value != ".."
}

fn parse_issue(raw: &serde_json::Value) -> Result<IssueListItem, ForgeError> {
    let state = match raw.get("state").and_then(serde_json::Value::as_str) {
        Some("open") | Some("opened") => "open",
        Some("closed") => "closed",
        _ => return Err(missing("issue list", "state")),
    };
    Ok(IssueListItem {
        number: raw
            .get("number")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| missing("issue list", "number"))?,
        url: required_string(raw, "issue list", "html_url")?,
        state,
        title: required_string(raw, "issue list", "title")?,
        labels: login_or_name_list(raw, "labels", "name"),
        author: raw
            .pointer("/user/login")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        assignees: login_or_name_list(raw, "assignees", "login"),
    })
}

fn login_or_name_list(raw: &serde_json::Value, key: &str, field: &str) -> Vec<String> {
    raw.get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get(field).and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn bool_field(raw: &serde_json::Value, key: &str) -> bool {
    raw.get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn required_string(
    raw: &serde_json::Value,
    operation: &str,
    key: &str,
) -> Result<String, ForgeError> {
    raw.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| missing(operation, key))
}

fn missing(operation: &str, field: &str) -> ForgeError {
    ForgeError::software(
        error_schema(),
        format!("Forgejo {operation} response missing required field: {field}"),
        None,
    )
}

fn error_schema() -> String {
    schema_version_for(BINARY, "error", 1)
}
