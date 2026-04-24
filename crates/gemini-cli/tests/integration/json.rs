use gemini_cli::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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

#[test]
fn json_i64_at_parses_numeric_string() {
    let dir = TestDir::new("json-numeric-string");
    let path = dir.path().join("limits.json");
    fs::write(
        &path,
        r#"{
            "limits": {
                "weekly_reset_at_epoch": "1737331200"
            }
        }"#,
    )
    .expect("write json");

    let value = json::read_json(&path).expect("read json");
    let parsed = json::i64_at(&value, &["limits", "weekly_reset_at_epoch"]);
    assert_eq!(parsed, Some(1_737_331_200));
}

#[test]
fn json_i64_at_rejects_non_numeric_types() {
    let dir = TestDir::new("json-reject-bool");
    let path = dir.path().join("limits.json");
    fs::write(
        &path,
        r#"{
            "limits": {
                "weekly_reset_at_epoch": true
            }
        }"#,
    )
    .expect("write json");

    let value = json::read_json(&path).expect("read json");
    let parsed = json::i64_at(&value, &["limits", "weekly_reset_at_epoch"]);
    assert_eq!(parsed, None);
}
