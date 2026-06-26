use anyhow::Result;
use chrono::Utc;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::auth;
use crate::auth::output::{self, AuthRemotePullResult};
use crate::json;
use crate::paths;
use nils_common::env as shared_env;
use nils_common::fs;

const COMMAND_PULL: &str = "auth remote pull";
pub const ENV_AUTH_REMOTE_SSH: &str = "CODEX_AUTH_REMOTE_SSH";
pub const ENV_AUTH_REMOTE_NAME: &str = "CODEX_AUTH_REMOTE_NAME";
pub const ENV_AUTH_REMOTE_REFRESH: &str = "CODEX_AUTH_REMOTE_REFRESH";

#[derive(Debug, Clone)]
pub struct ConfiguredRemotePull {
    pub ssh: String,
    pub name: String,
    pub refresh: bool,
}

#[derive(Debug, Clone)]
pub struct RemoteEnvError {
    pub code: &'static str,
    pub message: String,
    pub details: Value,
}

#[derive(Debug, Clone)]
pub struct RemotePullFailure {
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
    pub exit_code: i32,
}

pub fn pull_with_json(
    ssh_host: Option<&str>,
    name: Option<&str>,
    access_only: bool,
    write_active: bool,
    refresh: bool,
    output_json: bool,
) -> Result<i32> {
    let Some(ssh_host) = ssh_host else {
        return usage_error(
            output_json,
            "missing-ssh",
            "codex-remote-pull: --ssh is required",
        );
    };
    let Some(name) = name else {
        return usage_error(
            output_json,
            "missing-name",
            "codex-remote-pull: --name is required",
        );
    };
    if !is_valid_ssh_host(ssh_host) {
        return usage_error(
            output_json,
            "invalid-ssh-host",
            "codex-remote-pull: invalid ssh host",
        );
    }
    if !is_valid_secret_name(name) {
        return usage_error(
            output_json,
            "invalid-secret-name",
            "codex-remote-pull: invalid secret name",
        );
    }
    if !access_only {
        return usage_error(
            output_json,
            "access-only-required",
            "codex-remote-pull: --access-only is required",
        );
    }
    if !write_active {
        return usage_error(
            output_json,
            "write-active-required",
            "codex-remote-pull: --write-active is required",
        );
    }

    let result = match pull_access_only_to_active(ssh_host, name, refresh)? {
        Ok(result) => result,
        Err(err) => return emit_pull_failure(output_json, err),
    };

    if output_json {
        output::emit_result(COMMAND_PULL, result)?;
    } else {
        println!(
            "codex-remote-pull: pulled access-only auth '{}' from {} into {}",
            name, ssh_host, result.auth_file
        );
    }

    Ok(0)
}

pub fn configured_pull_from_env()
-> std::result::Result<Option<ConfiguredRemotePull>, RemoteEnvError> {
    let ssh = env_non_empty(ENV_AUTH_REMOTE_SSH);
    let name = env_non_empty(ENV_AUTH_REMOTE_NAME);

    if ssh.is_none() && name.is_none() {
        return Ok(None);
    }

    let Some(ssh) = ssh else {
        return Err(remote_env_error(
            "remote-ssh-missing",
            format!(
                "codex-refresh: {ENV_AUTH_REMOTE_SSH} is required when {ENV_AUTH_REMOTE_NAME} is set"
            ),
        ));
    };
    let Some(name) = name else {
        return Err(remote_env_error(
            "remote-name-missing",
            format!(
                "codex-refresh: {ENV_AUTH_REMOTE_NAME} is required when {ENV_AUTH_REMOTE_SSH} is set"
            ),
        ));
    };

    if !is_valid_ssh_host(&ssh) {
        return Err(remote_env_error(
            "remote-ssh-invalid",
            format!("codex-refresh: invalid {ENV_AUTH_REMOTE_SSH}"),
        ));
    }
    if !is_valid_secret_name(&name) {
        return Err(remote_env_error(
            "remote-name-invalid",
            format!("codex-refresh: invalid {ENV_AUTH_REMOTE_NAME}"),
        ));
    }

    Ok(Some(ConfiguredRemotePull {
        ssh,
        name,
        refresh: shared_env::env_truthy(ENV_AUTH_REMOTE_REFRESH),
    }))
}

pub fn pull_access_only_to_active(
    ssh_host: &str,
    name: &str,
    refresh: bool,
) -> Result<std::result::Result<AuthRemotePullResult, RemotePullFailure>> {
    let mut command = Command::new("ssh");
    command
        .arg(ssh_host)
        .arg("codex-cli")
        .arg("auth")
        .arg("remote")
        .arg("export")
        .arg("--name")
        .arg(name)
        .arg("--access-only");
    if refresh {
        command.arg("--refresh");
    }

    let remote_output = match command.output() {
        Ok(output) => output,
        Err(err) => {
            return Ok(Err(RemotePullFailure {
                code: "ssh-exec-failed",
                message: format!("codex-remote-pull: failed to run ssh: {err}"),
                details: None,
                exit_code: 1,
            }));
        }
    };

    if !remote_output.status.success() {
        let exit_code = remote_output.status.code().unwrap_or(1);
        return Ok(Err(RemotePullFailure {
            code: "remote-export-failed",
            message: format!("codex-remote-pull: remote export failed (exit {exit_code})"),
            details: Some(serde_json::json!({
                "ssh": ssh_host,
                "name": name,
                "exit_code": exit_code,
            })),
            exit_code: 1,
        }));
    }

    let imported: Value = match serde_json::from_slice(&remote_output.stdout) {
        Ok(value) => value,
        Err(_) => {
            return Ok(Err(RemotePullFailure {
                code: "remote-export-invalid-json",
                message: "codex-remote-pull: remote export returned invalid JSON".to_string(),
                details: Some(serde_json::json!({
                    "ssh": ssh_host,
                    "name": name,
                })),
                exit_code: 1,
            }));
        }
    };
    let mut imported = sanitize_access_only(imported);
    if !has_oauth_access_token(&imported) {
        return Ok(Err(RemotePullFailure {
            code: "remote-export-missing-access-token",
            message: "codex-remote-pull: remote export did not include an OAuth access token"
                .to_string(),
            details: Some(serde_json::json!({
                "ssh": ssh_host,
                "name": name,
            })),
            exit_code: 1,
        }));
    }
    ensure_last_refresh(&mut imported);

    let auth_file = match paths::resolve_auth_file() {
        Some(path) => path,
        None => {
            return Ok(Err(RemotePullFailure {
                code: "auth-file-not-configured",
                message: "codex-remote-pull: CODEX_AUTH_FILE is not configured".to_string(),
                details: None,
                exit_code: 1,
            }));
        }
    };

    if let Err(err) = write_active_auth(&auth_file, &imported) {
        return Ok(Err(RemotePullFailure {
            code: err.code(),
            message: format!(
                "codex-remote-pull: failed to write active auth {}: {}",
                auth_file.display(),
                err.source()
            ),
            details: Some(serde_json::json!({
                "auth_file": auth_file.display().to_string(),
                "phase": err.phase(),
                "auth_written": err.auth_written(),
            })),
            exit_code: 1,
        }));
    }

    Ok(Ok(AuthRemotePullResult {
        ssh: ssh_host.to_string(),
        name: name.to_string(),
        access_only: true,
        write_active: true,
        auth_file: auth_file.display().to_string(),
        has_oauth_access_token: has_oauth_access_token(&imported),
        has_oauth_refresh_token: has_non_empty_string(&imported, &["tokens", "refresh_token"])
            || has_non_empty_string(&imported, &["refresh_token"]),
    }))
}

pub fn export(name: &str, access_only: bool, refresh: bool) -> Result<i32> {
    if !is_valid_secret_name(name) {
        eprintln!("codex-remote-export: invalid secret name");
        return Ok(64);
    }
    if !access_only {
        eprintln!("codex-remote-export: --access-only is required");
        return Ok(64);
    }

    let secret_name = auth::normalize_secret_file_name(name);
    let target = match secret_file(&secret_name) {
        Some(path) => path,
        None => {
            eprintln!("codex-remote-export: CODEX_SECRET_DIR is not configured");
            return Ok(1);
        }
    };

    if refresh {
        let rc = auth::refresh::run_silent(std::slice::from_ref(&secret_name))?;
        if rc != 0 {
            eprintln!("codex-remote-export: refresh failed for {secret_name}");
            return Ok(rc);
        }
    }

    if !target.is_file() {
        eprintln!("codex-remote-export: {} not found", target.display());
        return Ok(1);
    }

    let value = match json::read_json(&target) {
        Ok(value) => sanitize_access_only(value),
        Err(_) => {
            eprintln!("codex-remote-export: failed to read {}", target.display());
            return Ok(2);
        }
    };

    println!("{}", serde_json::to_string(&value)?);
    Ok(0)
}

fn usage_error(output_json: bool, code: &str, message: &str) -> Result<i32> {
    if output_json {
        output::emit_error(COMMAND_PULL, code, message, None)?;
    } else {
        eprintln!("{message}");
    }
    Ok(64)
}

fn emit_pull_failure(output_json: bool, failure: RemotePullFailure) -> Result<i32> {
    if output_json {
        output::emit_error(COMMAND_PULL, failure.code, failure.message, failure.details)?;
    } else {
        eprintln!("{}", failure.message);
    }
    Ok(failure.exit_code)
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(|value| {
        let value = value.trim().to_string();
        if value.is_empty() { None } else { Some(value) }
    })
}

fn remote_env_error(code: &'static str, message: String) -> RemoteEnvError {
    RemoteEnvError {
        code,
        message,
        details: serde_json::json!({
            "ssh_env": ENV_AUTH_REMOTE_SSH,
            "name_env": ENV_AUTH_REMOTE_NAME,
            "refresh_env": ENV_AUTH_REMOTE_REFRESH,
        }),
    }
}

enum ActiveAuthWriteError {
    AuthFile(anyhow::Error),
    Timestamp(anyhow::Error),
}

impl ActiveAuthWriteError {
    fn code(&self) -> &'static str {
        match self {
            Self::AuthFile(_) => "active-auth-write-failed",
            Self::Timestamp(_) => "active-auth-timestamp-write-failed",
        }
    }

    fn phase(&self) -> &'static str {
        match self {
            Self::AuthFile(_) => "auth-file",
            Self::Timestamp(_) => "timestamp",
        }
    }

    fn auth_written(&self) -> bool {
        matches!(self, Self::Timestamp(_))
    }

    fn source(&self) -> &anyhow::Error {
        match self {
            Self::AuthFile(err) | Self::Timestamp(err) => err,
        }
    }
}

fn write_active_auth(
    auth_file: &Path,
    value: &Value,
) -> std::result::Result<(), ActiveAuthWriteError> {
    let output =
        serde_json::to_vec(value).map_err(|err| ActiveAuthWriteError::AuthFile(err.into()))?;
    fs::write_atomic(auth_file, &output, fs::SECRET_FILE_MODE)
        .map_err(|err| ActiveAuthWriteError::AuthFile(err.into()))?;

    if let Some(timestamp_path) = paths::resolve_secret_timestamp_path(auth_file) {
        let last_refresh = json::string_at(value, &["last_refresh"]);
        fs::write_timestamp(&timestamp_path, last_refresh.as_deref())
            .map_err(|err| ActiveAuthWriteError::Timestamp(err.into()))?;
    }

    Ok(())
}

fn secret_file(secret_name: &str) -> Option<PathBuf> {
    Some(paths::resolve_secret_dir()?.join(secret_name))
}

fn sanitize_access_only(mut value: Value) -> Value {
    const TOKEN_KEYS: &[&str] = &["access_token", "id_token", "account_id"];

    let mut sanitized = Map::new();
    let mut tokens = Map::new();

    if let Some(source_tokens) = value.get_mut("tokens").and_then(Value::as_object_mut) {
        for key in TOKEN_KEYS {
            if let Some(token_value) = source_tokens.remove(*key) {
                tokens.insert((*key).to_string(), token_value);
            }
        }
    }
    if !tokens.is_empty() {
        sanitized.insert("tokens".to_string(), Value::Object(tokens));
    }

    for key in TOKEN_KEYS {
        if let Some(root_value) = value.get_mut(*key) {
            sanitized.insert((*key).to_string(), root_value.take());
        }
    }

    if let Some(last_refresh) = value.get_mut("last_refresh") {
        sanitized.insert("last_refresh".to_string(), last_refresh.take());
    }

    Value::Object(sanitized)
}

fn ensure_last_refresh(value: &mut Value) {
    if json::string_at(value, &["last_refresh"]).is_some() {
        return;
    }
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "last_refresh".to_string(),
            Value::String(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()),
        );
    }
}

fn has_oauth_access_token(value: &Value) -> bool {
    has_non_empty_string(value, &["tokens", "access_token"])
        || has_non_empty_string(value, &["access_token"])
}

fn has_non_empty_string(value: &Value, path: &[&str]) -> bool {
    json::string_at(value, path)
        .map(|value| !value.is_empty())
        .unwrap_or(false)
}

fn is_valid_ssh_host(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('-')
        && !host
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '\'' | '"' | '`' | '$' | ';' | '&' | '|'))
}

fn is_valid_secret_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !auth::is_invalid_secret_target(name)
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::{is_valid_secret_name, is_valid_ssh_host, sanitize_access_only};

    #[test]
    fn remote_sanitize_access_only_keeps_only_access_fields() {
        let value = serde_json::json!({
            "OPENAI_API_KEY": "sk-secret",
            "refresh_token": "top",
            "tokens": {
                "access_token": "access",
                "id_token": "id",
                "account_id": "acct",
                "api_key": "token-api-key",
                "refresh_token": "nested"
            },
            "last_refresh": "2025-01-20T12:34:56Z",
            "other": [{"refresh_token": "array"}]
        });

        let sanitized = sanitize_access_only(value);

        assert!(sanitized.get("refresh_token").is_none());
        assert!(sanitized["tokens"].get("refresh_token").is_none());
        assert!(sanitized.get("OPENAI_API_KEY").is_none());
        assert!(sanitized["tokens"].get("api_key").is_none());
        assert!(sanitized.get("other").is_none());
        assert_eq!(sanitized["tokens"]["access_token"], "access");
        assert_eq!(sanitized["tokens"]["id_token"], "id");
        assert_eq!(sanitized["tokens"]["account_id"], "acct");
        assert_eq!(sanitized["last_refresh"], "2025-01-20T12:34:56Z");
    }

    #[test]
    fn remote_validates_ssh_host_and_secret_name() {
        assert!(is_valid_ssh_host("auth-host"));
        assert!(is_valid_ssh_host("operator@auth-host"));
        assert!(!is_valid_ssh_host(""));
        assert!(!is_valid_ssh_host("-oProxyCommand=bad"));
        assert!(!is_valid_ssh_host("auth-host;bad"));
        assert!(!is_valid_ssh_host("auth-host bad"));

        assert!(is_valid_secret_name("team"));
        assert!(is_valid_secret_name("team.json"));
        assert!(!is_valid_secret_name(""));
        assert!(!is_valid_secret_name("-bad"));
        assert!(!is_valid_secret_name("../bad"));
        assert!(!is_valid_secret_name(r"a\bad"));
        assert!(!is_valid_secret_name("a bad"));
        assert!(!is_valid_secret_name("a;bad"));
        assert!(!is_valid_secret_name("a$bad"));
        assert!(!is_valid_secret_name("a`bad"));
    }
}
