use pretty_assertions::assert_eq;
use serde_json::json;

use crate::support;
use support::{parse_json_stdout, run_memo_cli, test_db_path};

#[test]
fn json_contract() {
    let db_path = test_db_path("json_contract");

    let add_output = run_memo_cli(&db_path, &["--json", "add", "buy 1tb ssd for mom"], None);
    assert_eq!(
        add_output.code,
        0,
        "add failed: {}",
        add_output.stderr_text()
    );
    let add_json = parse_json_stdout(&add_output);
    assert_eq!(add_json["schema_version"], "memo-cli.add.v1");
    assert_eq!(add_json["command"], "memo-cli add");
    assert_eq!(add_json["ok"], true);
    let item_id = add_json["result"]["item_id"]
        .as_str()
        .expect("item_id should be a string");
    assert!(add_json.get("result").is_some(), "result key should exist");
    assert!(
        add_json.get("results").is_none(),
        "results key should not exist"
    );

    let update_output = run_memo_cli(
        &db_path,
        &["--json", "update", item_id, "buy 2tb ssd for mom"],
        None,
    );
    assert_eq!(
        update_output.code,
        0,
        "update failed: {}",
        update_output.stderr_text()
    );
    let update_json = parse_json_stdout(&update_output);
    assert_eq!(update_json["schema_version"], "memo-cli.update.v1");
    assert_eq!(update_json["command"], "memo-cli update");
    assert_eq!(update_json["ok"], true);
    assert_eq!(update_json["result"]["state"], "pending");

    let list_output = run_memo_cli(&db_path, &["--json", "list", "--limit", "20"], None);
    assert_eq!(
        list_output.code,
        0,
        "list failed: {}",
        list_output.stderr_text()
    );
    let list_json = parse_json_stdout(&list_output);
    assert_eq!(list_json["schema_version"], "memo-cli.list.v1");
    assert_eq!(list_json["command"], "memo-cli list");
    assert_eq!(list_json["ok"], true);
    assert!(
        list_json.get("result").is_none(),
        "result key should not exist"
    );
    assert!(
        list_json.get("results").is_some(),
        "results key should exist"
    );
    assert!(
        list_json.get("pagination").is_some(),
        "pagination key should exist"
    );
    assert_eq!(list_json["pagination"]["limit"], 20);
    assert_eq!(list_json["pagination"]["offset"], 0);
    assert_eq!(list_json["pagination"]["returned"], 1);
    let first_list_item = &list_json["results"][0];
    assert!(
        first_list_item.get("content_type").is_some(),
        "list item should include content_type key"
    );
    assert!(
        first_list_item.get("validation_status").is_some(),
        "list item should include validation_status key"
    );

    let search_output = run_memo_cli(&db_path, &["--json", "search", "ssd", "--limit", "5"], None);
    assert_eq!(
        search_output.code,
        0,
        "search failed: {}",
        search_output.stderr_text()
    );
    let search_json = parse_json_stdout(&search_output);
    assert_eq!(search_json["schema_version"], "memo-cli.search.v1");
    assert_eq!(search_json["command"], "memo-cli search");
    assert_eq!(search_json["ok"], true);
    assert!(
        search_json.get("results").is_some(),
        "results key should exist"
    );
    assert!(search_json.get("meta").is_some(), "meta key should exist");
    assert_eq!(search_json["meta"]["query"], "ssd");
    assert_eq!(search_json["meta"]["limit"], 5);
    assert_eq!(search_json["meta"]["state"], "all");
    assert_eq!(search_json["meta"]["match"], "fts");
    assert_eq!(
        search_json["meta"]["fields"],
        json!(["raw_text", "derived_text", "tags_text"])
    );

    let fetch_output = run_memo_cli(&db_path, &["--json", "fetch", "--limit", "1"], None);
    assert_eq!(
        fetch_output.code,
        0,
        "fetch failed: {}",
        fetch_output.stderr_text()
    );
    let fetch_json = parse_json_stdout(&fetch_output);
    assert_eq!(fetch_json["schema_version"], "memo-cli.fetch.v1");
    assert!(
        fetch_json.get("results").is_some(),
        "results key should exist"
    );
    assert!(
        fetch_json.get("pagination").is_some(),
        "pagination key should exist"
    );
    let first_fetch_item = &fetch_json["results"][0];
    assert!(
        first_fetch_item.get("content_type").is_some(),
        "fetch item should include content_type key"
    );
    assert!(
        first_fetch_item.get("validation_status").is_some(),
        "fetch item should include validation_status key"
    );

    let invalid_apply = run_memo_cli(&db_path, &["--json", "apply", "--stdin"], Some("{}"));
    assert_eq!(invalid_apply.code, 65, "apply should fail with data error");
    let invalid_apply_json = parse_json_stdout(&invalid_apply);
    assert_eq!(invalid_apply_json["schema_version"], "memo-cli.apply.v1");
    assert_eq!(invalid_apply_json["command"], "memo-cli apply");
    assert_eq!(invalid_apply_json["ok"], false);
    assert!(invalid_apply_json.get("result").is_none());
    assert!(invalid_apply_json.get("results").is_none());
    assert_eq!(
        invalid_apply_json["error"]["code"],
        serde_json::Value::String("invalid-apply-payload".to_string())
    );

    let delete_without_hard = run_memo_cli(&db_path, &["--json", "delete", item_id], None);
    assert_eq!(
        delete_without_hard.code, 64,
        "delete without --hard should fail with usage error"
    );
    let delete_without_hard_json = parse_json_stdout(&delete_without_hard);
    assert_eq!(delete_without_hard_json["ok"], false);

    let delete_output = run_memo_cli(&db_path, &["--json", "delete", item_id, "--hard"], None);
    assert_eq!(
        delete_output.code,
        0,
        "delete failed: {}",
        delete_output.stderr_text()
    );
    let delete_json = parse_json_stdout(&delete_output);
    assert_eq!(delete_json["schema_version"], "memo-cli.delete.v1");
    assert_eq!(delete_json["command"], "memo-cli delete");
    assert_eq!(delete_json["ok"], true);
    assert_eq!(delete_json["result"]["deleted"], true);
}

#[test]
fn json_no_secret_leak() {
    let db_path = test_db_path("json_no_secret_leak");
    let secret = "SECRET_TOKEN_SHOULD_NOT_LEAK";

    let add_output = run_memo_cli(
        &db_path,
        &["--json", "add", "renew passport in april"],
        None,
    );
    assert_eq!(
        add_output.code,
        0,
        "add failed: {}",
        add_output.stderr_text()
    );
    let add_json = parse_json_stdout(&add_output);
    let item_id = add_json["result"]["item_id"]
        .as_str()
        .expect("item_id should be a string");

    let success_payload = json!({
        "items": [{
            "item_id": item_id,
            "derivation_hash": "hash-secret-check",
            "summary": "renew passport",
            "category": "admin",
            "normalized_text": "renew passport in april",
            "confidence": 0.77,
            "payload": {
                "access_token": secret,
                "note": "should never be echoed in outputs"
            }
        }]
    });
    let apply_success = run_memo_cli(
        &db_path,
        &["--json", "apply", "--stdin"],
        Some(&success_payload.to_string()),
    );
    assert_eq!(
        apply_success.code,
        0,
        "successful apply failed: {}",
        apply_success.stderr_text()
    );
    let apply_success_stdout = apply_success.stdout_text();
    let apply_success_stderr = apply_success.stderr_text();
    assert!(
        !apply_success_stdout.contains(secret),
        "stdout leaked a secret token"
    );
    assert!(
        !apply_success_stderr.contains(secret),
        "stderr leaked a secret token"
    );

    let invalid_payload = json!({
        "items": [{
            "access_token": secret
        }]
    });
    let apply_failure = run_memo_cli(
        &db_path,
        &["--json", "apply", "--stdin"],
        Some(&invalid_payload.to_string()),
    );
    assert_eq!(
        apply_failure.code, 65,
        "invalid apply should fail with data error"
    );
    let apply_failure_stdout = apply_failure.stdout_text();
    let apply_failure_stderr = apply_failure.stderr_text();
    assert!(
        !apply_failure_stdout.contains(secret),
        "stdout leaked a secret token"
    );
    assert!(
        !apply_failure_stderr.contains(secret),
        "stderr leaked a secret token"
    );
}
