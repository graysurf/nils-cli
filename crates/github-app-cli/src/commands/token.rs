//! `token` subcommand payload.

use serde::Serialize;

use crate::github::InstallationToken;

/// Non-secret metadata about a minted installation token.
///
/// This is the JSON payload for the `token` command. It deliberately OMITS the
/// raw token: the workspace output contract forbids secret material in JSON
/// (`docs/specs/cli-output-contract-v1.md`). The token is only ever written to
/// stdout in text mode, for `GH_TOKEN=$(github-app-cli token ...)` capture.
#[derive(Debug, Clone, Serialize)]
pub struct TokenMetadata {
    /// Always `"installation"` — the GitHub App installation token kind.
    pub token_type: &'static str,
    /// RFC 3339 expiry timestamp from GitHub (tokens last ~1 hour).
    pub expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_selection: Option<String>,
    /// Effective permissions granted to the token.
    pub permissions: serde_json::Value,
}

impl TokenMetadata {
    /// Project the non-secret fields out of a minted [`InstallationToken`].
    pub fn from_token(token: &InstallationToken) -> Self {
        Self {
            token_type: "installation",
            expires_at: token.expires_at.clone(),
            repository_selection: token.repository_selection.clone(),
            permissions: token.permissions.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_metadata_never_contains_the_raw_token() {
        let raw = InstallationToken {
            token: "ghs_SUPERSECRETTOKENVALUE".to_string(),
            expires_at: "2026-06-18T23:49:56Z".to_string(),
            repository_selection: Some("all".to_string()),
            permissions: serde_json::json!({ "contents": "write" }),
        };
        let meta = TokenMetadata::from_token(&raw);
        let json = serde_json::to_string(&meta).expect("serialize metadata");

        assert!(
            !json.contains("ghs_"),
            "token metadata JSON must not leak the raw token: {json}"
        );
        assert!(!json.contains("SUPERSECRET"), "no secret substring: {json}");
        assert!(json.contains("\"expires_at\":\"2026-06-18T23:49:56Z\""));
        assert!(json.contains("\"token_type\":\"installation\""));
    }
}
