use nils_common::env as shared_env;
use serde_json::Value;
use std::process::{Command, Stdio};

const DEFAULT_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenSource {
    AccessTokenEnv,
    CredentialsJsonEnv,
    Keychain,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessToken {
    pub value: String,
    pub source: TokenSource,
}

pub fn resolve_access_token() -> Option<AccessToken> {
    if let Some(value) = shared_env::env_non_empty("CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN") {
        return Some(AccessToken {
            value,
            source: TokenSource::AccessTokenEnv,
        });
    }

    if let Some(raw) = shared_env::env_non_empty("CLAUDE_PROMPT_SEGMENT_CREDENTIALS_JSON")
        && let Some(value) = access_token_from_credentials_json(&raw)
    {
        return Some(AccessToken {
            value,
            source: TokenSource::CredentialsJsonEnv,
        });
    }

    if shared_env::env_truthy("CLAUDE_PROMPT_SEGMENT_KEYCHAIN_DISABLED") {
        return None;
    }

    keychain_credentials().and_then(|raw| {
        access_token_from_credentials_json(&raw).map(|value| AccessToken {
            value,
            source: TokenSource::Keychain,
        })
    })
}

pub fn access_token_from_credentials_json(raw: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    string_at(&value, &["claudeAiOauth", "accessToken"])
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "null")
        .map(ToOwned::to_owned)
}

fn keychain_credentials() -> Option<String> {
    let service = shared_env::env_non_empty("CLAUDE_PROMPT_SEGMENT_KEYCHAIN_SERVICE")
        .unwrap_or_else(|| DEFAULT_KEYCHAIN_SERVICE.to_string());
    let output = Command::new("security")
        .arg("find-generic-password")
        .arg("-s")
        .arg(service)
        .arg("-w")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn string_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_token_from_credentials_json_reads_claude_oauth_token() {
        let raw = r#"{"claudeAiOauth":{"accessToken":" token-123 "}}"#;
        assert_eq!(
            access_token_from_credentials_json(raw),
            Some("token-123".to_string())
        );
    }

    #[test]
    fn access_token_from_credentials_json_rejects_missing_or_empty_tokens() {
        for raw in [
            "{}",
            r#"{"claudeAiOauth":{}}"#,
            r#"{"claudeAiOauth":{"accessToken":""}}"#,
            "not-json",
        ] {
            assert_eq!(access_token_from_credentials_json(raw), None, "raw={raw}");
        }
    }
}
