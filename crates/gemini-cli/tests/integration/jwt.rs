use gemini_cli::auth;
use gemini_cli::jwt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const HEADER: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
const PAYLOAD_ALPHA: &str = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlcl8xMjMiLCJlbWFpbCI6ImFscGhhQGV4YW1wbGUuY29tIn19";

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "nils-gemini-cli-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp test dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

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
    let dir = TestDir::new("jwt-auth-file");
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
