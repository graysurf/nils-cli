use std::io::Write;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::suite::resolve::{
    resolve_gql_url_for_env, resolve_grpc_url_for_env, resolve_path_from_repo_root,
    resolve_rest_base_url_for_env, resolve_ws_url_for_env,
};
use crate::suite::schema::SuiteDefaults;

pub(crate) fn sanitize_id(id: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in id.chars() {
        let ok = c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-';
        if ok {
            out.push(c);
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "case".to_string()
    } else {
        out
    }
}

pub(crate) fn time_run_id_now() -> Result<String> {
    let format = time::format_description::parse_borrowed::<2>(
        "[year][month][day]-[hour][minute][second]Z",
    )?;
    Ok(time::OffsetDateTime::now_utc().format(&format)?)
}

pub(crate) fn time_iso_now() -> Result<String> {
    let format = time::format_description::parse_borrowed::<2>(
        "[year]-[month]-[day]T[hour]:[minute]:[second]Z",
    )?;
    Ok(time::OffsetDateTime::now_utc().format(&format)?)
}

pub(crate) fn path_relative_to_repo_or_abs(repo_root: &Path, path: &Path) -> String {
    if let Ok(stripped) = path.strip_prefix(repo_root) {
        let s = stripped.to_string_lossy();
        if s.is_empty() {
            ".".to_string()
        } else {
            s.to_string()
        }
    } else {
        path.to_string_lossy().to_string()
    }
}

pub(crate) fn resolve_rest_base_url(
    repo_root: &Path,
    setup_dir_raw: &str,
    url_override: &str,
    env_value: &str,
    suite_defaults: &SuiteDefaults,
    env_rest_url: &str,
) -> Result<String> {
    let url_override = url_override.trim();
    if !url_override.is_empty() {
        return Ok(url_override.to_string());
    }
    let default_url = suite_defaults.rest.url.trim();
    if !default_url.is_empty() {
        return Ok(default_url.to_string());
    }
    let env_rest_url = env_rest_url.trim();
    if !env_rest_url.is_empty() {
        return Ok(env_rest_url.to_string());
    }
    let env_value = env_value.trim();
    if !env_value.is_empty() {
        let setup_dir = resolve_path_from_repo_root(repo_root, setup_dir_raw);
        return resolve_rest_base_url_for_env(&setup_dir, env_value);
    }
    Ok("http://localhost:6700".to_string())
}

pub(crate) fn resolve_gql_url(
    repo_root: &Path,
    setup_dir_raw: &str,
    url_override: &str,
    env_value: &str,
    suite_defaults: &SuiteDefaults,
    env_gql_url: &str,
) -> Result<String> {
    let url_override = url_override.trim();
    if !url_override.is_empty() {
        return Ok(url_override.to_string());
    }
    let default_url = suite_defaults.graphql.url.trim();
    if !default_url.is_empty() {
        return Ok(default_url.to_string());
    }
    let env_gql_url = env_gql_url.trim();
    if !env_gql_url.is_empty() {
        return Ok(env_gql_url.to_string());
    }
    let env_value = env_value.trim();
    if !env_value.is_empty() {
        let setup_dir = resolve_path_from_repo_root(repo_root, setup_dir_raw);
        return resolve_gql_url_for_env(&setup_dir, env_value);
    }
    Ok("http://localhost:6700/graphql".to_string())
}

pub(crate) fn resolve_grpc_url(
    repo_root: &Path,
    setup_dir_raw: &str,
    url_override: &str,
    env_value: &str,
    suite_defaults: &SuiteDefaults,
    env_grpc_url: &str,
) -> Result<String> {
    let url_override = url_override.trim();
    if !url_override.is_empty() {
        return Ok(url_override.to_string());
    }
    let default_url = suite_defaults.grpc.url.trim();
    if !default_url.is_empty() {
        return Ok(default_url.to_string());
    }
    let env_grpc_url = env_grpc_url.trim();
    if !env_grpc_url.is_empty() {
        return Ok(env_grpc_url.to_string());
    }
    let env_value = env_value.trim();
    if !env_value.is_empty() {
        let setup_dir = resolve_path_from_repo_root(repo_root, setup_dir_raw);
        return resolve_grpc_url_for_env(&setup_dir, env_value);
    }
    Ok("127.0.0.1:50051".to_string())
}

pub(crate) fn resolve_ws_url(
    repo_root: &Path,
    setup_dir_raw: &str,
    url_override: &str,
    env_value: &str,
    suite_defaults: &SuiteDefaults,
    env_ws_url: &str,
) -> Result<String> {
    let url_override = url_override.trim();
    if !url_override.is_empty() {
        return Ok(url_override.to_string());
    }
    let default_url = suite_defaults.websocket.url.trim();
    if !default_url.is_empty() {
        return Ok(default_url.to_string());
    }
    let env_ws_url = env_ws_url.trim();
    if !env_ws_url.is_empty() {
        return Ok(env_ws_url.to_string());
    }
    let env_value = env_value.trim();
    if !env_value.is_empty() {
        let setup_dir = resolve_path_from_repo_root(repo_root, setup_dir_raw);
        return resolve_ws_url_for_env(&setup_dir, env_value);
    }
    Ok("ws://127.0.0.1:9001/ws".to_string())
}

pub(crate) fn resolve_rest_token_profile(setup_dir: &Path, profile: &str) -> Result<String> {
    let tokens_env = setup_dir.join("tokens.env");
    let tokens_local = setup_dir.join("tokens.local.env");
    let files: Vec<&Path> = if tokens_env.is_file() || tokens_local.is_file() {
        vec![&tokens_env, &tokens_local]
    } else {
        Vec::new()
    };

    let found = crate::env_file::read_prefixed_var("REST_TOKEN_", profile, &files)?;
    found.ok_or_else(|| anyhow::anyhow!("Token profile '{profile}' is empty/missing."))
}

pub(crate) fn resolve_grpc_token_profile(setup_dir: &Path, profile: &str) -> Result<String> {
    let tokens_env = setup_dir.join("tokens.env");
    let tokens_local = setup_dir.join("tokens.local.env");
    let files: Vec<&Path> = if tokens_env.is_file() || tokens_local.is_file() {
        vec![&tokens_env, &tokens_local]
    } else {
        Vec::new()
    };

    let found = crate::env_file::read_prefixed_var("GRPC_TOKEN_", profile, &files)?;
    found.ok_or_else(|| anyhow::anyhow!("Token profile '{profile}' is empty/missing."))
}

pub(crate) fn resolve_ws_token_profile(setup_dir: &Path, profile: &str) -> Result<String> {
    let tokens_env = setup_dir.join("tokens.env");
    let tokens_local = setup_dir.join("tokens.local.env");
    let files: Vec<&Path> = if tokens_env.is_file() || tokens_local.is_file() {
        vec![&tokens_env, &tokens_local]
    } else {
        Vec::new()
    };

    let found = crate::env_file::read_prefixed_var("WS_TOKEN_", profile, &files)?;
    found.ok_or_else(|| anyhow::anyhow!("Token profile '{profile}' is empty/missing."))
}

pub(crate) fn resolve_graphql_bearer_token(
    setup_dir: &Path,
    endpoint_url: &str,
    operation_file: &Path,
    jwt_profile: &str,
    stderr: &mut dyn Write,
) -> Result<Option<String>> {
    if jwt_profile.trim().is_empty() {
        return Ok(None);
    }
    let auth = crate::graphql::auth::resolve_bearer_token(
        setup_dir,
        endpoint_url,
        Some(operation_file),
        Some(jwt_profile),
        stderr,
    )?;
    Ok(auth.bearer_token)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaseOutputPaths {
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
}

pub(crate) fn plan_case_output_paths(run_dir_abs: &Path, safe_id: &str) -> CaseOutputPaths {
    CaseOutputPaths {
        stdout_path: run_dir_abs.join(format!("{safe_id}.response.json")),
        stderr_path: run_dir_abs.join(format!("{safe_id}.stderr.log")),
    }
}

pub(crate) fn resolve_effective_env(raw_env: &str, defaults: &SuiteDefaults) -> String {
    let effective_env = raw_env.trim().to_string();
    if effective_env.is_empty() {
        defaults.env.trim().to_string()
    } else {
        effective_env
    }
}

pub(crate) fn resolve_effective_no_history(
    case_no_history: Option<bool>,
    defaults: &SuiteDefaults,
) -> bool {
    case_no_history.unwrap_or(defaults.no_history)
}
