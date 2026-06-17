use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::diag_output;

pub const AUTH_SCHEMA_VERSION: &str = "codex-cli.auth.v1";

#[derive(Debug, Clone, Serialize)]
pub struct AuthLoginResult {
    pub method: String,
    pub provider: String,
    pub completed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthUseResult {
    pub target: String,
    pub matched_secret: Option<String>,
    pub applied: bool,
    pub auth_file: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthSaveResult {
    pub auth_file: String,
    pub target_file: String,
    pub saved: bool,
    pub overwritten: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthRemoveResult {
    pub target_file: String,
    pub removed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthRefreshResult {
    pub target_file: String,
    pub refreshed: bool,
    pub synced: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refreshed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthAutoRefreshTargetResult {
    pub target_file: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthAutoRefreshResult {
    pub enabled: bool,
    pub refreshed: i64,
    pub skipped: i64,
    pub failed: i64,
    pub min_age_days: i64,
    pub targets: Vec<AuthAutoRefreshTargetResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthStatusResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_file: Option<String>,
    pub exists: bool,
    pub readable: bool,
    pub parse_ok: bool,
    pub authenticated: bool,
    pub prompt_segment_authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_kind: Option<String>,
    pub has_oauth_access_token: bool,
    pub has_oauth_refresh_token: bool,
    pub has_api_key: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_mode: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthCurrentResult {
    pub auth_file: String,
    pub matched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthSyncResult {
    pub auth_file: String,
    pub synced: usize,
    pub skipped: usize,
    pub failed: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub updated_files: Vec<String>,
}

pub fn emit_result<T: Serialize>(command: &str, result: T) -> Result<()> {
    diag_output::emit_success_result(AUTH_SCHEMA_VERSION, command, result)
}

pub fn emit_error(
    command: &str,
    code: &str,
    message: impl Into<String>,
    details: Option<Value>,
) -> Result<()> {
    diag_output::emit_error(AUTH_SCHEMA_VERSION, command, code, message, details)
}
