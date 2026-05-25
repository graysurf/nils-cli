use std::path::Path;

use serde_json::Value;

use super::error::CoreError;
use super::json;
use super::jwt;

pub fn identity_from_auth_file(path: &Path) -> Result<Option<String>, CoreError> {
    let value = json::read_json(path)?;
    let payload = decoded_payload_from_auth_json(&value);
    Ok(identity_from_auth_json(payload.as_ref()))
}

pub fn email_from_auth_file(path: &Path) -> Result<Option<String>, CoreError> {
    let value = json::read_json(path)?;
    let payload = decoded_payload_from_auth_json(&value);
    Ok(email_from_auth_json(payload.as_ref()))
}

pub fn account_id_from_auth_file(path: &Path) -> Result<Option<String>, CoreError> {
    let value = json::read_json(path)?;
    let payload = decoded_payload_from_auth_json(&value);
    Ok(account_id_from_auth_json(&value, payload.as_ref()))
}

pub fn last_refresh_from_auth_file(path: &Path) -> Result<Option<String>, CoreError> {
    let value = json::read_json(path)?;
    Ok(json::string_at(&value, &["last_refresh"]))
}

pub fn identity_key_from_auth_file(path: &Path) -> Result<Option<String>, CoreError> {
    let value = json::read_json(path)?;
    let payload = decoded_payload_from_auth_json(&value);
    let identity = identity_from_auth_json(payload.as_ref());
    let identity = match identity {
        Some(value) => value,
        None => return Ok(None),
    };
    let account_id = account_id_from_auth_json(&value, payload.as_ref());
    let key = match account_id {
        Some(account) => format!("{}::{}", identity, account),
        None => identity,
    };
    Ok(Some(key))
}

pub fn resolve_secret_file_by_email(secret_dir: &Path, target: &str) -> SecretFileResolution {
    let query = target.to_lowercase();
    let want_full = target.contains('@');

    let mut matches = Vec::new();
    if let Ok(entries) = std::fs::read_dir(secret_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            let email = match email_from_auth_file(&path) {
                Ok(Some(value)) => value,
                _ => continue,
            };
            let email_lower = email.to_lowercase();
            if want_full {
                if email_lower == query {
                    matches.push(file_name(&path));
                }
            } else if let Some(local_part) = email_lower.split('@').next()
                && local_part == query
            {
                matches.push(file_name(&path));
            }
        }
    }
    matches.sort();

    if matches.len() == 1 {
        SecretFileResolution::Exact(matches.remove(0))
    } else if matches.is_empty() {
        SecretFileResolution::NotFound
    } else {
        SecretFileResolution::Ambiguous {
            candidates: matches,
        }
    }
}

pub fn token_from_auth_json(value: &serde_json::Value) -> Option<String> {
    json::string_at(value, &["tokens", "id_token"])
        .or_else(|| json::string_at(value, &["id_token"]))
        .or_else(|| json::string_at(value, &["tokens", "access_token"]))
        .or_else(|| json::string_at(value, &["access_token"]))
}

fn decoded_payload_from_auth_json(value: &Value) -> Option<Value> {
    let token = token_from_auth_json(value)?;
    jwt::decode_payload_json(&token)
}

fn identity_from_auth_json(payload: Option<&Value>) -> Option<String> {
    payload.and_then(jwt::identity_from_payload)
}

fn email_from_auth_json(payload: Option<&Value>) -> Option<String> {
    payload.and_then(jwt::email_from_payload)
}

fn account_id_from_auth_json(value: &Value, payload: Option<&Value>) -> Option<String> {
    json::string_at(value, &["tokens", "account_id"])
        .or_else(|| json::string_at(value, &["account_id"]))
        .or_else(|| {
            payload
                .and_then(|decoded| decoded.get("sub"))
                .and_then(|sub| sub.as_str())
                .map(json::strip_newlines)
        })
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretFileResolution {
    Exact(String),
    Ambiguous { candidates: Vec<String> },
    NotFound,
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use std::fs;
    use tempfile::TempDir;

    fn write_auth_json(path: &Path, contents: &str) {
        fs::write(path, contents).expect("write auth json");
    }

    fn jwt(payload: Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(payload.to_string());
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn account_id_falls_back_to_token_sub_when_fields_missing() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("auth.json");
        let token = jwt(serde_json::json!({"sub":"acct_123"}));

        write_auth_json(&path, &format!(r#"{{"tokens":{{"id_token":"{token}"}}}}"#));

        assert_eq!(
            account_id_from_auth_file(&path).expect("account id"),
            Some("acct_123".to_string())
        );
    }

    #[test]
    fn identity_key_prefers_account_id_from_json_over_jwt_subject() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("auth.json");
        let token = jwt(serde_json::json!({
            "sub": "sub_from_jwt",
            "https://api.openai.com/auth": {"chatgpt_user_id":"user_456"}
        }));

        write_auth_json(
            &path,
            &format!(r#"{{"tokens":{{"id_token":"{token}","account_id":"acct_999"}}}}"#),
        );

        assert_eq!(
            identity_key_from_auth_file(&path).expect("identity key"),
            Some("user_456::acct_999".to_string())
        );
    }

    #[test]
    fn identity_key_is_none_when_identity_is_missing() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("auth.json");
        write_auth_json(&path, r#"{"tokens":{"account_id":"acct_100"}}"#);

        assert_eq!(
            identity_key_from_auth_file(&path).expect("identity key"),
            None
        );
    }

    #[test]
    fn resolve_secret_file_by_email_supports_full_and_local_part_lookup() {
        let dir = TempDir::new().expect("tempdir");
        write_auth_json(
            &dir.path().join("alpha.json"),
            &auth_json_for_email("alpha@example.com"),
        );
        write_auth_json(
            &dir.path().join("beta.json"),
            &auth_json_for_email("beta@example.com"),
        );
        write_auth_json(&dir.path().join("notes.txt"), "not json");

        assert_eq!(
            resolve_secret_file_by_email(dir.path(), "alpha@example.com"),
            SecretFileResolution::Exact("alpha.json".to_string())
        );
        assert_eq!(
            resolve_secret_file_by_email(dir.path(), "beta"),
            SecretFileResolution::Exact("beta.json".to_string())
        );
    }

    #[test]
    fn resolve_secret_file_by_email_reports_ambiguous_and_not_found() {
        let dir = TempDir::new().expect("tempdir");
        write_auth_json(
            &dir.path().join("alpha-1.json"),
            &auth_json_for_email("alpha@example.com"),
        );
        write_auth_json(
            &dir.path().join("alpha-2.json"),
            &auth_json_for_email("alpha@example.com"),
        );

        match resolve_secret_file_by_email(dir.path(), "alpha@example.com") {
            SecretFileResolution::Ambiguous { candidates } => {
                assert_eq!(
                    candidates,
                    vec!["alpha-1.json".to_string(), "alpha-2.json".to_string()]
                );
            }
            other => panic!("expected ambiguous match, got {other:?}"),
        }

        assert_eq!(
            resolve_secret_file_by_email(dir.path(), "missing@example.com"),
            SecretFileResolution::NotFound
        );
    }

    fn auth_json_for_email(email: &str) -> String {
        let token = jwt(serde_json::json!({ "email": email }));
        format!(r#"{{"tokens":{{"id_token":"{token}"}}}}"#)
    }
}
