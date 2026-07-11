use anyhow::Result;
use nils_common::provider_usage::{ProviderUsageReason, classify_http_failure};
use reqwest::blocking::Client;
use serde_json::Value;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::auth::remote;
use crate::json;
use crate::paths;

pub struct UsageResponse {
    pub body: String,
    pub json: Value,
}

pub struct UsageRequest {
    pub target_file: PathBuf,
    pub refresh_on_401: bool,
    pub suppress_auth_refresh_output: bool,
    pub base_url: String,
    pub connect_timeout_seconds: u64,
    pub max_time_seconds: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsageFetchError {
    reason: ProviderUsageReason,
}

impl UsageFetchError {
    fn new(reason: ProviderUsageReason) -> Self {
        Self { reason }
    }

    pub const fn reason(&self) -> ProviderUsageReason {
        self.reason
    }
}

impl fmt::Display for UsageFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "codex-rate-limits: usage request failed ({})",
            self.reason.as_str()
        )
    }
}

impl std::error::Error for UsageFetchError {}

pub fn fetch_usage(request: &UsageRequest) -> Result<UsageResponse, UsageFetchError> {
    let (access_token, account_id) = read_tokens(&request.target_file)
        .map_err(|_| UsageFetchError::new(ProviderUsageReason::AuthRequired))?;
    let mut response = send_request(request, &access_token, account_id.as_deref())?;

    if response.status == 401 && request.refresh_on_401 {
        let refreshed_tokens =
            refresh_target(&request.target_file, request.suppress_auth_refresh_output)
                .or_else(|| read_tokens(&request.target_file).ok());
        if let Some((access_token, account_id)) = refreshed_tokens {
            response = send_request(request, &access_token, account_id.as_deref())?;
        }
    }

    if response.status != 200 {
        return Err(UsageFetchError::new(classify_http_failure(
            response.status,
            &response.body,
        )));
    }

    let json: Value = serde_json::from_str(&response.body)
        .map_err(|_| UsageFetchError::new(ProviderUsageReason::Unknown))?;

    Ok(UsageResponse {
        body: response.body,
        json,
    })
}

pub fn read_tokens(target_file: &Path) -> Result<(String, Option<String>)> {
    let value = json::read_json(target_file)?;
    read_tokens_from_value(&value)
}

fn read_tokens_from_value(value: &Value) -> Result<(String, Option<String>)> {
    let access_token = json::string_at(value, &["tokens", "access_token"])
        .or_else(|| json::string_at(value, &["access_token"]))
        .ok_or_else(|| anyhow::anyhow!("missing access_token"))?;
    let account_id = json::string_at(value, &["tokens", "account_id"])
        .or_else(|| json::string_at(value, &["account_id"]));
    Ok((access_token, account_id))
}

fn send_request(
    request: &UsageRequest,
    access_token: &str,
    account_id: Option<&str>,
) -> Result<HttpResponse, UsageFetchError> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(request.connect_timeout_seconds))
        .timeout(Duration::from_secs(request.max_time_seconds))
        .build()
        .map_err(|_| UsageFetchError::new(ProviderUsageReason::Unknown))?;

    let url = format!("{}/wham/usage", request.base_url.trim_end_matches('/'));
    let mut req = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Accept", "application/json")
        .header("User-Agent", "codex-cli");
    if let Some(account_id) = account_id {
        req = req.header("ChatGPT-Account-Id", account_id);
    }

    let resp = req.send().map_err(|error| {
        UsageFetchError::new(if error.is_timeout() {
            ProviderUsageReason::Timeout
        } else {
            ProviderUsageReason::ServiceUnavailable
        })
    })?;

    let status = resp.status().as_u16();
    let body = resp.text().unwrap_or_default();
    Ok(HttpResponse { status, body })
}

fn refresh_target(target_file: &Path, suppress_output: bool) -> Option<(String, Option<String>)> {
    if let Some(auth_file) = paths::resolve_auth_file()
        && auth_file == target_file
    {
        let _ = if suppress_output {
            crate::auth::refresh::run_silent(&[])
        } else {
            crate::auth::refresh::run(&[])
        };
        return read_tokens(target_file).ok();
    }

    if let Some(secret_dir) = paths::resolve_secret_dir()
        && let Some(file_name) = target_file.file_name().and_then(|n| n.to_str())
    {
        let path = secret_dir.join(file_name);
        if path == target_file {
            if let Ok(Some(payload)) = remote::export_access_only_for_target_from_env(target_file)
                && let Ok(tokens) = read_tokens_from_value(&payload.auth)
            {
                return Some(tokens);
            }
            let args = [file_name.to_string()];
            let _ = if suppress_output {
                crate::auth::refresh::run_silent(&args)
            } else {
                crate::auth::refresh::run(&args)
            };
            return read_tokens(target_file).ok();
        }
    }

    None
}

struct HttpResponse {
    status: u16,
    body: String,
}
