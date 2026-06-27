use anyhow::Result;
use chrono::Utc;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

#[derive(Debug, Clone)]
pub struct RemoteAccessOnlyPayload {
    pub auth: Value,
    pub config: ConfiguredRemotePull,
}

struct RemoteExportSuccess {
    output: Output,
    refresh_attempted: bool,
    refresh_fallback: bool,
    refresh_error_code: Option<String>,
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
    configured_pull_from_env_for_name(None)
}

pub fn configured_pull_for_target_from_env(
    target_file: &Path,
) -> std::result::Result<Option<ConfiguredRemotePull>, RemoteEnvError> {
    configured_pull_from_env_for_name(infer_secret_name_for_target(target_file))
}

fn configured_pull_from_env_for_name(
    inferred_name: Option<String>,
) -> std::result::Result<Option<ConfiguredRemotePull>, RemoteEnvError> {
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
    let Some(name) = inferred_name.or(name) else {
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

pub fn export_access_only_for_target_from_env(
    target_file: &Path,
) -> Result<Option<RemoteAccessOnlyPayload>> {
    let Some(config) = configured_pull_for_target_from_env(target_file)
        .map_err(|err| anyhow::anyhow!(err.message))?
    else {
        return Ok(None);
    };

    let remote_export =
        match run_remote_export_with_fallback(&config.ssh, &config.name, config.refresh) {
            Ok(success) => success,
            Err(failure) => anyhow::bail!(failure.message),
        };

    let mut imported = sanitize_remote_output(&remote_export.output, &config.ssh, &config.name)
        .map_err(|failure| anyhow::anyhow!(failure.message))?;
    ensure_last_refresh(&mut imported);
    ensure_access_only_refresh_placeholder(&mut imported);

    Ok(Some(RemoteAccessOnlyPayload {
        auth: imported,
        config,
    }))
}

pub fn pull_access_only_to_active(
    ssh_host: &str,
    name: &str,
    refresh: bool,
) -> Result<std::result::Result<AuthRemotePullResult, RemotePullFailure>> {
    let remote_export = match run_remote_export_with_fallback(ssh_host, name, refresh) {
        Ok(success) => success,
        Err(failure) => return Ok(Err(failure)),
    };
    let mut imported = match sanitize_remote_output(&remote_export.output, ssh_host, name) {
        Ok(value) => value,
        Err(failure) => return Ok(Err(failure)),
    };
    ensure_last_refresh(&mut imported);
    ensure_access_only_refresh_placeholder(&mut imported);

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
        has_oauth_refresh_token: has_real_oauth_refresh_token(&imported),
        remote_refresh_attempted: remote_export.refresh_attempted.then_some(true),
        remote_refresh_fallback: remote_export.refresh_fallback.then_some(true),
        remote_refresh_error_code: remote_export.refresh_error_code,
    }))
}

fn sanitize_remote_output(
    output: &Output,
    ssh_host: &str,
    name: &str,
) -> std::result::Result<Value, RemotePullFailure> {
    let imported: Value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(_) => {
            return Err(RemotePullFailure {
                code: "remote-export-invalid-json",
                message: "codex-remote-pull: remote export returned invalid JSON".to_string(),
                details: Some(serde_json::json!({
                    "ssh": ssh_host,
                    "name": name,
                })),
                exit_code: 1,
            });
        }
    };
    let imported = sanitize_access_only(imported);
    if !has_oauth_access_token(&imported) {
        return Err(RemotePullFailure {
            code: "remote-export-missing-access-token",
            message: "codex-remote-pull: remote export did not include an OAuth access token"
                .to_string(),
            details: Some(serde_json::json!({
                "ssh": ssh_host,
                "name": name,
            })),
            exit_code: 1,
        });
    }
    Ok(imported)
}

fn run_remote_export_with_fallback(
    ssh_host: &str,
    name: &str,
    refresh: bool,
) -> std::result::Result<RemoteExportSuccess, RemotePullFailure> {
    match run_remote_export(ssh_host, name, refresh) {
        Ok(output) if output.status.success() => Ok(RemoteExportSuccess {
            output,
            refresh_attempted: refresh,
            refresh_fallback: false,
            refresh_error_code: None,
        }),
        Ok(output) => {
            let primary_failure = remote_export_status_failure(ssh_host, name, &output);
            if !refresh {
                return Err(primary_failure);
            }

            match run_remote_export(ssh_host, name, false) {
                Ok(fallback_output) if fallback_output.status.success() => {
                    Ok(RemoteExportSuccess {
                        output: fallback_output,
                        refresh_attempted: true,
                        refresh_fallback: true,
                        refresh_error_code: Some(primary_failure.code.to_string()),
                    })
                }
                Ok(_) | Err(_) => Err(primary_failure),
            }
        }
        Err(failure) => Err(failure),
    }
}

fn run_remote_export(
    ssh_host: &str,
    name: &str,
    refresh: bool,
) -> std::result::Result<Output, RemotePullFailure> {
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

    match command.output() {
        Ok(output) => Ok(output),
        Err(err) => Err(RemotePullFailure {
            code: "ssh-exec-failed",
            message: format!("codex-remote-pull: failed to run ssh: {err}"),
            details: None,
            exit_code: 1,
        }),
    }
}

fn remote_export_status_failure(ssh_host: &str, name: &str, output: &Output) -> RemotePullFailure {
    let exit_code = output.status.code().unwrap_or(1);
    RemotePullFailure {
        code: "remote-export-failed",
        message: format!("codex-remote-pull: remote export failed (exit {exit_code})"),
        details: Some(serde_json::json!({
            "ssh": ssh_host,
            "name": name,
            "exit_code": exit_code,
        })),
        exit_code: 1,
    }
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
        if let Err(err) = sync_matching_active_from_secret(&target) {
            eprintln!(
                "codex-remote-export: failed to sync refreshed secret into matching active auth: {err}"
            );
            return Ok(6);
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

fn sync_matching_active_from_secret(secret_file: &Path) -> Result<bool> {
    let Some(auth_file) = paths::resolve_auth_file() else {
        return Ok(false);
    };
    if !auth_file.is_file() || !secret_file.is_file() {
        return Ok(false);
    }

    let Some(secret_key) = auth::identity_key_from_auth_file(secret_file)
        .ok()
        .flatten()
    else {
        return Ok(false);
    };
    let Some(active_key) = auth::identity_key_from_auth_file(&auth_file).ok().flatten() else {
        return Ok(false);
    };
    if active_key != secret_key {
        return Ok(false);
    }

    let active_hash = fs::sha256_file(&auth_file)?;
    let secret_hash = fs::sha256_file(secret_file)?;
    if active_hash == secret_hash {
        write_active_timestamp_from_auth(&auth_file)?;
        return Ok(false);
    }

    let contents = std::fs::read(secret_file)?;
    fs::write_atomic(&auth_file, &contents, fs::SECRET_FILE_MODE)?;
    write_active_timestamp_from_auth(&auth_file)?;
    Ok(true)
}

fn write_active_timestamp_from_auth(auth_file: &Path) -> Result<()> {
    let Some(timestamp_path) = paths::resolve_secret_timestamp_path(auth_file) else {
        return Ok(());
    };
    let iso = auth::last_refresh_from_auth_file(auth_file).unwrap_or(None);
    fs::write_timestamp(&timestamp_path, iso.as_deref())?;
    Ok(())
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

fn infer_secret_name_for_target(target_file: &Path) -> Option<String> {
    if let Some(secret_dir) = paths::resolve_secret_dir()
        && let Some(file_name) = target_file.file_name().and_then(|name| name.to_str())
        && secret_dir.join(file_name) == target_file
    {
        return secret_name_from_file_name(file_name);
    }

    if let Some(auth_file) = paths::resolve_auth_file()
        && auth_file == target_file
    {
        return matching_secret_name_for_auth(target_file);
    }

    None
}

fn matching_secret_name_for_auth(auth_file: &Path) -> Option<String> {
    let secret_dir = paths::resolve_secret_dir()?;
    let mut secret_files = std::fs::read_dir(&secret_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    secret_files.sort();

    if let Ok(auth_hash) = fs::sha256_file(auth_file) {
        for secret_file in &secret_files {
            if fs::sha256_file(secret_file).ok().as_deref() == Some(auth_hash.as_str())
                && let Some(name) = secret_file.file_name().and_then(|value| value.to_str())
                && let Some(secret_name) = secret_name_from_file_name(name)
            {
                return Some(secret_name);
            }
        }
    }

    let auth_key = auth::identity_key_from_auth_file(auth_file)
        .ok()
        .flatten()?;
    for secret_file in &secret_files {
        if auth::identity_key_from_auth_file(secret_file)
            .ok()
            .flatten()
            .as_deref()
            == Some(auth_key.as_str())
            && let Some(name) = secret_file.file_name().and_then(|value| value.to_str())
            && let Some(secret_name) = secret_name_from_file_name(name)
        {
            return Some(secret_name);
        }
    }

    None
}

fn secret_name_from_file_name(file_name: &str) -> Option<String> {
    let secret_name = file_name.strip_suffix(".json")?;
    if is_valid_secret_name(secret_name) {
        Some(secret_name.to_string())
    } else {
        None
    }
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

fn ensure_access_only_refresh_placeholder(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let tokens = object
        .entry("tokens")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(tokens_object) = tokens.as_object_mut() else {
        return;
    };
    tokens_object.insert(
        "refresh_token".to_string(),
        Value::String(auth::ACCESS_ONLY_REFRESH_TOKEN_PLACEHOLDER.to_string()),
    );
}

fn has_oauth_access_token(value: &Value) -> bool {
    has_non_empty_string(value, &["tokens", "access_token"])
        || has_non_empty_string(value, &["access_token"])
}

fn has_real_oauth_refresh_token(value: &Value) -> bool {
    has_real_refresh_token(value, &["tokens", "refresh_token"])
        || has_real_refresh_token(value, &["refresh_token"])
}

fn has_real_refresh_token(value: &Value, path: &[&str]) -> bool {
    json::string_at(value, path)
        .map(|value| auth::is_real_refresh_token(&value))
        .unwrap_or(false)
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
    use super::{
        infer_secret_name_for_target, is_valid_secret_name, is_valid_ssh_host,
        sanitize_access_only, sync_matching_active_from_secret,
    };
    use nils_test_support::{EnvGuard, GlobalStateLock};

    const HEADER: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
    const PAYLOAD_ALPHA: &str = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlcl8xMjMiLCJlbWFpbCI6ImFscGhhQGV4YW1wbGUuY29tIn19";
    const PAYLOAD_BETA: &str = "eyJzdWIiOiJ1c2VyXzQ1NiIsImVtYWlsIjoiYmV0YUBleGFtcGxlLmNvbSIsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X3VzZXJfaWQiOiJ1c2VyXzQ1NiIsImVtYWlsIjoiYmV0YUBleGFtcGxlLmNvbSJ9fQ";

    fn token(payload: &str) -> String {
        format!("{HEADER}.{payload}.sig")
    }

    fn auth_json(payload: &str, refresh_token: &str, last_refresh: &str) -> String {
        format!(
            r#"{{"tokens":{{"access_token":"{}","id_token":"{}","refresh_token":"{}","account_id":"acct_001"}},"last_refresh":"{}"}}"#,
            token(payload),
            token(payload),
            refresh_token,
            last_refresh
        )
    }

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

    #[test]
    fn remote_infers_secret_name_for_secret_file_target() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secrets = dir.path().join("secrets");
        std::fs::create_dir_all(&secrets).expect("secrets dir");
        let _secret_dir = EnvGuard::set(
            &lock,
            "CODEX_SECRET_DIR",
            secrets.to_string_lossy().as_ref(),
        );

        assert_eq!(
            infer_secret_name_for_target(&secrets.join("sym.json")).as_deref(),
            Some("sym")
        );
    }

    #[test]
    fn remote_sync_matching_active_from_secret_updates_same_identity() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let auth_file = dir.path().join("auth.json");
        let secret_file = dir.path().join("gamania.json");
        let cache_dir = dir.path().join("cache");
        std::fs::create_dir_all(&cache_dir).expect("cache");

        let active = auth_json(PAYLOAD_ALPHA, "refresh_old", "2025-01-19T12:34:56Z");
        let refreshed = auth_json(PAYLOAD_ALPHA, "refresh_new", "2025-01-20T12:34:56Z");
        std::fs::write(&auth_file, active).expect("active auth");
        std::fs::write(&secret_file, &refreshed).expect("secret");

        let _auth = EnvGuard::set(
            &lock,
            "CODEX_AUTH_FILE",
            auth_file.to_string_lossy().as_ref(),
        );
        let _cache = EnvGuard::set(
            &lock,
            "CODEX_SECRET_CACHE_DIR",
            cache_dir.to_string_lossy().as_ref(),
        );

        assert!(sync_matching_active_from_secret(&secret_file).expect("sync"));

        assert_eq!(
            std::fs::read_to_string(&auth_file).expect("read active"),
            refreshed
        );
        assert_eq!(
            std::fs::read_to_string(cache_dir.join("auth.json.timestamp")).expect("read timestamp"),
            "2025-01-20T12:34:56Z"
        );
    }

    #[test]
    fn remote_sync_matching_active_from_secret_skips_different_identity() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let auth_file = dir.path().join("auth.json");
        let secret_file = dir.path().join("gamania.json");

        let active = auth_json(PAYLOAD_BETA, "refresh_beta", "2025-01-19T12:34:56Z");
        let refreshed = auth_json(PAYLOAD_ALPHA, "refresh_new", "2025-01-20T12:34:56Z");
        std::fs::write(&auth_file, &active).expect("active auth");
        std::fs::write(&secret_file, refreshed).expect("secret");

        let _auth = EnvGuard::set(
            &lock,
            "CODEX_AUTH_FILE",
            auth_file.to_string_lossy().as_ref(),
        );

        assert!(!sync_matching_active_from_secret(&secret_file).expect("sync"));
        assert_eq!(
            std::fs::read_to_string(&auth_file).expect("read active"),
            active
        );
    }
}
