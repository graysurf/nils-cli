//! `issue view` atom + shared issue JSON parser.
//!
//! Spec / ops: `cli.forge-cli.issue.view.v1`. Both backends emit a single JSON
//! object that we normalize into [`IssueViewPayload`]. The parser is `pub`
//! because `issue create / edit / close / reopen / comment` all re-fetch the
//! canonical view after their mutating call so the envelope reports the
//! post-action state.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload};
use crate::cli::{BINARY, GlobalFlags, IssueViewArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_state::normalize_state;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};
use crate::rate_limit::default_runner;

pub const SCHEMA: &str = "issue.view";
pub const SCHEMA_VERSION: u32 = 1;

const GH_JSON_FIELDS: &str = "number,url,state,title,labels,assignees,body";
const GH_JSON_FIELDS_WITH_COMMENTS: &str = "number,url,state,title,labels,assignees,body,comments";

/// One issue comment, normalized across providers.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IssueCommentSummary {
    pub url: String,
    pub author: String,
    pub created_at: String,
    pub body: String,
}

/// Envelope payload for `cli.forge-cli.issue.view.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IssueViewPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub state: &'static str,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    /// Populated only when the caller passed `--with-comments`; the empty
    /// vector is always serialized so the schema shape is stable.
    pub comments: Vec<IssueCommentSummary>,
}

pub fn run(
    global: &GlobalFlags,
    args: IssueViewArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    if global.is_local() {
        let runner = crate::local::LocalRunner::from_global(global)?;
        return run_with(&runner, global, args, format, git_remote_url);
    }
    let runner = default_runner();
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: IssueViewArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    let call = build_view_call_with(&ctx, args.id, args.with_comments);

    if global.dry_run {
        let payload = DryRunPayload::new(ctx.provider, &call);
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            payload,
            format,
            |p| println!("would run: {plan}", plan = p.plan.join(" ")),
        ));
    }

    let output = runner.run(&call)?;
    let mut payload = parse_view_output_with(&ctx, &output, args.with_comments)?;
    if args.with_comments && ctx.provider == Provider::GitLab {
        let comments_call = build_gitlab_notes_call(&ctx, &payload)?;
        let comments_output = runner.run(&comments_call)?;
        payload.comments = parse_gitlab_notes(&comments_output, &payload.url)?;
    }
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        payload,
        format,
        render_text,
    ))
}

/// View-only call for mutation ops that re-fetch after a write. Comments are
/// never requested here so the post-action refresh stays a single backend hop.
pub fn build_view_call(ctx: &ProviderContext, id: u64) -> BackendCall {
    build_view_call_with(ctx, id, false)
}

/// View call for mutation ops that need the post-action comment stream.
pub fn build_view_with_comments_call(ctx: &ProviderContext, id: u64) -> BackendCall {
    build_view_call_with(ctx, id, true)
}

pub fn fetch_view_with_comments<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    id: u64,
) -> Result<IssueViewPayload, ForgeError> {
    let output = runner.run(&build_view_with_comments_call(ctx, id))?;
    let mut payload = parse_view_output_with(ctx, &output, true)?;
    if ctx.provider == Provider::GitLab {
        let comments_call = build_gitlab_notes_call(ctx, &payload)?;
        let comments_output = runner.run(&comments_call)?;
        payload.comments = parse_gitlab_notes(&comments_output, &payload.url)?;
    }
    Ok(payload)
}

fn build_view_call_with(ctx: &ProviderContext, id: u64, with_comments: bool) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let gh_fields = if with_comments {
        GH_JSON_FIELDS_WITH_COMMENTS
    } else {
        GH_JSON_FIELDS
    };
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub | Provider::Local => vec![
            OsString::from("issue"),
            OsString::from("view"),
            OsString::from(id.to_string()),
            OsString::from("--json"),
            OsString::from(gh_fields),
        ],
        Provider::GitLab => vec![
            OsString::from("issue"),
            OsString::from("view"),
            OsString::from(id.to_string()),
            OsString::from("-F"),
            OsString::from("json"),
        ],
    };
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

/// Build the `glab api .../notes` call used by `--with-comments` on GitLab.
/// Both the project path AND the GitLab host are derived from the issue's
/// `web_url` so we do not need the user to pass `--repo` or `--remote`. This
/// also avoids the bug where forcing `--provider gitlab` makes
/// `ProviderContext::host` default to `gitlab.com` even when the repo lives
/// on a different GitLab instance.
fn build_gitlab_notes_call(
    _ctx: &ProviderContext,
    view: &IssueViewPayload,
) -> Result<BackendCall, ForgeError> {
    let project_path = gitlab_project_path_from_url(&view.url).ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "unable to derive GitLab project path from issue web_url",
            Some(format!("url={}", view.url)),
        )
    })?;
    let host = gitlab_host_from_url(&view.url).ok_or_else(|| {
        ForgeError::software(
            schema_err(),
            "unable to derive GitLab host from issue web_url",
            Some(format!("url={}", view.url)),
        )
    })?;
    let encoded = project_path.replace('/', "%2F");
    let path = format!(
        "projects/{encoded}/issues/{iid}/notes?per_page=100&order_by=created_at&sort=asc",
        iid = view.number,
    );
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

/// Extract the host (`<host>`) from an `https?://<host>/...` URL. Used
/// alongside [`gitlab_project_path_from_url`] to wire `glab api --hostname`
/// against the actual GitLab instance hosting the issue, not whatever
/// `ProviderContext::host` defaulted to when `--provider gitlab` was forced.
fn gitlab_host_from_url(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest)?;
    let host = after_scheme.split_once('/').map(|(host, _)| host)?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

pub fn parse_view_output(
    ctx: &ProviderContext,
    output: &BackendSuccess,
) -> Result<IssueViewPayload, ForgeError> {
    parse_view_output_with(ctx, output, false)
}

fn parse_view_output_with(
    ctx: &ProviderContext,
    output: &BackendSuccess,
    with_comments: bool,
) -> Result<IssueViewPayload, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "issue view JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    match ctx.provider {
        Provider::GitHub | Provider::Local => parse_github(&value, ctx, with_comments),
        Provider::GitLab => parse_gitlab(&value, ctx),
    }
}

fn parse_github(
    value: &serde_json::Value,
    ctx: &ProviderContext,
    with_comments: bool,
) -> Result<IssueViewPayload, ForgeError> {
    let state = normalize_state(
        value.get("state").and_then(|v| v.as_str()).unwrap_or(""),
        ctx.provider,
    )?;
    let comments = if with_comments {
        github_comments(value)
    } else {
        Vec::new()
    };
    Ok(IssueViewPayload {
        provider: ctx.provider.as_str(),
        number: value
            .get("number")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| missing("number"))?,
        url: required_str(value, "url")?,
        state,
        title: required_str(value, "title")?,
        body: value
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        labels: github_name_list(value, "labels"),
        assignees: github_assignees(value),
        comments,
    })
}

fn parse_gitlab(
    value: &serde_json::Value,
    ctx: &ProviderContext,
) -> Result<IssueViewPayload, ForgeError> {
    let state = normalize_state(
        value.get("state").and_then(|v| v.as_str()).unwrap_or(""),
        ctx.provider,
    )?;
    Ok(IssueViewPayload {
        provider: ctx.provider.as_str(),
        number: value
            .get("iid")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| missing("iid"))?,
        url: required_str(value, "web_url")?,
        state,
        title: required_str(value, "title")?,
        body: value
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        labels: gitlab_label_list(value),
        assignees: gitlab_assignees(value),
        comments: Vec::new(),
    })
}

fn github_comments(value: &serde_json::Value) -> Vec<IssueCommentSummary> {
    value
        .get("comments")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|item| IssueCommentSummary {
                    url: item
                        .get("url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    author: item
                        .get("author")
                        .and_then(|a| a.get("login"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    created_at: item
                        .get("createdAt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    body: item
                        .get("body")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse the response from `glab api .../notes` (a JSON array of note
/// objects). `issue_url` is the issue's `web_url`, used to construct each
/// comment URL (`<issue_url>#note_<note_id>`) since notes don't carry their
/// own HTML URL in the REST response.
fn parse_gitlab_notes(
    output: &BackendSuccess,
    issue_url: &str,
) -> Result<Vec<IssueCommentSummary>, ForgeError> {
    let trimmed = output.stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    // `glab api --paginate` concatenates JSON arrays back-to-back when the
    // result spans pages. Try one-shot parse first, then fall back to
    // splitting on `][`.
    let chunks = if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        vec![value]
    } else {
        let mut acc = Vec::new();
        for raw in trimmed.split("][") {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            let normalized = if raw.starts_with('[') {
                raw.to_string()
            } else {
                format!("[{raw}")
            };
            let normalized = if normalized.ends_with(']') {
                normalized
            } else {
                format!("{normalized}]")
            };
            let value: serde_json::Value = serde_json::from_str(&normalized).map_err(|e| {
                ForgeError::software(
                    schema_err(),
                    "gitlab notes response is invalid JSON",
                    Some(e.to_string()),
                )
            })?;
            acc.push(value);
        }
        acc
    };
    let mut out = Vec::new();
    for chunk in chunks {
        let items = chunk
            .as_array()
            .map(|arr| arr.to_vec())
            .unwrap_or_else(|| vec![chunk]);
        for item in items {
            // GitLab's REST `/notes` includes both user-authored comments and
            // synthetic system notes (e.g. label changes). plan-issue and
            // other audit consumers only want real comments.
            if item
                .get("system")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                continue;
            }
            let id = item
                .get("id")
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_default();
            let url = format!("{issue_url}#note_{id}");
            out.push(IssueCommentSummary {
                url,
                author: item
                    .get("author")
                    .and_then(|a| a.get("username"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                created_at: item
                    .get("created_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                body: item
                    .get("body")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    Ok(out)
}

/// Extract the GitLab project path (group/project, possibly nested) from an
/// issue's `web_url`. Returns `None` when the URL does not look like a GitLab
/// issue URL. Accepts `/-/issues/<iid>`, `/issues/<iid>`, and the newer
/// `/-/work_items/<iid>` shape that GitLab returns for projects that have
/// migrated issues to the unified Work Items API.
fn gitlab_project_path_from_url(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let path = after_scheme.split_once('/').map(|(_, rest)| rest)?;
    let path = path.trim_end_matches('/');
    let project_path = if let Some(idx) = path.find("/-/issues/") {
        &path[..idx]
    } else if let Some(idx) = path.find("/-/work_items/") {
        &path[..idx]
    } else if let Some(idx) = path.find("/issues/") {
        &path[..idx]
    } else {
        return None;
    };
    if project_path.is_empty() {
        None
    } else {
        Some(project_path.to_string())
    }
}

fn github_name_list(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.get("name")
                        .and_then(|n| n.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn github_assignees(value: &serde_json::Value) -> Vec<String> {
    value
        .get("assignees")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.get("login")
                        .and_then(|n| n.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn gitlab_label_list(value: &serde_json::Value) -> Vec<String> {
    value
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.as_str().map(str::to_string).or_else(|| {
                        item.get("name")
                            .and_then(|n| n.as_str())
                            .map(str::to_string)
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn gitlab_assignees(value: &serde_json::Value) -> Vec<String> {
    value
        .get("assignees")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.get("username")
                        .and_then(|n| n.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn required_str(value: &serde_json::Value, key: &str) -> Result<String, ForgeError> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing(key))
}

fn missing(key: &str) -> ForgeError {
    ForgeError::software(
        schema_err(),
        format!("missing required field '{key}' in issue view JSON"),
        None,
    )
}

pub(super) fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &IssueViewPayload) {
    println!(
        "#{number} [{state}] {title}\n  {url}",
        number = payload.number,
        state = payload.state,
        title = payload.title,
        url = payload.url,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::DetectionSource;
    use pretty_assertions::assert_eq;

    fn ctx(p: Provider) -> ProviderContext {
        ProviderContext {
            provider: p,
            host: "example.com".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    #[test]
    fn build_view_call_github_uses_json_fields() {
        let call = build_view_call(&ctx(Provider::GitHub), 5);
        let plan = call.plan_argv();
        assert_eq!(plan[1..3], ["issue".to_string(), "view".to_string()]);
        assert!(plan.iter().any(|s| s == "--json"));
    }

    #[test]
    fn parse_github_open_issue() {
        let output = BackendSuccess {
            stdout: r#"{"number":7,"url":"u","state":"OPEN","title":"t","body":"b","labels":[{"name":"a"},{"name":"b"}],"assignees":[{"login":"x"}]}"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitHub), &output).unwrap();
        assert_eq!(p.state, "open");
        assert_eq!(p.labels, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(p.assignees, vec!["x".to_string()]);
        assert_eq!(p.body, "b");
    }

    #[test]
    fn parse_gitlab_opened_normalises_to_open() {
        let output = BackendSuccess {
            stdout: r#"{"iid":3,"web_url":"u","state":"opened","title":"t","description":"d","labels":["a"],"assignees":[{"username":"y"}]}"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitLab), &output).unwrap();
        assert_eq!(p.number, 3);
        assert_eq!(p.state, "open");
        assert_eq!(p.labels, vec!["a".to_string()]);
        assert_eq!(p.assignees, vec!["y".to_string()]);
        assert_eq!(p.body, "d");
    }

    #[test]
    fn parse_gitlab_closed_normalises_to_closed() {
        let output = BackendSuccess {
            stdout: r#"{"iid":3,"web_url":"u","state":"closed","title":"t","description":"","labels":[],"assignees":[]}"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitLab), &output).unwrap();
        assert_eq!(p.state, "closed");
    }

    #[test]
    fn build_view_call_github_includes_comments_field_when_requested() {
        let plain = build_view_call_with(&ctx(Provider::GitHub), 5, false).plan_argv();
        let json_plain = plain
            .iter()
            .position(|s| s == "--json")
            .map(|i| plain[i + 1].as_str())
            .unwrap();
        assert!(
            !json_plain.contains("comments"),
            "default view should not request comments"
        );

        let with = build_view_call_with(&ctx(Provider::GitHub), 5, true).plan_argv();
        let json_with = with
            .iter()
            .position(|s| s == "--json")
            .map(|i| with[i + 1].as_str())
            .unwrap();
        assert!(
            json_with.contains("comments"),
            "--with-comments must extend --json"
        );
    }

    #[test]
    fn build_view_call_gitlab_does_not_change_for_with_comments() {
        let plain = build_view_call_with(&ctx(Provider::GitLab), 5, false).plan_argv();
        let with = build_view_call_with(&ctx(Provider::GitLab), 5, true).plan_argv();
        assert_eq!(
            plain, with,
            "GitLab view call shape is identical; comments come from a follow-up api call"
        );
    }

    #[test]
    fn parse_github_with_comments_normalises_author_and_url() {
        let output = BackendSuccess {
            stdout: r#"{
                "number":7,"url":"https://github.com/o/r/issues/7","state":"OPEN","title":"t","body":"b",
                "labels":[],"assignees":[],
                "comments":[
                    {"author":{"login":"alice"},"body":"hi","url":"https://github.com/o/r/issues/7#issuecomment-1","createdAt":"2025-01-01T00:00:00Z"},
                    {"author":{"login":"bob"},"body":"hello","url":"https://github.com/o/r/issues/7#issuecomment-2","createdAt":"2025-01-02T00:00:00Z"}
                ]
            }"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output_with(&ctx(Provider::GitHub), &output, true).unwrap();
        assert_eq!(p.comments.len(), 2);
        assert_eq!(p.comments[0].author, "alice");
        assert_eq!(p.comments[0].body, "hi");
        assert_eq!(
            p.comments[0].url,
            "https://github.com/o/r/issues/7#issuecomment-1"
        );
        assert_eq!(p.comments[0].created_at, "2025-01-01T00:00:00Z");
    }

    #[test]
    fn parse_github_without_with_comments_keeps_comments_empty() {
        let output = BackendSuccess {
            stdout: r#"{"number":7,"url":"u","state":"OPEN","title":"t","body":"b","labels":[],"assignees":[],"comments":[{"author":{"login":"a"},"body":"x","url":"u","createdAt":"t"}]}"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitHub), &output).unwrap();
        assert!(
            p.comments.is_empty(),
            "without --with-comments the field stays empty"
        );
    }

    #[test]
    fn parse_gitlab_notes_filters_system_notes_and_constructs_url() {
        let output = BackendSuccess {
            stdout: r#"[
                {"id":1,"body":"first","author":{"username":"alice"},"created_at":"2025-01-01T00:00:00Z","system":false},
                {"id":2,"body":"added label","author":{"username":"alice"},"created_at":"2025-01-01T00:01:00Z","system":true},
                {"id":3,"body":"second","author":{"username":"bob"},"created_at":"2025-01-02T00:00:00Z","system":false}
            ]"#.into(),
            stderr: String::new(),
        };
        let comments = parse_gitlab_notes(
            &output,
            "https://gitlab.com/graysury/nils-cli-gitlab-sandbox/-/issues/4",
        )
        .unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].author, "alice");
        assert_eq!(comments[0].body, "first");
        assert_eq!(
            comments[0].url,
            "https://gitlab.com/graysury/nils-cli-gitlab-sandbox/-/issues/4#note_1"
        );
        assert_eq!(comments[1].author, "bob");
        assert_eq!(
            comments[1].url,
            "https://gitlab.com/graysury/nils-cli-gitlab-sandbox/-/issues/4#note_3"
        );
    }

    #[test]
    fn parse_gitlab_notes_handles_empty_array() {
        let output = BackendSuccess {
            stdout: "[]".into(),
            stderr: String::new(),
        };
        let comments =
            parse_gitlab_notes(&output, "https://gitlab.example.com/g/p/-/issues/1").unwrap();
        assert!(comments.is_empty());
    }

    #[test]
    fn parse_gitlab_notes_handles_concatenated_pages() {
        let output = BackendSuccess {
            stdout: r#"[{"id":1,"body":"a","author":{"username":"u"},"created_at":"t","system":false}][{"id":2,"body":"b","author":{"username":"u"},"created_at":"t","system":false}]"#.into(),
            stderr: String::new(),
        };
        let comments =
            parse_gitlab_notes(&output, "https://gitlab.example.com/g/p/-/issues/1").unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].body, "a");
        assert_eq!(comments[1].body, "b");
    }

    #[test]
    fn gitlab_host_from_url_extracts_host_segment() {
        assert_eq!(
            gitlab_host_from_url(
                "https://gitlab.com/graysury/nils-cli-gitlab-sandbox/-/work_items/6"
            )
            .as_deref(),
            Some("gitlab.com"),
        );
        assert_eq!(
            gitlab_host_from_url("https://gitlab.com/group/sub/project/-/issues/12").as_deref(),
            Some("gitlab.com"),
        );
        assert!(gitlab_host_from_url("/no-scheme/path/-/issues/1").is_none());
        assert!(gitlab_host_from_url("https://no-path-here").is_none());
    }

    #[test]
    fn gitlab_project_path_from_url_extracts_nested_groups() {
        assert_eq!(
            gitlab_project_path_from_url(
                "https://gitlab.com/graysury/nils-cli-gitlab-sandbox/-/issues/4"
            )
            .as_deref(),
            Some("graysury/nils-cli-gitlab-sandbox")
        );
        assert_eq!(
            gitlab_project_path_from_url(
                "https://gitlab.com/graysury/nils-cli-gitlab-sandbox/-/work_items/6"
            )
            .as_deref(),
            Some("graysury/nils-cli-gitlab-sandbox"),
            "GitLab projects migrated to the Work Items API surface issues at /-/work_items/<iid>",
        );
        assert_eq!(
            gitlab_project_path_from_url("https://gitlab.com/group/sub/project/-/issues/12")
                .as_deref(),
            Some("group/sub/project")
        );
        assert_eq!(
            gitlab_project_path_from_url("https://gitlab.example.com/g/p/issues/1").as_deref(),
            Some("g/p"),
            "fallback for hosts that elide the `/-/` segment",
        );
        assert!(
            gitlab_project_path_from_url("https://gitlab.example.com/not-an-issue-url").is_none()
        );
    }
}
