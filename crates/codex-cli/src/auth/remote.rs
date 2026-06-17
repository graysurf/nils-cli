use anyhow::Result;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::auth;
use crate::auth::output::{self, AuthRemotePullResult};
use crate::json;
use crate::paths;
use nils_common::fs;

const COMMAND_PULL: &str = "auth remote pull";

pub fn pull_with_json(
    ssh_host: &str,
    name: &str,
    access_only: bool,
    write_active: bool,
    output_json: bool,
) -> Result<i32> {
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

    let remote_output = Command::new("ssh")
        .arg(ssh_host)
        .arg("codex-cli")
        .arg("auth")
        .arg("remote")
        .arg("export")
        .arg("--name")
        .arg(name)
        .arg("--access-only")
        .arg("--refresh")
        .output();

    let remote_output = match remote_output {
        Ok(output) => output,
        Err(err) => {
            return runtime_error(
                output_json,
                "ssh-exec-failed",
                format!("codex-remote-pull: failed to run ssh: {err}"),
                None,
            );
        }
    };

    if !remote_output.status.success() {
        let exit_code = remote_output.status.code().unwrap_or(1);
        return runtime_error(
            output_json,
            "remote-export-failed",
            format!("codex-remote-pull: remote export failed (exit {exit_code})"),
            Some(serde_json::json!({
                "ssh": ssh_host,
                "name": name,
                "exit_code": exit_code,
            })),
        );
    }

    let imported: Value = match serde_json::from_slice(&remote_output.stdout) {
        Ok(value) => value,
        Err(_) => {
            return runtime_error(
                output_json,
                "remote-export-invalid-json",
                "codex-remote-pull: remote export returned invalid JSON",
                Some(serde_json::json!({
                    "ssh": ssh_host,
                    "name": name,
                })),
            );
        }
    };
    let imported = sanitize_access_only(imported);

    let auth_file = match paths::resolve_auth_file() {
        Some(path) => path,
        None => {
            return runtime_error(
                output_json,
                "auth-file-not-configured",
                "codex-remote-pull: CODEX_AUTH_FILE is not configured",
                None,
            );
        }
    };

    write_active_auth(&auth_file, &imported)?;

    let result = AuthRemotePullResult {
        ssh: ssh_host.to_string(),
        name: name.to_string(),
        access_only,
        write_active,
        auth_file: auth_file.display().to_string(),
        has_oauth_access_token: has_non_empty_string(&imported, &["tokens", "access_token"])
            || has_non_empty_string(&imported, &["access_token"]),
        has_oauth_refresh_token: has_non_empty_string(&imported, &["tokens", "refresh_token"])
            || has_non_empty_string(&imported, &["refresh_token"]),
    };

    if output_json {
        output::emit_result(COMMAND_PULL, result)?;
    } else {
        println!(
            "codex-remote-pull: pulled access-only auth '{}' from {} into {}",
            name,
            ssh_host,
            auth_file.display()
        );
    }

    Ok(0)
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

fn runtime_error(
    output_json: bool,
    code: &str,
    message: impl Into<String>,
    details: Option<Value>,
) -> Result<i32> {
    let message = message.into();
    if output_json {
        output::emit_error(COMMAND_PULL, code, message, details)?;
    } else {
        eprintln!("{message}");
    }
    Ok(1)
}

fn write_active_auth(auth_file: &Path, value: &Value) -> Result<()> {
    let output = serde_json::to_vec(value)?;
    fs::write_atomic(auth_file, &output, fs::SECRET_FILE_MODE)?;

    if let Some(timestamp_path) = paths::resolve_secret_timestamp_path(auth_file) {
        let last_refresh = json::string_at(value, &["last_refresh"]);
        fs::write_timestamp(&timestamp_path, last_refresh.as_deref())?;
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
        assert!(is_valid_ssh_host("g14"));
        assert!(is_valid_ssh_host("terry@g14"));
        assert!(!is_valid_ssh_host(""));
        assert!(!is_valid_ssh_host("-oProxyCommand=bad"));
        assert!(!is_valid_ssh_host("g14;bad"));
        assert!(!is_valid_ssh_host("g14 bad"));

        assert!(is_valid_secret_name("gamania"));
        assert!(is_valid_secret_name("gamania.json"));
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
