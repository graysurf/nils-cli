//! `pr view` atom.
//!
//! Spec / ops: `cli.forge-cli.pr.view.v1`. Accepts either a numeric id or a
//! branch name. Numeric ids are passed through to the backend directly;
//! branch names are resolved upstream via `gh pr view <branch>` (GitHub
//! handles this natively) or `glab mr list --source-branch <branch>` (which
//! returns a JSON array whose first entry's `iid` is taken).

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{
    BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload, ProcessRunner,
};
use crate::cli::{BINARY, GlobalFlags};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::pr_state::{
    normalize_mergeable_github, normalize_mergeable_gitlab, normalize_state,
};
use crate::provider::{Provider, ProviderContext, detect, git_remote_url};

const SCHEMA: &str = "pr.view";
const SCHEMA_VERSION: u32 = 1;

const GH_JSON_FIELDS: &str = "number,url,state,isDraft,title,headRefName,baseRefName,mergeable,mergedAt,mergeCommit,labels,body";

/// Envelope payload for `cli.forge-cli.pr.view.v1`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PrViewPayload {
    pub provider: &'static str,
    pub number: u64,
    pub url: String,
    pub state: &'static str,
    pub draft: bool,
    pub title: String,
    pub head: String,
    pub base: String,
    pub mergeable: &'static str,
    pub merged_at: Option<String>,
    /// Commit SHA of the merge commit when the PR/MR is merged. Always
    /// emitted (the field defaults to `null` for non-merged PRs); GitHub
    /// derives it from `mergeCommit.oid`, GitLab reads the top-level
    /// `merge_commit_sha`.
    pub merge_commit_sha: Option<String>,
    pub labels: Vec<String>,
    /// PR/MR description body. Additive: GitHub reads `body`, GitLab reads
    /// `description`; `null` when the provider response omits the field
    /// (callers that re-parse view JSON with narrower field lists, e.g.
    /// `pr ready`, keep working).
    pub body: Option<String>,
}

pub fn run(global: &GlobalFlags, id: String, format: OutputFormat) -> Result<i32, ForgeError> {
    if global.is_local() {
        let runner = crate::local::LocalRunner::from_global(global)?;
        return run_with(&runner, global, &id, format, git_remote_url);
    }
    let runner = ProcessRunner;
    run_with(&runner, global, &id, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    id: &str,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;

    // GitLab cannot resolve a branch name in a single view call; fan out to
    // `mr list --source-branch <branch>` first.
    let resolved_id = if id.chars().all(|c| c.is_ascii_digit()) {
        id.to_string()
    } else {
        resolve_branch_id(runner, &ctx, id)?
    };

    let call = build_view_call(&ctx, &resolved_id);

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
    let payload = parse_view_output(&ctx, &output)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        payload,
        format,
        render_text,
    ))
}

/// Macro-facing entry point: fetch one PR/MR by number and return the typed
/// view payload (including `body` on providers that model it). Used by
/// `pr deliver` to inspect and re-validate an adopted PR.
pub(crate) fn compute<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    number: u64,
) -> Result<PrViewPayload, ForgeError> {
    let call = build_view_call(ctx, &number.to_string());
    let output = runner.run(&call)?;
    parse_view_output(ctx, &output)
}

pub(crate) fn build_view_call(ctx: &ProviderContext, id: &str) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = match ctx.provider {
        Provider::GitHub | Provider::Local => vec![
            OsString::from("pr"),
            OsString::from("view"),
            OsString::from(id),
            OsString::from("--json"),
            OsString::from(GH_JSON_FIELDS),
        ],
        Provider::GitLab => vec![
            OsString::from("mr"),
            OsString::from("view"),
            OsString::from(id),
            OsString::from("-F"),
            OsString::from("json"),
        ],
    };
    ctx.push_repo_override(&mut argv);
    BackendCall::new(program, argv)
}

fn resolve_branch_id<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
    branch: &str,
) -> Result<String, ForgeError> {
    match ctx.provider {
        Provider::GitHub | Provider::Local => Ok(branch.to_string()),
        Provider::GitLab => {
            let mut argv: Vec<OsString> = vec![
                OsString::from("mr"),
                OsString::from("list"),
                OsString::from("--source-branch"),
                OsString::from(branch),
                OsString::from("-F"),
                OsString::from("json"),
            ];
            ctx.push_repo_override(&mut argv);
            let call = BackendCall::new(BackendProgram::Glab, argv);
            let output = runner.run(&call)?;
            extract_first_iid(&output.stdout).ok_or_else(|| {
                ForgeError::software(
                    schema_err(),
                    format!("no GitLab MR matches source-branch '{branch}'"),
                    Some(format!("stdout={:?}", output.stdout)),
                )
            })
        }
    }
}

fn extract_first_iid(stdout: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let iid = value
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("iid"))
        .and_then(|v| v.as_u64())?;
    Some(iid.to_string())
}

pub fn parse_view_output(
    ctx: &ProviderContext,
    output: &BackendSuccess,
) -> Result<PrViewPayload, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(schema_err(), "pr view JSON is invalid", Some(e.to_string()))
    })?;
    match ctx.provider {
        Provider::GitHub | Provider::Local => parse_github(&value, ctx),
        Provider::GitLab => parse_gitlab(&value, ctx),
    }
}

fn parse_github(
    value: &serde_json::Value,
    ctx: &ProviderContext,
) -> Result<PrViewPayload, ForgeError> {
    let raw_state = value.get("state").and_then(|v| v.as_str()).unwrap_or("");
    let merged_at = value
        .get("mergedAt")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    let state = if merged_at.is_some() {
        "merged"
    } else {
        normalize_state(raw_state, ctx.provider)?
    };
    let merge_commit_sha = value
        .get("mergeCommit")
        .and_then(|mc| mc.get("oid"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    let labels = github_label_names(value);
    Ok(PrViewPayload {
        provider: ctx.provider.as_str(),
        number: value
            .get("number")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| missing("number"))?,
        url: required_str(value, "url")?,
        state,
        draft: value
            .get("isDraft")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        title: required_str(value, "title")?,
        head: required_str(value, "headRefName")?,
        base: required_str(value, "baseRefName")?,
        mergeable: normalize_mergeable_github(value.get("mergeable").and_then(|v| v.as_str())),
        merged_at,
        merge_commit_sha,
        labels,
        body: value
            .get("body")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

fn parse_gitlab(
    value: &serde_json::Value,
    ctx: &ProviderContext,
) -> Result<PrViewPayload, ForgeError> {
    let raw_state = value.get("state").and_then(|v| v.as_str()).unwrap_or("");
    let state = normalize_state(raw_state, ctx.provider)?;
    let merged_at = value
        .get("merged_at")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    let draft = value
        .get("draft")
        .and_then(|v| v.as_bool())
        .or_else(|| value.get("work_in_progress").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    let merge_commit_sha = value
        .get("merge_commit_sha")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    let labels = gitlab_label_names(value);
    Ok(PrViewPayload {
        provider: ctx.provider.as_str(),
        number: value
            .get("iid")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| missing("iid"))?,
        url: required_str(value, "web_url")?,
        state,
        draft,
        title: required_str(value, "title")?,
        head: required_str(value, "source_branch")?,
        base: required_str(value, "target_branch")?,
        mergeable: normalize_mergeable_gitlab(value.get("merge_status").and_then(|v| v.as_str())),
        merged_at,
        merge_commit_sha,
        labels,
        body: value
            .get("description")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

fn github_label_names(value: &serde_json::Value) -> Vec<String> {
    value
        .get("labels")
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

fn gitlab_label_names(value: &serde_json::Value) -> Vec<String> {
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
        format!("missing required field '{key}' in pr view JSON"),
        None,
    )
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn render_text(payload: &PrViewPayload) {
    println!(
        "#{number} [{state}{draft}] {title}\n  {url}",
        number = payload.number,
        state = payload.state,
        draft = if payload.draft { ",draft" } else { "" },
        title = payload.title,
        url = payload.url,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::DetectionSource;
    use pretty_assertions::assert_eq;

    fn ctx(provider: Provider) -> ProviderContext {
        ProviderContext {
            provider,
            host: "example.com".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    #[test]
    fn build_view_call_github_uses_json_fields() {
        let call = build_view_call(&ctx(Provider::GitHub), "5");
        let plan = call.plan_argv();
        assert_eq!(plan[1..3], ["pr".to_string(), "view".to_string()]);
        let json_idx = plan.iter().position(|s| s == "--json").unwrap();
        assert!(plan[json_idx + 1].contains("mergeable"));
    }

    #[test]
    fn build_view_call_gitlab_uses_f_json() {
        let call = build_view_call(&ctx(Provider::GitLab), "9");
        let plan = call.plan_argv();
        assert_eq!(plan[1..3], ["mr".to_string(), "view".to_string()]);
        assert!(plan.iter().any(|s| s == "-F"));
    }

    #[test]
    fn parse_github_open_pr() {
        let output = BackendSuccess {
            stdout: r#"{"number":5,"url":"u","state":"OPEN","isDraft":true,"title":"t","headRefName":"feat/x","baseRefName":"main","mergeable":"MERGEABLE","mergedAt":null,"labels":[{"name":"a"},{"name":"b"}]}"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitHub), &output).unwrap();
        assert_eq!(p.state, "open");
        assert!(p.draft);
        assert_eq!(p.mergeable, "yes");
        assert_eq!(p.merged_at, None);
        assert_eq!(p.labels, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn parse_github_merged_pr_overrides_state() {
        // GitHub keeps state=CLOSED on merged PRs; mergedAt is the canonical
        // signal so we promote to "merged".
        let output = BackendSuccess {
            stdout: r#"{"number":5,"url":"u","state":"CLOSED","isDraft":false,"title":"t","headRefName":"feat/x","baseRefName":"main","mergeable":"UNKNOWN","mergedAt":"2026-05-19T10:00:00Z","labels":[]}"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitHub), &output).unwrap();
        assert_eq!(p.state, "merged");
        assert_eq!(p.merged_at.as_deref(), Some("2026-05-19T10:00:00Z"));
    }

    #[test]
    fn parse_gitlab_opened_normalises_to_open() {
        let output = BackendSuccess {
            stdout: r#"{"iid":7,"web_url":"u","state":"opened","draft":true,"title":"t","source_branch":"feat/x","target_branch":"main","merge_status":"can_be_merged","labels":["x","y"]}"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitLab), &output).unwrap();
        assert_eq!(p.state, "open");
        assert!(p.draft);
        assert_eq!(p.mergeable, "yes");
        assert_eq!(p.labels, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn parse_github_extracts_body_and_defaults_to_none_when_absent() {
        let with_body = BackendSuccess {
            stdout: r###"{"number":5,"url":"u","state":"OPEN","isDraft":false,"title":"t","headRefName":"feat/x","baseRefName":"main","mergeable":"MERGEABLE","mergedAt":null,"labels":[],"body":"## Summary\nx"}"###.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitHub), &with_body).unwrap();
        assert_eq!(p.body.as_deref(), Some("## Summary\nx"));

        // Narrower field lists (e.g. pr ready's re-fetch) omit `body`.
        let without_body = BackendSuccess {
            stdout: r#"{"number":5,"url":"u","state":"OPEN","isDraft":false,"title":"t","headRefName":"feat/x","baseRefName":"main","mergeable":"MERGEABLE","mergedAt":null,"labels":[]}"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitHub), &without_body).unwrap();
        assert_eq!(p.body, None);
    }

    #[test]
    fn parse_gitlab_maps_description_to_body() {
        let output = BackendSuccess {
            stdout: r###"{"iid":7,"web_url":"u","state":"opened","draft":false,"title":"t","source_branch":"feat/x","target_branch":"main","merge_status":"can_be_merged","labels":[],"description":"## Summary\ny"}"###.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitLab), &output).unwrap();
        assert_eq!(p.body.as_deref(), Some("## Summary\ny"));
    }

    #[test]
    fn parse_gitlab_locked_normalises_to_closed() {
        let output = BackendSuccess {
            stdout: r#"{"iid":7,"web_url":"u","state":"locked","draft":false,"title":"t","source_branch":"feat/x","target_branch":"main","merge_status":"cannot_be_merged","labels":[]}"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitLab), &output).unwrap();
        assert_eq!(p.state, "closed");
        assert_eq!(p.mergeable, "no");
    }

    #[test]
    fn parse_gitlab_unknown_state_errors_software() {
        let output = BackendSuccess {
            stdout: r#"{"iid":1,"web_url":"u","state":"invented","draft":false,"title":"t","source_branch":"feat/x","target_branch":"main","merge_status":"unknown","labels":[]}"#.into(),
            stderr: String::new(),
        };
        let err = parse_view_output(&ctx(Provider::GitLab), &output).expect_err("invented");
        assert_eq!(err.kind(), "software_error");
    }

    #[test]
    fn extract_first_iid_from_list_array() {
        let s = r#"[{"iid":42,"title":"x"},{"iid":7}]"#;
        assert_eq!(extract_first_iid(s), Some("42".into()));
        assert_eq!(extract_first_iid("[]"), None);
        assert_eq!(extract_first_iid("not json"), None);
    }

    #[test]
    fn build_view_call_github_requests_merge_commit_field() {
        let plan = build_view_call(&ctx(Provider::GitHub), "5").plan_argv();
        let json_idx = plan.iter().position(|s| s == "--json").unwrap();
        assert!(
            plan[json_idx + 1].contains("mergeCommit"),
            "GitHub --json field list must request mergeCommit so merge_commit_sha is populated"
        );
    }

    #[test]
    fn parse_github_merged_pr_extracts_merge_commit_sha() {
        let output = BackendSuccess {
            stdout: r#"{
                "number":5,"url":"u","state":"CLOSED","isDraft":false,"title":"t",
                "headRefName":"feat/x","baseRefName":"main","mergeable":"UNKNOWN",
                "mergedAt":"2026-05-19T10:00:00Z",
                "mergeCommit":{"oid":"abcdef0123456789"},
                "labels":[]
            }"#
            .into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitHub), &output).unwrap();
        assert_eq!(p.state, "merged");
        assert_eq!(p.merge_commit_sha.as_deref(), Some("abcdef0123456789"));
    }

    #[test]
    fn parse_github_open_pr_has_no_merge_commit_sha() {
        let output = BackendSuccess {
            stdout: r#"{"number":5,"url":"u","state":"OPEN","isDraft":false,"title":"t","headRefName":"feat/x","baseRefName":"main","mergeable":"MERGEABLE","mergedAt":null,"mergeCommit":null,"labels":[]}"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitHub), &output).unwrap();
        assert_eq!(p.merge_commit_sha, None);
    }

    #[test]
    fn parse_gitlab_merged_mr_extracts_merge_commit_sha() {
        let output = BackendSuccess {
            stdout: r#"{
                "iid":7,"web_url":"u","state":"merged","draft":false,"title":"t",
                "source_branch":"feat/x","target_branch":"main","merge_status":"can_be_merged",
                "merged_at":"2026-05-19T10:00:00Z",
                "merge_commit_sha":"1234567890abcdef",
                "labels":[]
            }"#
            .into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitLab), &output).unwrap();
        assert_eq!(p.state, "merged");
        assert_eq!(p.merge_commit_sha.as_deref(), Some("1234567890abcdef"));
    }

    #[test]
    fn parse_gitlab_open_mr_treats_empty_merge_commit_sha_as_none() {
        let output = BackendSuccess {
            stdout: r#"{"iid":7,"web_url":"u","state":"opened","draft":false,"title":"t","source_branch":"feat/x","target_branch":"main","merge_status":"can_be_merged","merge_commit_sha":"","labels":[]}"#.into(),
            stderr: String::new(),
        };
        let p = parse_view_output(&ctx(Provider::GitLab), &output).unwrap();
        assert_eq!(p.merge_commit_sha, None);
    }
}
