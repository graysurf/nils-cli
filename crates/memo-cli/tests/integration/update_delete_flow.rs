use memo_cli::output::parse_item_id;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::support;
use support::{parse_json_stdout, run_memo_cli, test_db_path};

#[test]
fn update_and_delete_keep_layers_consistent() {
    let db_path = test_db_path("update_and_delete_keep_layers_consistent");

    let add_output = run_memo_cli(&db_path, &["--json", "add", "buy 1tb ssd for mom"], None);
    assert_eq!(
        add_output.code,
        0,
        "add failed: {}",
        add_output.stderr_text()
    );
    let add_json = parse_json_stdout(&add_output);
    let item_id_str = add_json["data"]["item_id"]
        .as_str()
        .expect("item_id should be a string");
    let item_id = parse_item_id(item_id_str).expect("item_id should parse");

    let apply_payload = json!({
        "agent_run_id": "agent-run-update-delete-flow",
        "items": [{
            "item_id": item_id_str,
            "derivation_hash": "hash-update-delete-flow-1",
            "summary": "buy ssd for mom",
            "category": "shopping",
            "normalized_text": "buy 1tb ssd for mom",
            "confidence": 0.93,
            "tags": ["family", "shopping"],
            "payload": {"source":"test"}
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

    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    conn.execute(
        "insert into workflow_item_anchors(item_id, workflow_type) values (?1, 'game')",
        rusqlite::params![item_id],
    )
    .expect("insert workflow anchor");
    let anchor_id: i64 = conn
        .query_row(
            "select anchor_id from workflow_item_anchors where item_id = ?1 and workflow_type = 'game'",
            rusqlite::params![item_id],
            |row| row.get(0),
        )
        .expect("lookup anchor id");
    conn.execute(
        "insert into workflow_game_entries(anchor_id, game_name, source_url, description)
         values(?1, 'elden ring', 'https://example.com', 'wishlist')",
        rusqlite::params![anchor_id],
    )
    .expect("insert game entry");
    drop(conn);

    let update_output = run_memo_cli(
        &db_path,
        &["--json", "update", item_id_str, "buy 2tb ssd for mom"],
        None,
    );
    assert_eq!(
        update_output.code,
        0,
        "update failed: {}",
        update_output.stderr_text()
    );
    let update_json = parse_json_stdout(&update_output);
    assert_eq!(update_json["schema_version"], "cli.memo-cli.update.v1");
    assert_eq!(update_json["data"]["state"], "pending");

    let fetch_after_update = run_memo_cli(&db_path, &["--json", "fetch", "--limit", "20"], None);
    assert_eq!(
        fetch_after_update.code,
        0,
        "fetch after update failed: {}",
        fetch_after_update.stderr_text()
    );
    let fetch_after_update_json = parse_json_stdout(&fetch_after_update);
    let rows = fetch_after_update_json["data"]["items"]
        .as_array()
        .expect("results array should exist");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["item_id"], item_id_str);

    let search_old = run_memo_cli(
        &db_path,
        &["--json", "search", "shopping", "--limit", "10"],
        None,
    );
    assert_eq!(
        search_old.code,
        0,
        "search after update failed: {}",
        search_old.stderr_text()
    );
    let search_old_json = parse_json_stdout(&search_old);
    let search_old_rows = search_old_json["data"]["items"]
        .as_array()
        .expect("search results should be array");
    assert!(
        search_old_rows.is_empty(),
        "old derived tags should be gone"
    );

    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let derivation_count: i64 = conn
        .query_row(
            "select count(*) from item_derivations where item_id = ?1",
            rusqlite::params![item_id],
            |row| row.get(0),
        )
        .expect("query derivation count");
    assert_eq!(derivation_count, 0);
    let anchor_count: i64 = conn
        .query_row(
            "select count(*) from workflow_item_anchors where item_id = ?1",
            rusqlite::params![item_id],
            |row| row.get(0),
        )
        .expect("query workflow anchor count");
    assert_eq!(anchor_count, 0);
    let game_entry_count: i64 = conn
        .query_row("select count(*) from workflow_game_entries", [], |row| {
            row.get(0)
        })
        .expect("query workflow game count");
    assert_eq!(game_entry_count, 0);
    drop(conn);

    let delete_without_yes = run_memo_cli(&db_path, &["delete", item_id_str], None);
    assert_eq!(delete_without_yes.code, 64);
    assert!(
        delete_without_yes
            .stderr_text()
            .contains("delete requires --yes"),
        "stderr={}",
        delete_without_yes.stderr_text()
    );

    let delete_with_compat_hard = run_memo_cli(&db_path, &["delete", item_id_str, "--hard"], None);
    assert_eq!(delete_with_compat_hard.code, 64);
    assert!(
        delete_with_compat_hard
            .stderr_text()
            .contains("--hard no longer confirms deletion"),
        "stderr={}",
        delete_with_compat_hard.stderr_text()
    );

    let delete_output = run_memo_cli(&db_path, &["--json", "delete", item_id_str, "--yes"], None);
    assert_eq!(
        delete_output.code,
        0,
        "delete failed: {}",
        delete_output.stderr_text()
    );
    let delete_json = parse_json_stdout(&delete_output);
    assert_eq!(delete_json["schema_version"], "cli.memo-cli.delete.v1");
    assert_eq!(delete_json["data"]["deleted"], true);

    let list_after_delete = run_memo_cli(&db_path, &["--json", "list", "--limit", "20"], None);
    assert_eq!(
        list_after_delete.code,
        0,
        "list after delete failed: {}",
        list_after_delete.stderr_text()
    );
    let list_after_delete_json = parse_json_stdout(&list_after_delete);
    assert_eq!(
        list_after_delete_json["data"]["items"]
            .as_array()
            .expect("list results array")
            .len(),
        0
    );

    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let inbox_count: i64 = conn
        .query_row("select count(*) from inbox_items", [], |row| row.get(0))
        .expect("query inbox count");
    let search_doc_count: i64 = conn
        .query_row("select count(*) from item_search_documents", [], |row| {
            row.get(0)
        })
        .expect("query search docs count");
    assert_eq!(inbox_count, 0);
    assert_eq!(search_doc_count, 0);
}
