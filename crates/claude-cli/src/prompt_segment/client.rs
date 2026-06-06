use anyhow::{Context, Result};
use nils_common::env as shared_env;
use reqwest::blocking::Client;
use std::time::Duration;

const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const DEFAULT_ANTHROPIC_BETA: &str = "oauth-2025-04-20";
const DEFAULT_USER_AGENT: &str = "claude-code/2.1.0";

pub fn fetch_usage(access_token: &str) -> Result<String> {
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
        .context("failed to build HTTP client")?;

    let resp = client
        .get(&endpoint)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("anthropic-beta", anthropic_beta)
        .header("User-Agent", user_agent)
        .header("Accept", "application/json")
        .send()
        .with_context(|| format!("claude prompt-segment usage request failed: {endpoint}"))?;

    let status = resp.status().as_u16();
    let body = resp.text().unwrap_or_default();
    if !(200..300).contains(&status) {
        let preview = body
            .chars()
            .take(200)
            .collect::<String>()
            .replace(['\n', '\r'], " ");
        if preview.is_empty() {
            anyhow::bail!("claude prompt-segment GET {endpoint} failed (HTTP {status})");
        }
        anyhow::bail!("claude prompt-segment GET {endpoint} failed (HTTP {status}): {preview}");
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
