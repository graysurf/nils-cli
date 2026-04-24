use gemini_cli::rate_limits;
use nils_test_support::{EnvGuard, GlobalStateLock};

use std::fs as stdfs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "nils-gemini-cli-{label}-{}-{nanos}",
            std::process::id()
        ));
        let _ = stdfs::remove_dir_all(&path);
        stdfs::create_dir_all(&path).expect("temp dir");
        Self { path }
    }

    fn join(&self, child: &str) -> PathBuf {
        self.path.join(child)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = stdfs::remove_dir_all(&self.path);
    }
}

#[test]
fn diag_json_contract_schema_constants_are_stable() {
    assert_eq!(
        rate_limits::DIAG_SCHEMA_VERSION,
        "gemini-cli.diag.rate-limits.v1"
    );
    assert_eq!(rate_limits::DIAG_COMMAND, "diag rate-limits");
}

#[test]
fn diag_json_contract_single_missing_access_token_returns_2() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("diag-json-contract-missing-token");

    let secrets = dir.join("secrets");
    stdfs::create_dir_all(&secrets).expect("secrets");
    stdfs::write(
        secrets.join("alpha.json"),
        r#"{"tokens":{"account_id":"acct_001"}}"#,
    )
    .expect("write secret");
    let secrets = stdfs::canonicalize(&secrets).expect("canonical secrets");

    let secrets_env = secrets.display().to_string();
    let _secret_dir = EnvGuard::set(&lock, "GEMINI_SECRET_DIR", &secrets_env);
    let _default_all = EnvGuard::set(&lock, "GEMINI_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false");

    let options = rate_limits::RateLimitsOptions {
        json: true,
        secret: Some("alpha.json".to_string()),
        ..Default::default()
    };
    assert_eq!(rate_limits::run(&options), 2);
}

#[test]
fn diag_json_contract_all_empty_secret_dir_returns_1() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("diag-json-contract-all-empty");
    let secrets = dir.join("secrets");
    stdfs::create_dir_all(&secrets).expect("secrets");
    let secrets = stdfs::canonicalize(&secrets).expect("canonical secrets");

    let secrets_env = secrets.display().to_string();
    let _secret_dir = EnvGuard::set(&lock, "GEMINI_SECRET_DIR", &secrets_env);

    let options = rate_limits::RateLimitsOptions {
        all: true,
        json: true,
        ..Default::default()
    };
    assert_eq!(rate_limits::run(&options), 1);
}

#[test]
fn diag_json_contract_rejects_one_line_with_json() {
    let _lock = GlobalStateLock::new();
    let options = rate_limits::RateLimitsOptions {
        json: true,
        one_line: true,
        ..Default::default()
    };
    assert_eq!(rate_limits::run(&options), 64);
}
