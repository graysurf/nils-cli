//! `auth status` atom.
//!
//! Spec: `crates/forge-cli/docs/specs/forge-cli-spec-v1.md` parity row for `auth status` plus
//! `crates/forge-cli/docs/specs/forge-cli-ops-v1.yaml::operations.auth.status`. Schema literal:
//! `cli.forge-cli.auth.status.v1`. Both backends emit text output; this module
//! parses each provider's text into a normalized envelope payload.

use std::ffi::OsString;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess, DryRunPayload};
use crate::cli::{BINARY, GlobalFlags};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::provider::{Provider, ProviderContext, detect_unscoped, git_remote_url};
use crate::rate_limit::default_runner;

const SCHEMA: &str = "auth.status";
const SCHEMA_VERSION: u32 = 1;

/// Normalized payload emitted by the envelope's `data` field.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AuthStatusPayload {
    pub provider: &'static str,
    pub host: String,
    pub user: Option<String>,
    pub scopes: Vec<String>,
}

/// CLI entry point: dispatch using the real subprocess runner.
pub fn run(global: &GlobalFlags, format: OutputFormat) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_with(&runner, global, format, git_remote_url)
}

/// Test-friendly entry: caller injects the backend runner and the remote-URL
/// lookup.
pub fn run_with<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    format: OutputFormat,
    remote_url_lookup: F,
) -> Result<i32, ForgeError> {
    let ctx = detect_unscoped(
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

    let payload = compute_with_ctx(runner, &ctx)?;
    Ok(emit_success(
        schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION),
        payload,
        format,
        render_text,
    ))
}

/// Macro-facing entry point: compute the payload without emitting an
/// envelope. Used by `pr deliver` to capture each step's typed output.
pub fn compute<R: BackendRunner, F: Fn(&str) -> Option<String>>(
    runner: &R,
    global: &GlobalFlags,
    remote_url_lookup: F,
) -> Result<AuthStatusPayload, ForgeError> {
    let ctx = detect_unscoped(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        remote_url_lookup,
    )?;
    compute_with_ctx(runner, &ctx)
}

fn compute_with_ctx<R: BackendRunner>(
    runner: &R,
    ctx: &ProviderContext,
) -> Result<AuthStatusPayload, ForgeError> {
    let call = build_call(ctx);
    let output = runner.run(&call)?;
    parse_backend_output(ctx, &output)
}

pub(crate) fn build_call(ctx: &ProviderContext) -> BackendCall {
    let program = BackendProgram::for_provider(ctx.provider);
    let argv: Vec<OsString> = vec![
        OsString::from("auth"),
        OsString::from("status"),
        OsString::from("--hostname"),
        OsString::from(crate::provider::canonical_provider_host(
            ctx.provider,
            &ctx.host,
        )),
    ];
    BackendCall::new(program, argv).with_host(ctx.provider, &ctx.host)
}

/// Parse the backend stdout/stderr into the normalized payload.
/// `gh auth status` and `glab auth status` print to stderr, not stdout — both
/// streams are searched.
pub fn parse_backend_output(
    ctx: &ProviderContext,
    output: &BackendSuccess,
) -> Result<AuthStatusPayload, ForgeError> {
    let combined = format!("{}\n{}", output.stdout, output.stderr);
    match ctx.provider {
        Provider::GitHub | Provider::Local => parse_github(&combined, ctx),
        Provider::GitLab => parse_gitlab(&combined, ctx),
    }
}

fn parse_github(text: &str, ctx: &ProviderContext) -> Result<AuthStatusPayload, ForgeError> {
    // gh prints:
    //   github.com
    //     ✓ Logged in to github.com account <user> (keyring)
    //     - Active account: true
    //     - Token scopes: 'repo', 'read:org'
    let host = find_first_match(text, "Logged in to", |after| {
        after.split_whitespace().next().map(str::to_string)
    })
    .unwrap_or_else(|| ctx.host.clone());
    let user = find_first_match(text, "account ", |after| {
        after
            .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
            .find(|s| !s.is_empty())
            .map(str::to_string)
    });
    let scopes = find_first_match(text, "Token scopes:", |after| Some(after.to_string()))
        .map(parse_scope_list)
        .unwrap_or_default();
    Ok(AuthStatusPayload {
        provider: ctx.provider.as_str(),
        host,
        user,
        scopes,
    })
}

fn parse_gitlab(text: &str, ctx: &ProviderContext) -> Result<AuthStatusPayload, ForgeError> {
    // glab prints:
    //   gitlab.com
    //     ✓ Logged in to gitlab.com as <user> (~/.config/glab-cli/config.yml)
    //     ✓ Git operations for gitlab.com configured to use ssh protocol.
    //     ✓ API calls for gitlab.com are made over https protocol
    //     ✓ REST API Endpoint: https://gitlab.com/api/v4/
    //     ✓ GraphQL Endpoint: https://gitlab.com/api/graphql/
    //     ✓ Token: <token>
    let host = find_first_match(text, "Logged in to", |after| {
        after.split_whitespace().next().map(str::to_string)
    })
    .unwrap_or_else(|| ctx.host.clone());
    let user = find_first_match(text, " as ", |after| {
        after
            .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
            .find(|s| !s.is_empty())
            .map(str::to_string)
    });
    // glab does not surface scopes in `auth status`; leave the vec empty.
    Ok(AuthStatusPayload {
        provider: ctx.provider.as_str(),
        host,
        user,
        scopes: Vec::new(),
    })
}

fn find_first_match<F>(text: &str, marker: &str, extract: F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    for line in text.lines() {
        if let Some(idx) = line.find(marker) {
            let after = &line[idx + marker.len()..];
            if let Some(value) = extract(after.trim())
                && !value.is_empty()
            {
                return Some(value);
            }
        }
    }
    None
}

fn parse_scope_list(raw: String) -> Vec<String> {
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .map(|s| s.trim_matches(|c: char| c == '\'' || c == '"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn render_text(payload: &AuthStatusPayload) {
    let user = payload.user.as_deref().unwrap_or("<unknown>");
    println!(
        "logged in to {host} as {user} ({provider})",
        host = payload.host,
        provider = payload.provider
    );
    if !payload.scopes.is_empty() {
        println!("scopes: {}", payload.scopes.join(", "));
    }
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
    fn parses_github_auth_status_with_scopes() {
        let ctx = github_ctx();
        let output = BackendSuccess {
            stdout: String::new(),
            stderr: "github.com\n  ✓ Logged in to github.com account testuser-gh (keyring)\n  - Active account: true\n  - Token scopes: 'repo', 'read:org'\n".into(),
        };
        let payload = parse_backend_output(&ctx, &output).expect("parse");
        assert_eq!(payload.provider, "github");
        assert_eq!(payload.host, "github.com");
        assert_eq!(payload.user.as_deref(), Some("testuser-gh"));
        assert_eq!(
            payload.scopes,
            vec!["repo".to_string(), "read:org".to_string()]
        );
    }

    #[test]
    fn parses_gitlab_auth_status_without_scopes() {
        let ctx = gitlab_ctx();
        let output = BackendSuccess {
            stdout: String::new(),
            stderr: "gitlab.com\n  ✓ Logged in to gitlab.com as testuser-glab (~/.config/glab-cli/config.yml)\n".into(),
        };
        let payload = parse_backend_output(&ctx, &output).expect("parse");
        assert_eq!(payload.provider, "gitlab");
        assert_eq!(payload.host, "gitlab.com");
        assert_eq!(payload.user.as_deref(), Some("testuser-glab"));
        assert!(payload.scopes.is_empty());
    }

    #[test]
    fn parses_missing_user_as_none() {
        let ctx = github_ctx();
        let output = BackendSuccess {
            stdout: String::new(),
            stderr: "github.com\n  ✓ Logged in to github.com.\n".into(),
        };
        let payload = parse_backend_output(&ctx, &output).expect("parse");
        assert_eq!(payload.user, None);
    }

    #[test]
    fn build_call_binds_github_auth_status_to_resolved_host() {
        let ctx = github_ctx();
        let call = build_call(&ctx);
        assert_eq!(
            call.plan_argv()[1..],
            vec![
                "auth".to_string(),
                "status".to_string(),
                "--hostname".to_string(),
                "github.com".to_string(),
            ]
        );
    }

    #[test]
    fn build_call_binds_gitlab_auth_status_to_resolved_host() {
        let mut ctx = gitlab_ctx();
        ctx.host = "gitlab.example.com".into();
        let call = build_call(&ctx);
        assert_eq!(
            call.plan_argv()[1..],
            vec![
                "auth".to_string(),
                "status".to_string(),
                "--hostname".to_string(),
                "gitlab.example.com".to_string(),
            ]
        );
    }
}
