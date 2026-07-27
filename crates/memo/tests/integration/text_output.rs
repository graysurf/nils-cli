use pretty_assertions::assert_eq;
use serde_json::json;

use crate::support;
use support::{run_memo, run_memo_with_env, test_db};

#[test]
fn text_output_respects_no_color() {
    let (_db_dir, db_path) = test_db("text_output_no_color");
    let add_one = run_memo(&db_path, &["add", "buy 1tb ssd for mom"], None);
    assert_eq!(add_one.code, 0, "add one failed: {}", add_one.stderr_text());

    let add_two = run_memo(
        &db_path,
        &["add", "book pediatric dentist appointment"],
        None,
    );
    assert_eq!(add_two.code, 0, "add two failed: {}", add_two.stderr_text());

    let list_output = run_memo_with_env(
        &db_path,
        &["list", "--limit", "5"],
        None,
        &[("NO_COLOR", "1")],
    );
    assert_eq!(
        list_output.code,
        0,
        "list failed: {}",
        list_output.stderr_text()
    );

    let stdout = list_output.stdout_text();
    assert!(stdout.contains("item_id"));
    assert!(stdout.contains("created_at"));
    assert!(stdout.contains("state"));
    assert!(
        !stdout.contains("\u{1b}["),
        "NO_COLOR output must not contain ANSI escapes"
    );
}

#[test]
fn text_output_routes_warnings_to_stderr() {
    let (_db_dir, db_path) = test_db("text_output_warning_stderr");
    let add_output = run_memo(&db_path, &["add", "renew passport in april"], None);
    assert_eq!(
        add_output.code,
        0,
        "add failed: {}",
        add_output.stderr_text()
    );

    let apply_payload = json!({
        "items": [{
            "item_id": "itm_99999999",
            "summary": "invalid target item"
        }]
    });
    let apply_output = run_memo(
        &db_path,
        &["apply", "--stdin"],
        Some(&apply_payload.to_string()),
    );
    assert_eq!(
        apply_output.code,
        0,
        "apply run failed: {}",
        apply_output.stderr_text()
    );

    let stdout = apply_output.stdout_text();
    let stderr = apply_output.stderr_text();
    assert!(stdout.contains("apply payload processed=1"));
    assert!(!stdout.contains("warning:"));
    assert!(stderr.contains("warning:"));
}
