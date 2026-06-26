use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::auth;
use crate::auth::output::{self, AuthStatusResult};
use crate::json;
use crate::paths;
use nils_common::fs;

#[derive(Debug, Clone)]
pub struct ActiveAuthStatus {
    pub auth_file: Option<PathBuf>,
    pub exists: bool,
    pub readable: bool,
    pub parse_ok: bool,
    pub authenticated: bool,
    pub prompt_segment_authenticated: bool,
    pub auth_kind: Option<AuthKind>,
    pub has_oauth_access_token: bool,
    pub has_oauth_refresh_token: bool,
    pub has_api_key: bool,
    pub last_refresh: Option<String>,
    pub identity: Option<String>,
    pub matched_secret: Option<String>,
    pub match_mode: Option<SecretMatchMode>,
    pub reason: AuthStatusReason,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthKind {
    ChatgptOauth,
    OpenaiApiKey,
}

impl AuthKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ChatgptOauth => "chatgpt-oauth",
            Self::OpenaiApiKey => "openai-api-key",
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SecretMatchMode {
    Exact,
    Identity,
}

impl SecretMatchMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Identity => "identity",
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum AuthStatusReason {
    Ready,
    AuthFileNotConfigured,
    AuthFileNotFound,
    AuthFileReadFailed,
    AuthFileInvalidJson,
    CredentialsMissing,
}

impl AuthStatusReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::AuthFileNotConfigured => "auth-file-not-configured",
            Self::AuthFileNotFound => "auth-file-not-found",
            Self::AuthFileReadFailed => "auth-file-read-failed",
            Self::AuthFileInvalidJson => "auth-file-invalid-json",
            Self::CredentialsMissing => "credentials-missing",
        }
    }
}

pub fn run() -> Result<i32> {
    run_with_json(false)
}

pub fn run_with_json(output_json: bool) -> Result<i32> {
    let status = inspect_active_auth();
    if output_json {
        output::emit_result("auth status", AuthStatusResult::from(&status))?;
    } else {
        print_text_status(&status);
    }
    Ok(0)
}

pub fn inspect_active_auth() -> ActiveAuthStatus {
    let Some(auth_file) = paths::resolve_auth_file() else {
        return ActiveAuthStatus {
            auth_file: None,
            exists: false,
            readable: false,
            parse_ok: false,
            authenticated: false,
            prompt_segment_authenticated: false,
            auth_kind: None,
            has_oauth_access_token: false,
            has_oauth_refresh_token: false,
            has_api_key: false,
            last_refresh: None,
            identity: None,
            matched_secret: None,
            match_mode: None,
            reason: AuthStatusReason::AuthFileNotConfigured,
        };
    };

    if !auth_file.is_file() {
        return inactive_with_file(auth_file, AuthStatusReason::AuthFileNotFound);
    }

    let raw = match std::fs::read_to_string(&auth_file) {
        Ok(raw) => raw,
        Err(_) => {
            let mut status = inactive_with_file(auth_file, AuthStatusReason::AuthFileReadFailed);
            status.exists = true;
            return status;
        }
    };

    let value: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => {
            let mut status = inactive_with_file(auth_file, AuthStatusReason::AuthFileInvalidJson);
            status.exists = true;
            status.readable = true;
            return status;
        }
    };

    let has_oauth_access_token = has_non_empty_string(&value, &["tokens", "access_token"])
        || has_non_empty_string(&value, &["access_token"]);
    let has_oauth_refresh_token = has_real_refresh_token(&value, &["tokens", "refresh_token"])
        || has_real_refresh_token(&value, &["refresh_token"]);
    let has_api_key = has_non_empty_string(&value, &["OPENAI_API_KEY"])
        || has_non_empty_string(&value, &["api_key"])
        || has_non_empty_string(&value, &["openai_api_key"])
        || has_non_empty_string(&value, &["tokens", "api_key"])
        || has_non_empty_string(&value, &["tokens", "openai_api_key"]);

    let auth_kind = if has_oauth_access_token || has_oauth_refresh_token {
        Some(AuthKind::ChatgptOauth)
    } else if has_api_key {
        Some(AuthKind::OpenaiApiKey)
    } else {
        None
    };

    let authenticated = auth_kind.is_some();
    let prompt_segment_authenticated = has_oauth_access_token;
    let (matched_secret, match_mode) = inspect_matching_secret(&auth_file);

    ActiveAuthStatus {
        auth_file: Some(auth_file.clone()),
        exists: true,
        readable: true,
        parse_ok: true,
        authenticated,
        prompt_segment_authenticated,
        auth_kind,
        has_oauth_access_token,
        has_oauth_refresh_token,
        has_api_key,
        last_refresh: json::string_at(&value, &["last_refresh"]),
        identity: auth::identity_from_auth_file(&auth_file).ok().flatten(),
        matched_secret,
        match_mode,
        reason: if authenticated {
            AuthStatusReason::Ready
        } else {
            AuthStatusReason::CredentialsMissing
        },
    }
}

impl AuthStatusResult {
    pub fn from(status: &ActiveAuthStatus) -> Self {
        Self {
            auth_file: status
                .auth_file
                .as_ref()
                .map(|path| path.display().to_string()),
            exists: status.exists,
            readable: status.readable,
            parse_ok: status.parse_ok,
            authenticated: status.authenticated,
            prompt_segment_authenticated: status.prompt_segment_authenticated,
            auth_kind: status.auth_kind.map(|kind| kind.as_str().to_string()),
            has_oauth_access_token: status.has_oauth_access_token,
            has_oauth_refresh_token: status.has_oauth_refresh_token,
            has_api_key: status.has_api_key,
            last_refresh: status.last_refresh.clone(),
            identity: status.identity.clone(),
            matched_secret: status.matched_secret.clone(),
            match_mode: status.match_mode.map(|mode| mode.as_str().to_string()),
            reason: status.reason.as_str().to_string(),
        }
    }
}

fn inactive_with_file(auth_file: PathBuf, reason: AuthStatusReason) -> ActiveAuthStatus {
    ActiveAuthStatus {
        auth_file: Some(auth_file),
        exists: false,
        readable: false,
        parse_ok: false,
        authenticated: false,
        prompt_segment_authenticated: false,
        auth_kind: None,
        has_oauth_access_token: false,
        has_oauth_refresh_token: false,
        has_api_key: false,
        last_refresh: None,
        identity: None,
        matched_secret: None,
        match_mode: None,
        reason,
    }
}

fn has_non_empty_string(value: &Value, path: &[&str]) -> bool {
    json::string_at(value, path)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn has_real_refresh_token(value: &Value, path: &[&str]) -> bool {
    json::string_at(value, path)
        .map(|value| auth::is_real_refresh_token(value.trim()))
        .unwrap_or(false)
}

fn inspect_matching_secret(auth_file: &Path) -> (Option<String>, Option<SecretMatchMode>) {
    let Some(secret_dir) = paths::resolve_secret_dir() else {
        return (None, None);
    };
    let Ok(entries) = std::fs::read_dir(secret_dir) else {
        return (None, None);
    };

    let auth_key = auth::identity_key_from_auth_file(auth_file).ok().flatten();
    let auth_hash = fs::sha256_file(auth_file).ok();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        if let Some(key) = auth_key.as_deref()
            && let Ok(Some(candidate_key)) = auth::identity_key_from_auth_file(&path)
            && candidate_key == key
        {
            let mode = if hash_matches(auth_hash.as_deref(), &path) {
                SecretMatchMode::Exact
            } else {
                SecretMatchMode::Identity
            };
            return (Some(file_name(&path)), Some(mode));
        }

        if hash_matches(auth_hash.as_deref(), &path) {
            return (Some(file_name(&path)), Some(SecretMatchMode::Exact));
        }
    }

    (None, None)
}

fn hash_matches(expected: Option<&str>, path: &Path) -> bool {
    expected
        .zip(fs::sha256_file(path).ok())
        .map(|(expected, actual)| expected == actual)
        .unwrap_or(false)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string()
}

fn print_text_status(status: &ActiveAuthStatus) {
    let auth_file = status
        .auth_file
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<not configured>".to_string());
    let auth_kind = status.auth_kind.map(|kind| kind.as_str()).unwrap_or("none");
    let matched = status.matched_secret.as_deref().unwrap_or("none");
    println!(
        "codex: auth status authenticated={} kind={} prompt_segment_authenticated={} reason={} auth_file={} matched_secret={}",
        status.authenticated,
        auth_kind,
        status.prompt_segment_authenticated,
        status.reason.as_str(),
        auth_file,
        matched,
    );
}

#[cfg(test)]
mod tests {
    use super::{AuthKind, AuthStatusReason, has_non_empty_string, has_real_refresh_token};
    use crate::auth::ACCESS_ONLY_REFRESH_TOKEN_PLACEHOLDER;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn has_non_empty_string_rejects_missing_null_and_blank_values() {
        let value = json!({
            "a": { "present": "x", "blank": "   ", "null": null }
        });
        assert!(has_non_empty_string(&value, &["a", "present"]));
        assert!(!has_non_empty_string(&value, &["a", "blank"]));
        assert!(!has_non_empty_string(&value, &["a", "null"]));
        assert!(!has_non_empty_string(&value, &["a", "missing"]));
    }

    #[test]
    fn has_real_refresh_token_rejects_access_only_placeholder() {
        let value = json!({
            "tokens": {
                "refresh_token": ACCESS_ONLY_REFRESH_TOKEN_PLACEHOLDER
            },
            "real": {
                "refresh_token": "refresh-secret"
            }
        });

        assert!(!has_real_refresh_token(
            &value,
            &["tokens", "refresh_token"]
        ));
        assert!(has_real_refresh_token(&value, &["real", "refresh_token"]));
    }

    #[test]
    fn enum_string_contracts_are_stable() {
        assert_eq!(AuthKind::ChatgptOauth.as_str(), "chatgpt-oauth");
        assert_eq!(AuthKind::OpenaiApiKey.as_str(), "openai-api-key");
        assert_eq!(AuthStatusReason::Ready.as_str(), "ready");
    }
}
