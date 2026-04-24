use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use pretty_assertions::assert_eq;
use std::fs;
use std::path::{Path, PathBuf};

const HEADER: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
const PAYLOAD_ALPHA: &str = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlcl8xMjMiLCJlbWFpbCI6ImFscGhhQGV4YW1wbGUuY29tIn19";

fn token(payload: &str) -> String {
    format!("{HEADER}.{payload}.sig")
}

fn auth_json(payload: &str, account_id: &str, refresh_token: &str, last_refresh: &str) -> String {
    format!(
        r#"{{"tokens":{{"access_token":"{}","id_token":"{}","refresh_token":"{}","account_id":"{}"}},"last_refresh":"{}"}}"#,
        token(payload),
        token(payload),
        refresh_token,
        account_id,
        last_refresh
    )
}

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn run(args: &[&str], envs: &[(&str, &Path)]) -> CmdOutput {
    let mut options = CmdOptions::default();
    for (key, path) in envs {
        let value = path.to_string_lossy();
        options = options.with_env(key, value.as_ref());
    }
    let bin = codex_cli_bin();
    cmd::run_with(&bin, args, &options)
}

fn stdout(output: &CmdOutput) -> String {
    output.stdout_text()
}

fn stderr(output: &CmdOutput) -> String {
    output.stderr_text()
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(output.code, code);
}

#[test]
fn auth_use_missing_arg() {
    let output = run(&["auth", "use"], &[]);
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("codex-use: usage: codex-use <name|name.json|email>"));
}

#[test]
fn auth_use_extra_args() {
    let output = run(&["auth", "use", "one", "two"], &[]);
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("codex-use: usage: codex-use <name|name.json|email>"));
}

#[test]
fn auth_use_invalid_path() {
    let output = run(&["auth", "use", "../secret"], &[]);
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("codex-use: invalid secret name"));
}

#[test]
fn auth_use_rejects_backslash_path() {
    let output = run(&["auth", "use", r"a\\secret"], &[]);
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("codex-use: invalid secret name"));
}

#[test]
fn auth_use_name_without_json_suffix_resolves_json_secret() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    let cache = dir.path().join("cache");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::create_dir_all(&cache).expect("cache dir");

    let auth_file = dir.path().join("auth.json");
    let secret_file = secrets.join("alpha.json");
    let content = auth_json(
        PAYLOAD_ALPHA,
        "acct_001",
        "refresh_a",
        "2025-01-20T12:34:56Z",
    );
    fs::write(&secret_file, &content).expect("write secret");

    let output = run(
        &["auth", "use", "alpha"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("CODEX_SECRET_CACHE_DIR", &cache),
        ],
    );

    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(out.contains("applied stored secret"));
    assert!(!out.contains("alpha.json"));
    let applied = fs::read_to_string(&auth_file).expect("read auth file");
    assert_eq!(applied, content);
}

#[test]
fn auth_use_email_resolution() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    let cache = dir.path().join("cache");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::create_dir_all(&cache).expect("cache dir");

    let auth_file = dir.path().join("auth.json");
    let secret_file = secrets.join("alpha.json");
    let content = auth_json(
        PAYLOAD_ALPHA,
        "acct_001",
        "refresh_a",
        "2025-01-20T12:34:56Z",
    );
    fs::write(&secret_file, &content).expect("write secret");

    let output = run(
        &["auth", "use", "alpha@example.com"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("CODEX_SECRET_CACHE_DIR", &cache),
        ],
    );

    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(out.contains("applied stored secret"));
    assert!(!out.contains("alpha.json"));

    let applied = fs::read_to_string(&auth_file).expect("read auth file");
    assert_eq!(applied, content);

    let timestamp = cache.join("auth.json.timestamp");
    assert_eq!(
        fs::read_to_string(&timestamp).unwrap(),
        "2025-01-20T12:34:56Z"
    );
}

#[test]
fn auth_use_ambiguous_email() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");

    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, "{}").expect("write auth");

    let content = auth_json(
        PAYLOAD_ALPHA,
        "acct_001",
        "refresh_a",
        "2025-01-20T12:34:56Z",
    );
    fs::write(secrets.join("alpha.json"), &content).expect("write secret");
    fs::write(secrets.join("alpha-duplicate.json"), &content).expect("write secret");

    let output = run(
        &["auth", "use", "alpha@example.com"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
        ],
    );

    assert_exit(&output, 2);
    let err = stderr(&output);
    assert!(err.contains("identifier matches multiple secrets"));
    assert!(err.contains("alpha.json"));
    assert!(err.contains("alpha-duplicate.json"));
}
