use memo_cli::output::parse_item_id;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::support;
use support::{parse_json_stdout, run_memo_cli, test_db_path};

#[test]
fn fetch_apply_flow() {
    let db_path = test_db_path("fetch_apply_flow");

    let add_first = run_memo_cli(
        &db_path,
        &["--json", "add", "book pediatric dentist appointment"],
        None,
    );
    assert_eq!(
        add_first.code,
        0,
        "add first failed: {}",
        add_first.stderr_text()
    );

    let add_second = run_memo_cli(&db_path, &["--json", "add", "buy 1tb ssd for mom"], None);
    assert_eq!(
        add_second.code,
        0,
        "add second failed: {}",
        add_second.stderr_text()
    );

    let fetch_before = run_memo_cli(&db_path, &["--json", "fetch", "--limit", "20"], None);
    assert_eq!(
        fetch_before.code,
        0,
        "fetch before apply failed: {}",
        fetch_before.stderr_text()
    );
    let fetch_before_json = parse_json_stdout(&fetch_before);
    let fetch_before_rows = fetch_before_json["results"]
        .as_array()
        .expect("results array should exist");
    assert_eq!(fetch_before_rows.len(), 2);

    let apply_item_id = fetch_before_rows[0]["item_id"]
        .as_str()
        .expect("item_id should be a string");

    let apply_payload = json!({
        "agent_run_id": "agent-run-fetch-flow",
        "items": [{
            "item_id": apply_item_id,
            "derivation_hash": "hash-fetch-flow-1",
            "summary": "buy ssd for mom",
            "category": "shopping",
            "normalized_text": "buy 1tb ssd for mom",
            "confidence": 0.93,
            "tags": ["family", "shopping"],
            "payload": {
                "task": "buy ssd for mom"
            }
        }]
    });
    let apply_output = run_memo_cli(
        &db_path,
        &["--json", "apply", "--stdin"],
        Some(&apply_payload.to_string()),
    );
    assert_eq!(
        apply_output.code,
        0,
        "apply failed: {}",
        apply_output.stderr_text()
    );
    let apply_json = parse_json_stdout(&apply_output);
    assert_eq!(apply_json["result"]["accepted"], 1);
    assert_eq!(apply_json["result"]["failed"], 0);

    let fetch_after = run_memo_cli(&db_path, &["--json", "fetch", "--limit", "20"], None);
    assert_eq!(
        fetch_after.code,
        0,
        "fetch after apply failed: {}",
        fetch_after.stderr_text()
    );
    let fetch_after_json = parse_json_stdout(&fetch_after);
    let fetch_after_rows = fetch_after_json["results"]
        .as_array()
        .expect("results array should exist");
    assert_eq!(fetch_after_rows.len(), 1);
    assert_ne!(fetch_after_rows[0]["item_id"], apply_item_id);
}

#[test]
fn apply_idempotency() {
    let db_path = test_db_path("apply_idempotency");

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
    let item_id_str = add_json["result"]["item_id"]
        .as_str()
        .expect("item_id should be string");
    let item_id = parse_item_id(item_id_str).expect("item_id should parse");

    let first_payload = json!({
        "items": [{
            "item_id": item_id_str,
            "derivation_hash": "hash-idempotency-1",
            "summary": "renew passport",
            "category": "admin",
            "normalized_text": "renew passport in april",
            "confidence": 0.81,
            "payload": {"summary":"renew passport"}
        }]
    });
    let apply_first = run_memo_cli(
        &db_path,
        &["--json", "apply", "--stdin"],
        Some(&first_payload.to_string()),
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
        Some(&first_payload.to_string()),
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

    let second_payload = json!({
        "items": [{
            "item_id": item_id_str,
            "derivation_hash": "hash-idempotency-2",
            "summary": "renew passport at district office",
            "category": "admin",
            "normalized_text": "renew passport at district office in april",
            "confidence": 0.84,
            "payload": {"summary":"renew passport at district office"}
        }]
    });
    let apply_third = run_memo_cli(
        &db_path,
        &["--json", "apply", "--stdin"],
        Some(&second_payload.to_string()),
    );
    assert_eq!(
        apply_third.code,
        0,
        "third apply failed: {}",
        apply_third.stderr_text()
    );
    let apply_third_json = parse_json_stdout(&apply_third);
    assert_eq!(apply_third_json["result"]["accepted"], 1);

    let conn = rusqlite::Connection::open(db_path).expect("open db for assertions");
    let derivation_count: i64 = conn
        .query_row(
            "select count(*) from item_derivations where item_id = ?1",
            rusqlite::params![item_id],
            |row| row.get(0),
        )
        .expect("derivation count query");
    assert_eq!(derivation_count, 2);

    let active_version: i64 = conn
        .query_row(
            "select derivation_version
             from item_derivations
             where item_id = ?1 and is_active = 1 and status = 'accepted'
             limit 1",
            rusqlite::params![item_id],
            |row| row.get(0),
        )
        .expect("active derivation version query");
    assert_eq!(active_version, 2);
}
