use pretty_assertions::assert_eq;
use serde_json::json;

use crate::support;
use support::{fixture_json, parse_json_stdout, run_memo_cli, test_db_path};

#[test]
fn agent_roundtrip_empty_dataset_fetch_is_stable() {
    let db_path = test_db_path("agent_roundtrip_empty_dataset_fetch_is_stable");

    let fetch_output = run_memo_cli(&db_path, &["--json", "fetch", "--limit", "20"], None);
    assert_eq!(
        fetch_output.code,
        0,
        "fetch failed for empty dataset: {}",
        fetch_output.stderr_text()
    );
    let fetch_json = parse_json_stdout(&fetch_output);
    let rows = fetch_json["results"]
        .as_array()
        .expect("fetch results should be an array");
    assert_eq!(rows.len(), 0, "empty dataset should return no fetch rows");
    assert_eq!(fetch_json["pagination"]["returned"], 0);
    assert_eq!(fetch_json["pagination"]["has_more"], false);
}

#[test]
fn agent_roundtrip_malformed_apply_payload_returns_contract_error() {
    let db_path = test_db_path("agent_roundtrip_malformed_apply_payload_returns_contract_error");
    let fixture = fixture_json("memo_seed.json");
    let malformed_payload = fixture["malformed_apply_payload"].clone();

    let apply_output = run_memo_cli(
        &db_path,
        &["--json", "apply", "--stdin"],
        Some(&malformed_payload.to_string()),
    );
    assert_eq!(
        apply_output.code,
        65,
        "malformed payload should return data error: {}",
        apply_output.stderr_text()
    );

    let apply_json = parse_json_stdout(&apply_output);
    assert_eq!(apply_json["ok"], false);
    assert_eq!(apply_json["error"]["code"], "invalid-apply-payload");
}

#[test]
fn agent_roundtrip_duplicate_apply_is_idempotent() {
    let db_path = test_db_path("agent_roundtrip_duplicate_apply_is_idempotent");

    let add_output = run_memo_cli(
        &db_path,
        &["--json", "add", "renew passport in april"],
        None,
    );
    assert_eq!(
        add_output.code,
        0,
        "add command failed: {}",
        add_output.stderr_text()
    );

    let fetch_output = run_memo_cli(&db_path, &["--json", "fetch", "--limit", "20"], None);
    assert_eq!(
        fetch_output.code,
        0,
        "fetch command failed: {}",
        fetch_output.stderr_text()
    );
    let fetch_json = parse_json_stdout(&fetch_output);
    let item_id = fetch_json["results"][0]["item_id"]
        .as_str()
        .expect("fetched item should include item_id");

    let payload = json!({
        "agent_run_id": "agent-roundtrip",
        "items": [{
            "item_id": item_id,
            "derivation_hash": "agent-roundtrip-hash-1",
            "summary": "renew passport at district office",
            "category": "admin",
            "normalized_text": "renew passport at district office in april",
            "confidence": 0.86,
            "tags": ["admin"],
            "payload": {
                "source": "roundtrip-test"
            }
        }]
    });

    let apply_first = run_memo_cli(
        &db_path,
        &["--json", "apply", "--stdin"],
        Some(&payload.to_string()),
    );
    assert_eq!(
        apply_first.code,
        0,
        "first apply failed: {}",
        apply_first.stderr_text()
    );
    let apply_first_json = parse_json_stdout(&apply_first);
    assert_eq!(apply_first_json["result"]["accepted"], 1);
    assert_eq!(apply_first_json["result"]["skipped"], 0);

    let apply_second = run_memo_cli(
        &db_path,
        &["--json", "apply", "--stdin"],
        Some(&payload.to_string()),
    );
    assert_eq!(
        apply_second.code,
        0,
        "second apply failed: {}",
        apply_second.stderr_text()
    );
    let apply_second_json = parse_json_stdout(&apply_second);
    assert_eq!(apply_second_json["result"]["accepted"], 0);
    assert_eq!(apply_second_json["result"]["skipped"], 1);
    assert_eq!(apply_second_json["result"]["failed"], 0);
}
