use memo::output::parse_item_id;
use pretty_assertions::assert_eq;

use crate::support;
use support::{parse_json_stdout, run_memo, test_db_path};

#[test]
fn hard_delete_cleans_anchor_and_typed_workflow_rows() {
    let db_path = test_db_path("hard_delete_cleans_anchor_and_typed_workflow_rows");

    let add_output = run_memo(
        &db_path,
        &["--json", "add", "track game release dates"],
        None,
    );
    assert_eq!(
        add_output.code,
        0,
        "add failed: {}",
        add_output.stderr_text()
    );
    let add_json = parse_json_stdout(&add_output);
    let item_id_str = add_json["data"]["item_id"]
        .as_str()
        .expect("item_id should be string");
    let item_id = parse_item_id(item_id_str).expect("item_id should parse");

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
         values(?1, 'hades ii', 'https://example.com/hades2', 'wishlist')",
        rusqlite::params![anchor_id],
    )
    .expect("insert game row");
    drop(conn);

    let delete_output = run_memo(&db_path, &["--json", "delete", item_id_str, "--yes"], None);
    assert_eq!(
        delete_output.code,
        0,
        "delete failed: {}",
        delete_output.stderr_text()
    );

    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let anchor_count: i64 = conn
        .query_row("select count(*) from workflow_item_anchors", [], |row| {
            row.get(0)
        })
        .expect("anchor count");
    let game_count: i64 = conn
        .query_row("select count(*) from workflow_game_entries", [], |row| {
            row.get(0)
        })
        .expect("game count");
    assert_eq!(anchor_count, 0);
    assert_eq!(game_count, 0);
}
