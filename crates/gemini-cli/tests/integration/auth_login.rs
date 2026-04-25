use gemini_cli::auth;
use nils_test_support::{EnvGuard, GlobalStateLock, StubBinDir, prepend_path};
use std::fs;

fn set_env(lock: &GlobalStateLock, key: &str, value: impl AsRef<std::ffi::OsStr>) -> EnvGuard {
    let value = value.as_ref().to_string_lossy().into_owned();
    EnvGuard::set(lock, key, &value)
}

fn curl_stub_script() -> &'static str {
    r#"#!/bin/sh
set -eu
case "$*" in
  *openidconnect.googleapis.com/v1/userinfo*)
    cat <<'EOF'
{"email":"alpha@example.com"}
__HTTP_STATUS__:200
EOF
    exit 0
    ;;
esac
cat <<'EOF'
{"error":"unexpected"}
__HTTP_STATUS__:400
EOF
exit 0
"#
}

fn gemini_stub_script() -> &'static str {
    r#"#!/bin/sh
set -eu
cat > "$GEMINI_AUTH_FILE" <<'EOF'
{"access_token":"tok","id_token":"header.payload.sig"}
EOF
exit 0
"#
}

#[test]
fn auth_login_rejects_conflicting_flags() {
    let _lock = GlobalStateLock::new();
    let code = auth::login::run_with_json(true, true, false);
    assert_eq!(code, 64);
}

#[test]
fn auth_login_api_key_requires_env() {
    let lock = GlobalStateLock::new();
    let _api = EnvGuard::set(&lock, "GEMINI_API_KEY", "");
    let _google = EnvGuard::set(&lock, "GOOGLE_API_KEY", "");
    let code = auth::login::run_with_json(true, false, false);
    assert_eq!(code, 64);
}

#[test]
fn auth_login_api_key_succeeds_with_env() {
    let lock = GlobalStateLock::new();
    let _api = EnvGuard::set(&lock, "GEMINI_API_KEY", "dummy-key");
    let code = auth::login::run_with_json(true, false, false);
    assert_eq!(code, 0);
}

#[test]
fn auth_login_browser_and_device_code_use_userinfo_flow() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    stubs.write_exe("curl", curl_stub_script());
    stubs.write_exe("gemini", gemini_stub_script());

    let auth_file = dir.path().join("oauth_creds.json");
    fs::write(
        &auth_file,
        r#"{"access_token":"tok","id_token":"header.payload.sig"}"#,
    )
    .expect("write auth");

    let _path = prepend_path(&lock, stubs.path());
    let _auth = set_env(&lock, "GEMINI_AUTH_FILE", auth_file.as_os_str());

    assert_eq!(auth::login::run_with_json(false, false, false), 0);
    assert_eq!(auth::login::run_with_json(false, true, false), 0);
}
