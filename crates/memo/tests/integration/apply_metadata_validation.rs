use pretty_assertions::assert_eq;
use serde_json::json;

use crate::support;
use support::{parse_json_stdout, run_memo, test_db};

#[test]
fn apply_metadata_validation() {
    let (_db_dir, db_path) = test_db("apply_metadata_validation");

    let add_output = run_memo(
        &db_path,
        &["--json", "add", "metadata validation seed"],
        None,
    );
    assert_eq!(
        add_output.code,
        0,
        "add failed: {}",
        add_output.stderr_text()
    );

    let fetch_output = run_memo(&db_path, &["--json", "fetch", "--limit", "20"], None);
    assert_eq!(
        fetch_output.code,
        0,
        "fetch failed: {}",
        fetch_output.stderr_text()
    );
    let fetch_json = parse_json_stdout(&fetch_output);
    let item_id = fetch_json["data"]["items"][0]["item_id"]
        .as_str()
        .expect("item_id should be a string");

    let invalid_payload = json!({
        "items": [{
            "item_id": item_id,
            "derivation_hash": "metadata-validation-invalid",
            "summary": "invalid metadata",
            "content_type": "pdf",
            "payload": {"source": "test"}
        }]
    });
    let invalid_apply = run_memo(
        &db_path,
        &["--json", "apply", "--stdin"],
        Some(&invalid_payload.to_string()),
    );
    assert_eq!(invalid_apply.code, 65);
    let invalid_json = parse_json_stdout(&invalid_apply);
    assert_eq!(invalid_json["ok"], false);
    assert_eq!(invalid_json["error"]["code"], "invalid-apply-payload");
    assert_eq!(
        invalid_json["error"]["details"]["path"],
        "payload.items[0].content_type"
    );

    let valid_payload = json!({
        "items": [{
            "item_id": item_id,
            "derivation_hash": "metadata-validation-valid",
            "summary": "valid metadata",
            "content_type": "json",
            "validation_status": "invalid",
            "validation_errors": [{
                "code": "json.syntax.trailing-comma",
                "message": "trailing comma is not allowed",
                "path": "$"
            }],
            "payload": {"source": "test"}
        }]
    });
    let valid_apply = run_memo(
        &db_path,
        &["--json", "apply", "--stdin"],
        Some(&valid_payload.to_string()),
    );
    assert_eq!(
        valid_apply.code,
        0,
        "apply failed: {}",
        valid_apply.stderr_text()
    );
    let valid_json = parse_json_stdout(&valid_apply);
    assert_eq!(valid_json["ok"], true);
    assert_eq!(valid_json["data"]["items"][0]["content_type"], "json");
    assert_eq!(
        valid_json["data"]["items"][0]["validation_status"],
        "invalid"
    );
    assert_eq!(
        valid_json["data"]["items"][0]["validation_errors"][0]["code"],
        "json.syntax.trailing-comma"
    );
}
