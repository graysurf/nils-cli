use codex_cli::auth;
use codex_cli::jwt;
use pretty_assertions::assert_eq;
use std::fs;

const HEADER: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
const PAYLOAD_ALPHA: &str = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlcl8xMjMiLCJlbWFpbCI6ImFscGhhQGV4YW1wbGUuY29tIn19";

fn token(payload: &str) -> String {
    format!("{HEADER}.{payload}.sig")
}

#[test]
fn runtime_auth_contract_identity_and_email_are_extracted_from_jwt_payload() {
    let token = token(PAYLOAD_ALPHA);
    let payload = jwt::decode_payload_json(&token).expect("payload json");

    assert_eq!(
        jwt::identity_from_payload(&payload).expect("identity"),
        "user_123"
    );
    assert_eq!(
        jwt::email_from_payload(&payload).expect("email"),
        "alpha@example.com"
    );
}

#[test]
fn runtime_auth_contract_identity_precedence_and_account_fallback_are_stable() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("auth.json");

    let content = format!(
        r#"{{"tokens":{{"id_token":"{}","access_token":"{}","account_id":"acct_001"}},"last_refresh":"2025-01-20T12:34:56Z"}}"#,
        token(PAYLOAD_ALPHA),
        token(PAYLOAD_ALPHA)
    );
    fs::write(&path, content).expect("write auth json");

    let identity = auth::identity_from_auth_file(&path)
        .expect("identity")
        .expect("identity value");
    let email = auth::email_from_auth_file(&path)
        .expect("email")
        .expect("email value");
    let account_id = auth::account_id_from_auth_file(&path)
        .expect("account")
        .expect("account value");
    let identity_key = auth::identity_key_from_auth_file(&path)
        .expect("identity key")
        .expect("identity key value");

    assert_eq!(identity, "user_123");
    assert_eq!(email, "alpha@example.com");
    assert_eq!(account_id, "acct_001");
    assert_eq!(identity_key, "user_123::acct_001");
}

#[test]
fn runtime_auth_contract_top_level_tokens_are_supported_for_identity_and_email() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("auth.json");
    let content = format!(
        r#"{{"id_token":"{}","access_token":"{}"}}"#,
        token(PAYLOAD_ALPHA),
        token(PAYLOAD_ALPHA)
    );
    fs::write(&path, content).expect("write auth json");

    let identity = auth::identity_from_auth_file(&path).expect("identity");
    let email = auth::email_from_auth_file(&path).expect("email");
    assert_eq!(identity, Some("user_123".to_string()));
    assert_eq!(email, Some("alpha@example.com".to_string()));
}

#[test]
fn runtime_auth_contract_account_id_falls_back_to_sub_claim() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("auth.json");
    let content = format!(r#"{{"access_token":"{}"}}"#, token(PAYLOAD_ALPHA));
    fs::write(&path, content).expect("write auth json");

    let account_id = auth::account_id_from_auth_file(&path).expect("account");
    assert_eq!(account_id, Some("user_123".to_string()));
}

#[test]
fn runtime_auth_contract_invalid_auth_file_is_deterministic_and_non_secret_leaking() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("broken.json");
    fs::write(&path, "{not-json").expect("write malformed json");

    let error = auth::identity_from_auth_file(&path).expect_err("invalid auth error");
    let error_text = error.to_string();
    assert!(error_text.contains("invalid json"));
    assert!(!error_text.contains("id_token"));
}
