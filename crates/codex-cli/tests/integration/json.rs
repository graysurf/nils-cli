use codex_cli::json;
use serde_json::json;

#[test]
fn json_i64_at_parses_numeric_string() {
    let value = json!({
        "limits": {
            "weekly_reset_at_epoch": "1737331200"
        }
    });

    let parsed = json::i64_at(&value, &["limits", "weekly_reset_at_epoch"]);
    assert_eq!(parsed, Some(1_737_331_200));
}

#[test]
fn json_i64_at_rejects_non_numeric_types() {
    let value = json!({
        "limits": {
            "weekly_reset_at_epoch": true
        }
    });

    let parsed = json::i64_at(&value, &["limits", "weekly_reset_at_epoch"]);
    assert_eq!(parsed, None);
}
