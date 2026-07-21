//! `repo view` atom.
//!
//! Spec parity row for `repo view`; ops YAML
//! `operations.repo.view`. Schema literal: `cli.forge-cli.repo.view.v1`.
//! `gh repo view --json …` returns JSON with camelCase boolean trio for the
//! three merge methods; `glab repo view -F json` returns its own shape. Both
//! are normalized to `{owner, name, url, default_branch,
//! merge_methods_allowed}`.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload};
use crate::cli::{BINARY, GlobalFlags};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::provider::{Provider, ProviderContext, git_remote_url};
use crate::rate_limit::default_runner;

const SCHEMA: &str = "repo.view";
const SCHEMA_VERSION: u32 = 1;

const GH_JSON_FIELDS: &str =
    "name,owner,defaultBranchRef,mergeCommitAllowed,squashMergeAllowed,rebaseMergeAllowed,url";

/// Normalized payload emitted by the envelope's `data` field.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RepoViewPayload {
    pub provider: &'static str,
    pub owner: String,
    pub name: String,
    pub url: String,
    pub default_branch: String,
    pub merge_methods_allowed: Vec<&'static str>,
}

/// CLI entry point.
pub fn run(global: &GlobalFlags, format: OutputFormat) -> Result<i32, ForgeError> {
    if global.named_provider().is_some() {
        return crate::forgejo::run_repo_view(global, format);
    }
    let runner = default_runner();
    run_with(&runner, global, format, git_remote_url)
}

pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = crate::provider::detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;

    if global.dry_run {
        let call = build_call(&ctx);
        let payload = DryRunPayload::new(ctx.provider, &call);
        return Ok(emit_success(
            schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
            payload,
            format,
            |p| println!("would run: {plan}", plan = p.plan.join(" ")),
        ));
    }

    let payload = compute(runner, &ctx)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        payload,
        format,
        render_text,
    ))
}

/// Macro-facing entry point: compute the payload without emitting. Used by
/// `pr deliver` to capture the repo view step's typed output for the
/// composite envelope. Repo override comes from `ctx.repo` (set by the
/// global `--repo owner/name` flag in [`crate::provider::detect`]).
pub fn compute<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
) -> Result<RepoViewPayload, ForgeError> {
    let call = build_call(ctx);
    let output = runner.run(&call)?;
    parse_backend_output(ctx, &output)
}

fn build_call(ctx: &ProviderContext) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let mut argv: Vec<OsString> = Vec::new();
    match ctx.provider {
        Provider::GitHub | Provider::Local => {
            argv.push(OsString::from("repo"));
            argv.push(OsString::from("view"));
            if let Some(locator) = ctx.repo_locator() {
                argv.push(OsString::from(locator));
            }
            argv.push(OsString::from("--json"));
            argv.push(OsString::from(GH_JSON_FIELDS));
        }
        Provider::GitLab => {
            argv.push(OsString::from("repo"));
            argv.push(OsString::from("view"));
            if let Some(locator) = ctx.repo_locator() {
                argv.push(OsString::from(locator));
            }
            argv.push(OsString::from("-F"));
            argv.push(OsString::from("json"));
        }
    }
    BackendCall::new(program, argv).with_host(ctx.provider, &ctx.host)
}

/// Re-export of the internal builder so `pr create` (and later atoms) can
/// resolve the repo's default branch without duplicating the argv shape.
/// Repo override (when set) flows through `ctx.repo`.
pub fn build_call_for_default_branch(ctx: &ProviderContext) -> BackendCall {
    build_call(ctx)
}

/// Parse the backend stdout into the normalized payload.
pub fn parse_backend_output(
    ctx: &ProviderContext,
    output: &BackendSuccess,
) -> Result<RepoViewPayload, ForgeError> {
    match ctx.provider {
        Provider::GitHub | Provider::Local => parse_github(&output.stdout, ctx),
        Provider::GitLab => parse_gitlab(&output.stdout, ctx),
    }
}

fn parse_github(stdout: &str, ctx: &ProviderContext) -> Result<RepoViewPayload, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema(),
            "gh repo view --json output is not valid JSON",
            Some(e.to_string()),
        )
    })?;
    let owner = value
        .get("owner")
        .and_then(|o| o.get("login"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing("owner.login"))?;
    let name = value
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing("name"))?;
    let url = value
        .get("url")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing("url"))?;
    let default_branch = value
        .get("defaultBranchRef")
        .and_then(|d| d.get("name"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing("defaultBranchRef.name"))?;
    let mut methods: Vec<&'static str> = Vec::new();
    if value
        .get("squashMergeAllowed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        methods.push("squash");
    }
    if value
        .get("mergeCommitAllowed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        methods.push("merge");
    }
    if value
        .get("rebaseMergeAllowed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        methods.push("rebase");
    }
    Ok(RepoViewPayload {
        provider: ctx.provider.as_str(),
        owner,
        name,
        url,
        default_branch,
        merge_methods_allowed: methods,
    })
}

fn parse_gitlab(stdout: &str, ctx: &ProviderContext) -> Result<RepoViewPayload, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema(),
            "glab repo view -F json output is not valid JSON",
            Some(e.to_string()),
        )
    })?;
    let owner = value
        .get("namespace")
        .and_then(|n| n.get("full_path").or_else(|| n.get("path")))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing("namespace.full_path"))?;
    let name = value
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing("path"))?;
    let url = value
        .get("web_url")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing("web_url"))?;
    let default_branch = value
        .get("default_branch")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| missing("default_branch"))?;
    // GitLab exposes `merge_method = "merge"|"rebase_merge"|"ff"` plus a
    // `squash_option` of "always"|"default_on"|"default_off"|"never". v1 of
    // forge-cli normalizes these into the workspace's three-method enum.
    let mut methods: Vec<&'static str> = Vec::new();
    let squash_option = value
        .get("squash_option")
        .and_then(|v| v.as_str())
        .unwrap_or("default_on");
    if squash_option != "never" {
        methods.push("squash");
    }
    let merge_method = value
        .get("merge_method")
        .and_then(|v| v.as_str())
        .unwrap_or("merge");
    if merge_method == "merge" {
        methods.push("merge");
    }
    if merge_method.contains("rebase") || merge_method == "ff" {
        methods.push("rebase");
    }
    Ok(RepoViewPayload {
        provider: ctx.provider.as_str(),
        owner,
        name,
        url,
        default_branch,
        merge_methods_allowed: methods,
    })
}

fn render_text(payload: &RepoViewPayload) {
    println!(
        "{owner}/{name} (default: {default}, methods: {methods})\nurl: {url}",
        owner = payload.owner,
        name = payload.name,
        default = payload.default_branch,
        methods = payload.merge_methods_allowed.join(","),
        url = payload.url,
    );
}

fn schema() -> String {
    schema_version_for(BINARY, "error", 1)
}

fn missing(field: &str) -> ForgeError {
    ForgeError::software(
        schema(),
        format!("repo view JSON missing required field: {field}"),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::DetectionSource;
    use pretty_assertions::assert_eq;

    fn github_ctx() -> ProviderContext {
        ProviderContext {
            provider: Provider::GitHub,
            host: "github.com".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    fn gitlab_ctx() -> ProviderContext {
        ProviderContext {
            provider: Provider::GitLab,
            host: "gitlab.com".into(),
            source: DetectionSource::Flag,
            repo: None,
        }
    }

    #[test]
    fn parses_github_repo_view_all_methods() {
        let json = r#"{
            "name": "nils-cli",
            "owner": { "login": "sympoies" },
            "url": "https://github.com/sympoies/nils-cli",
            "defaultBranchRef": { "name": "main" },
            "mergeCommitAllowed": true,
            "squashMergeAllowed": true,
            "rebaseMergeAllowed": true
        }"#;
        let ctx = github_ctx();
        let output = BackendSuccess {
            stdout: json.to_string(),
            stderr: String::new(),
        };
        let payload = parse_backend_output(&ctx, &output).expect("parse");
        assert_eq!(payload.owner, "sympoies");
        assert_eq!(payload.name, "nils-cli");
        assert_eq!(payload.default_branch, "main");
        assert_eq!(
            payload.merge_methods_allowed,
            vec!["squash", "merge", "rebase"]
        );
    }

    #[test]
    fn parses_github_repo_view_squash_only() {
        let json = r#"{
            "name": "nils-cli",
            "owner": { "login": "sympoies" },
            "url": "https://github.com/sympoies/nils-cli",
            "defaultBranchRef": { "name": "main" },
            "mergeCommitAllowed": false,
            "squashMergeAllowed": true,
            "rebaseMergeAllowed": false
        }"#;
        let ctx = github_ctx();
        let output = BackendSuccess {
            stdout: json.to_string(),
            stderr: String::new(),
        };
        let payload = parse_backend_output(&ctx, &output).expect("parse");
        assert_eq!(payload.merge_methods_allowed, vec!["squash"]);
    }

    #[test]
    fn parses_gitlab_repo_view() {
        let json = r#"{
            "path": "nils-cli",
            "namespace": { "full_path": "sympoies" },
            "web_url": "https://gitlab.com/sympoies/nils-cli",
            "default_branch": "main",
            "merge_method": "merge",
            "squash_option": "default_on"
        }"#;
        let ctx = gitlab_ctx();
        let output = BackendSuccess {
            stdout: json.to_string(),
            stderr: String::new(),
        };
        let payload = parse_backend_output(&ctx, &output).expect("parse");
        assert_eq!(payload.owner, "sympoies");
        assert_eq!(payload.name, "nils-cli");
        assert_eq!(payload.default_branch, "main");
        // GitLab's "merge" + squash default_on → squash + merge (no rebase).
        assert_eq!(payload.merge_methods_allowed, vec!["squash", "merge"]);
    }

    #[test]
    fn malformed_json_is_software_error() {
        let ctx = github_ctx();
        let output = BackendSuccess {
            stdout: "not json".into(),
            stderr: String::new(),
        };
        let err = parse_backend_output(&ctx, &output).expect_err("software");
        assert_eq!(err.kind(), "software_error");
    }

    #[test]
    fn missing_field_is_software_error() {
        let ctx = github_ctx();
        let output = BackendSuccess {
            stdout: "{}".into(),
            stderr: String::new(),
        };
        let err = parse_backend_output(&ctx, &output).expect_err("software");
        assert_eq!(err.kind(), "software_error");
    }

    #[test]
    fn build_call_github_argv() {
        let call = build_call(&github_ctx());
        let argv = call.plan_argv();
        assert_eq!(argv[1..3], vec!["repo".to_string(), "view".to_string()]);
        assert!(argv.contains(&"--json".to_string()));
    }

    #[test]
    fn build_call_gitlab_argv() {
        let call = build_call(&gitlab_ctx());
        let argv = call.plan_argv();
        assert_eq!(argv[1..3], vec!["repo".to_string(), "view".to_string()]);
        assert!(argv.contains(&"-F".to_string()));
    }

    #[test]
    fn build_call_github_includes_repo_slug_when_ctx_has_repo() {
        let mut ctx = github_ctx();
        ctx.repo = Some("owner/name".into());
        let argv = build_call(&ctx).plan_argv();
        let pos = argv.iter().position(|s| s == "view").expect("view present");
        assert_eq!(argv[pos + 1], "owner/name");
    }

    #[test]
    fn build_call_github_qualifies_enterprise_repo_locator() {
        let mut ctx = github_ctx();
        ctx.host = "internal.ghe.com".into();
        ctx.repo = Some("owner/name".into());
        let call = build_call(&ctx);
        assert_eq!(call.resolved_host(), Some("internal.ghe.com"));
        let argv = call.plan_argv();
        let pos = argv.iter().position(|s| s == "view").expect("view present");
        assert_eq!(argv[pos + 1], "internal.ghe.com/owner/name");
    }

    #[test]
    fn build_call_gitlab_uses_url_for_self_hosted_repo_locator() {
        let mut ctx = gitlab_ctx();
        ctx.host = "gitlab.example.com".into();
        ctx.repo = Some("group/subgroup/project".into());
        let call = build_call(&ctx);
        assert_eq!(call.resolved_host(), Some("gitlab.example.com"));
        let argv = call.plan_argv();
        let pos = argv.iter().position(|s| s == "view").expect("view present");
        assert_eq!(
            argv[pos + 1],
            "https://gitlab.example.com/group/subgroup/project"
        );
    }
}
