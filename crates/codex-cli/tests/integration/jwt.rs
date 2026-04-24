use std::fs;

use codex_cli::auth;
use codex_cli::jwt;
use pretty_assertions::assert_eq;

const HEADER: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
const PAYLOAD_ALPHA: &str = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlcl8xMjMiLCJlbWFpbCI6ImFscGhhQGV4YW1wbGUuY29tIn19";

fn token(payload: &str) -> String {
    format!("{HEADER}.{payload}.sig")
}

#[test]
fn jwt_decode_payload() {
    let token = token(PAYLOAD_ALPHA);
    let payload = jwt::decode_payload_json(&token).expect("payload json");
    let identity = jwt::identity_from_payload(&payload).expect("identity");
    let email = jwt::email_from_payload(&payload).expect("email");
    assert_eq!(identity, "user_123");
    assert_eq!(email, "alpha@example.com");
}

#[test]
fn jwt_identity_key_from_auth_file() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("auth.json");
    let content = format!(
        r#"{{"tokens":{{"access_token":"{}","id_token":"{}","account_id":"acct_001"}},"last_refresh":"2025-01-20T12:34:56Z"}}"#,
        token(PAYLOAD_ALPHA),
        token(PAYLOAD_ALPHA)
    );
    fs::write(&path, content).expect("write auth json");

    let key = auth::identity_key_from_auth_file(&path)
        .expect("identity key result")
        .expect("identity key");
    assert_eq!(key, "user_123::acct_001");
}

#[test]
fn jwt_decode_payload_rejects_empty_payload_segment() {
    let token = format!("{HEADER}..sig");
    assert_eq!(jwt::decode_payload(&token), None);
}
