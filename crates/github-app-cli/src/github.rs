//! Minimal GitHub REST client for App auth: list installations and mint
//! installation access tokens. All calls are blocking and authenticated with a
//! short-lived App JWT (see [`crate::jwt`]).

use std::time::Duration;

use serde::Deserialize;

use crate::error::CommandError;

const USER_AGENT: &str = concat!("nils-github-app-cli/", env!("CARGO_PKG_VERSION"));
const API_VERSION: &str = "2022-11-28";

/// Response from `POST /app/installations/{id}/access_tokens`.
#[derive(Debug, Clone, Deserialize)]
pub struct InstallationToken {
    pub token: String,
    pub expires_at: String,
    #[serde(default)]
    pub repository_selection: Option<String>,
    #[serde(default)]
    pub permissions: serde_json::Value,
}

/// One element of `GET /app/installations`.
#[derive(Debug, Clone, Deserialize)]
pub struct Installation {
    pub id: i64,
    #[serde(default)]
    pub account: Option<Account>,
    #[serde(default)]
    pub repository_selection: Option<String>,
    #[serde(default)]
    pub permissions: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub login: String,
}

/// Thin blocking client over the GitHub REST API base URL.
pub struct Client {
    http: reqwest::blocking::Client,
    api_base: String,
}

impl Client {
    /// Build a client targeting `api_base` (e.g. `https://api.github.com`).
    pub fn new(api_base: &str) -> Result<Self, CommandError> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| {
                CommandError::unavailable("http-client", format!("build HTTP client: {e}"))
            })?;
        Ok(Self {
            http,
            api_base: api_base.trim_end_matches('/').to_string(),
        })
    }

    /// `GET /app/installations`, following `Link: rel="next"` pagination so an
    /// App with more than one page of installations is not silently truncated.
    pub fn list_installations(&self, jwt: &str) -> Result<Vec<Installation>, CommandError> {
        let mut next = Some(format!("{}/app/installations?per_page=100", self.api_base));
        let mut all = Vec::new();
        while let Some(url) = next {
            let resp = self.send(self.http.get(&url), jwt)?;
            next = next_link(resp.headers());
            let page = resp.json::<Vec<Installation>>().map_err(|e| {
                CommandError::unavailable("decode", format!("decode installations response: {e}"))
            })?;
            all.extend(page);
        }
        Ok(all)
    }

    /// `POST /app/installations/{installation_id}/access_tokens`.
    pub fn mint_installation_token(
        &self,
        jwt: &str,
        installation_id: &str,
    ) -> Result<InstallationToken, CommandError> {
        let url = format!(
            "{}/app/installations/{}/access_tokens",
            self.api_base, installation_id
        );
        let resp = self.send(self.http.post(&url), jwt)?;
        resp.json::<InstallationToken>()
            .map_err(|e| CommandError::unavailable("decode", format!("decode token response: {e}")))
    }

    fn send(
        &self,
        req: reqwest::blocking::RequestBuilder,
        jwt: &str,
    ) -> Result<reqwest::blocking::Response, CommandError> {
        let resp = req
            .bearer_auth(jwt)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .send()
            .map_err(|e| CommandError::unavailable("network", format!("request failed: {e}")))?;

        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        // Surface GitHub's own error message (a status string, never our token).
        let body = resp.text().unwrap_or_default();
        let detail = github_message(&body)
            .unwrap_or_else(|| format!("GitHub API returned HTTP {}", status.as_u16()));
        Err(CommandError::unavailable(
            "github-api",
            format!("HTTP {}: {detail}", status.as_u16()),
        ))
    }
}

/// Extract the `message` field from a GitHub JSON error body, if present.
fn github_message(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("message")?
        .as_str()
        .map(str::to_string)
}

/// The `rel="next"` URL from a response's `Link` header, if any.
fn next_link(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let link = headers.get(reqwest::header::LINK)?.to_str().ok()?;
    parse_next_link(link)
}

/// Parse a GitHub `Link` header value and return the `rel="next"` URL.
///
/// Example value: `<https://api.github.com/app/installations?page=2>; rel="next",
/// <https://api.github.com/app/installations?page=5>; rel="last"`.
fn parse_next_link(link: &str) -> Option<String> {
    for part in link.split(',') {
        let mut segments = part.split(';');
        let url_segment = match segments.next() {
            Some(s) => s.trim(),
            None => continue,
        };
        if segments.any(|s| s.trim() == "rel=\"next\"") {
            return Some(
                url_segment
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string(),
            );
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_next_link_picks_the_next_rel() {
        let header = "<https://api.github.com/app/installations?per_page=100&page=2>; rel=\"next\", \
<https://api.github.com/app/installations?per_page=100&page=5>; rel=\"last\"";
        assert_eq!(
            parse_next_link(header).as_deref(),
            Some("https://api.github.com/app/installations?per_page=100&page=2")
        );
    }

    #[test]
    fn parse_next_link_is_none_without_a_next_rel() {
        let header = "<https://api.github.com/app/installations?page=1>; rel=\"prev\", \
<https://api.github.com/app/installations?page=1>; rel=\"first\"";
        assert!(parse_next_link(header).is_none());
    }
}
