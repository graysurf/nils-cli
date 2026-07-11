use nils_common::env as shared_env;
use nils_common::provider_usage::{ProviderUsageReason, classify_http_failure};
use reqwest::blocking::Client;
use std::fmt;
use std::time::Duration;

const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const DEFAULT_ANTHROPIC_BETA: &str = "oauth-2025-04-20";
const DEFAULT_USER_AGENT: &str = "claude-code/2.1.0";

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
            "Claude usage request failed ({})",
            self.reason.as_str()
        )
    }
}

impl std::error::Error for UsageFetchError {}

pub fn fetch_usage(access_token: &str) -> Result<String, UsageFetchError> {
    let endpoint = shared_env::env_non_empty("CLAUDE_PROMPT_SEGMENT_ENDPOINT")
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
    let max_time_seconds = env_u64("CLAUDE_PROMPT_SEGMENT_MAX_TIME_SECONDS", 5);
    let user_agent = shared_env::env_non_empty("CLAUDE_PROMPT_SEGMENT_USER_AGENT")
        .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string());
    let anthropic_beta = shared_env::env_non_empty("CLAUDE_PROMPT_SEGMENT_ANTHROPIC_BETA")
        .unwrap_or_else(|| DEFAULT_ANTHROPIC_BETA.to_string());

    let client = Client::builder()
        .timeout(Duration::from_secs(max_time_seconds))
        .build()
        .map_err(|_| UsageFetchError::new(ProviderUsageReason::Unknown))?;

    let resp = client
        .get(&endpoint)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("anthropic-beta", anthropic_beta)
        .header("User-Agent", user_agent)
        .header("Accept", "application/json")
        .send()
        .map_err(|error| {
            UsageFetchError::new(if error.is_timeout() {
                ProviderUsageReason::Timeout
            } else {
                ProviderUsageReason::ServiceUnavailable
            })
        })?;

    let status = resp.status().as_u16();
    let body = resp.text().unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(UsageFetchError::new(classify_http_failure(status, &body)));
    }

    Ok(body)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}
