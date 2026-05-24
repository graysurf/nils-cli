//! `pr comments` atom — read-side companion to `pr comment`.
//!
//! Spec / ops: `cli.forge-cli.pr.comments.v1`. Returns the issue-style comment
//! stream attached to a PR/MR (`/repos/<owner>/<repo>/issues/<n>/comments` on
//! GitHub, `/projects/<encoded>/merge_requests/<iid>/notes` on GitLab). System
//! notes (label/assignee changes) are filtered out so callers only see
//! user-authored entries.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{
    BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload, ProcessRunner,
};
use crate::cli::{BINARY, GlobalFlags, PrCommentsArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_view;
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};

const SCHEMA: &str = "pr.comments";
const SCHEMA_VERSION: u32 = 1;

/// One PR / MR comment, normalized across providers.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrCommentSummary {
    pub url: String,
    pub author: String,
    pub created_at: String,
    pub body: String,
}

/// Envelope payload for `cli.forge-cli.pr.comments.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrCommentsPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub comments: Vec<PrCommentSummary>,
}

pub fn run(
    global: &GlobalFlags,
    args: PrCommentsArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = ProcessRunner;
    run_with(&runner, global, args, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    args: PrCommentsArgs,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;

    // Resolve the canonical PR/MR URL first; both providers need the
    // owner/repo (or group/project) segment for the comments API path, and
    // re-using `pr view` keeps URL parsing in one place.
    let view_output = runner.run(&pr_view::build_view_call(&ctx, &args.id.to_string()))?;
    let view = pr_view::parse_view_output(&ctx, &view_output)?;

    let comments_call = build_comments_call(&ctx, &view)?;

    if global.dry_run {
        let payload = DryRunPayload::new(ctx.provider, &comments_call);
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            payload,
            format,
            |p| println!("would run: {plan}", plan = p.plan.join(" ")),
        ));
    }

    let comments_output = runner.run(&comments_call)?;
    let comments = match ctx.provider {
        Provider::GitHub => parse_github_comments(&comments_output)?,
        Provider::GitLab => parse_gitlab_notes(&comments_output, &view.url)?,
    };

    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        PrCommentsPayload {
            provider: ctx.provider.as_str(),
            number: view.number,
            url: view.url,
            comments,
        },
        format,
        render_text,
    ))
}

fn build_comments_call(
    ctx: &ProviderContext,
    view: &pr_view::PrViewPayload,
) -> Result<BackendCall, ForgeError> {
    match ctx.provider {
        Provider::GitHub => {
            let slug = github_repo_slug_from_url(&view.url).ok_or_else(|| {
                ForgeError::software(
                    schema_err(),
                    "unable to derive GitHub owner/repo from PR url",
                    Some(format!("url={}", view.url)),
                )
            })?;
            let path = format!(
                "repos/{slug}/issues/{n}/comments?per_page=100",
                n = view.number,
            );
            Ok(BackendCall::new(
                BackendProgram::Gh,
                [
                    OsString::from("api"),
                    OsString::from("--paginate"),
                    OsString::from(path),
                ],
            ))
        }
        Provider::GitLab => {
            let project = gitlab_project_path_from_url(&view.url).ok_or_else(|| {
                ForgeError::software(
                    schema_err(),
                    "unable to derive GitLab project path from MR web_url",
                    Some(format!("url={}", view.url)),
                )
            })?;
            let encoded = project.replace('/', "%2F");
            let path = format!(
                "projects/{encoded}/merge_requests/{iid}/notes?per_page=100&order_by=created_at&sort=asc",
                iid = view.number,
            );
            Ok(BackendCall::new(
                BackendProgram::Glab,
                [
                    OsString::from("api"),
                    OsString::from("--paginate"),
                    OsString::from("--hostname"),
                    OsString::from(ctx.host.as_str()),
                    OsString::from(path),
                ],
            ))
        }
    }
}

fn parse_github_comments(output: &BackendSuccess) -> Result<Vec<PrCommentSummary>, ForgeError> {
    let chunks = split_concatenated_arrays(&output.stdout)?;
    let mut out = Vec::new();
    for chunk in chunks {
        let items = chunk.as_array().cloned().unwrap_or_else(|| vec![chunk]);
        for item in items {
            out.push(PrCommentSummary {
                url: item
                    .get("html_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                author: item
                    .get("user")
                    .and_then(|u| u.get("login"))
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

fn parse_gitlab_notes(
    output: &BackendSuccess,
    mr_url: &str,
) -> Result<Vec<PrCommentSummary>, ForgeError> {
    let chunks = split_concatenated_arrays(&output.stdout)?;
    let mut out = Vec::new();
    for chunk in chunks {
        let items = chunk.as_array().cloned().unwrap_or_else(|| vec![chunk]);
        for item in items {
            // Skip synthetic system notes (label changes, assignee changes,
            // etc.) — only user-authored comments are useful to consumers.
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
            out.push(PrCommentSummary {
                url: format!("{mr_url}#note_{id}"),
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

/// `gh api --paginate` / `glab api --paginate` concatenate JSON arrays
/// back-to-back when results span pages. Parse the trimmed stdout as one or
/// more arrays.
fn split_concatenated_arrays(stdout: &str) -> Result<Vec<serde_json::Value>, ForgeError> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Ok(vec![value]);
    }
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
                "comments response is invalid JSON",
                Some(e.to_string()),
            )
        })?;
        acc.push(value);
    }
    Ok(acc)
}

/// Extract `owner/repo` from a GitHub PR URL like
/// `https://github.com/owner/repo/pull/<n>`.
fn github_repo_slug_from_url(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let path = after_scheme.split_once('/').map(|(_, rest)| rest)?;
    let pull_idx = path.find("/pull/").or_else(|| path.find("/issues/"))?;
    let slug = &path[..pull_idx];
    let mut parts = slug.splitn(3, '/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// Extract the GitLab project path (group[/subgroup]/project) from an MR's
/// `web_url` like `https://gitlab.example.com/group/project/-/merge_requests/<iid>`.
fn gitlab_project_path_from_url(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let path = after_scheme.split_once('/').map(|(_, rest)| rest)?;
    let path = path.trim_end_matches('/');
    let idx = path
        .find("/-/merge_requests/")
        .or_else(|| path.find("/merge_requests/"))?;
    let project_path = &path[..idx];
    if project_path.is_empty() {
        None
    } else {
        Some(project_path.to_string())
    }
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrCommentsPayload) {
    println!(
        "{provider} #{number} ({n} comments)\n  {url}",
        provider = payload.provider,
        number = payload.number,
        n = payload.comments.len(),
        url = payload.url,
    );
    for comment in &payload.comments {
        let body_first_line = comment.body.lines().next().unwrap_or("");
        println!(
            "  - [{at}] {author}: {body}",
            at = comment.created_at,
            author = comment.author,
            body = body_first_line,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn github_repo_slug_from_pr_and_issue_urls() {
        assert_eq!(
            github_repo_slug_from_url("https://github.com/sympoies/nils-cli/pull/123").as_deref(),
            Some("sympoies/nils-cli")
        );
        assert_eq!(
            github_repo_slug_from_url("https://github.com/owner/repo/issues/9").as_deref(),
            Some("owner/repo")
        );
        assert!(
            github_repo_slug_from_url("https://github.com/just-owner").is_none(),
            "missing /pull/ or /issues/ segment must yield None"
        );
    }

    #[test]
    fn gitlab_project_path_from_mr_url_handles_nested_groups() {
        assert_eq!(
            gitlab_project_path_from_url(
                "https://gitlab.gamania.com/terrylin/agent-runtime-testing/-/merge_requests/12"
            )
            .as_deref(),
            Some("terrylin/agent-runtime-testing")
        );
        assert_eq!(
            gitlab_project_path_from_url("https://gitlab.com/group/sub/project/-/merge_requests/3")
                .as_deref(),
            Some("group/sub/project")
        );
        assert!(
            gitlab_project_path_from_url("https://gitlab.example.com/g/p/-/issues/1").is_none()
        );
    }

    #[test]
    fn parse_github_comments_extracts_user_author_and_html_url() {
        let output = BackendSuccess {
            stdout: r#"[
                {"user":{"login":"alice"},"body":"hi","html_url":"https://github.com/o/r/pull/1#issuecomment-100","created_at":"2025-01-01T00:00:00Z"},
                {"user":{"login":"bob"},"body":"hello","html_url":"https://github.com/o/r/pull/1#issuecomment-101","created_at":"2025-01-02T00:00:00Z"}
            ]"#.into(),
            stderr: String::new(),
        };
        let comments = parse_github_comments(&output).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].author, "alice");
        assert_eq!(
            comments[0].url,
            "https://github.com/o/r/pull/1#issuecomment-100"
        );
        assert_eq!(comments[0].body, "hi");
    }

    #[test]
    fn parse_github_comments_handles_concatenated_pages() {
        let output = BackendSuccess {
            stdout: r#"[{"user":{"login":"a"},"body":"x","html_url":"u","created_at":"t"}][{"user":{"login":"b"},"body":"y","html_url":"u","created_at":"t"}]"#.into(),
            stderr: String::new(),
        };
        let comments = parse_github_comments(&output).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].body, "x");
        assert_eq!(comments[1].body, "y");
    }

    #[test]
    fn parse_gitlab_notes_skips_system_notes_and_constructs_urls() {
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
            "https://gitlab.gamania.com/terrylin/agent-runtime-testing/-/merge_requests/12",
        )
        .unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].author, "alice");
        assert_eq!(
            comments[0].url,
            "https://gitlab.gamania.com/terrylin/agent-runtime-testing/-/merge_requests/12#note_1"
        );
        assert_eq!(comments[1].author, "bob");
    }

    #[test]
    fn split_concatenated_arrays_returns_empty_for_empty_stdout() {
        let chunks = split_concatenated_arrays("   \n").unwrap();
        assert!(chunks.is_empty());
    }
}
