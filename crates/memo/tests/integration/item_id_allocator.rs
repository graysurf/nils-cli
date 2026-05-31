use memo::output::parse_item_id;
use pretty_assertions::assert_eq;

use crate::support;
use support::{parse_json_stdout, run_memo, test_db_path};

#[test]
fn hard_delete_does_not_reuse_item_ids() {
    let db_path = test_db_path("hard_delete_does_not_reuse_item_ids");

    let first_add = run_memo(&db_path, &["--json", "add", "first item"], None);
    assert_eq!(
        first_add.code,
        0,
        "first add failed: {}",
        first_add.stderr_text()
    );
    let first_add_json = parse_json_stdout(&first_add);
    let first_item_id_str = first_add_json["data"]["item_id"]
        .as_str()
        .expect("first item_id should be a string");
    let first_item_id = parse_item_id(first_item_id_str).expect("first item_id should parse");

    let delete_first = run_memo(
        &db_path,
        &["--json", "delete", first_item_id_str, "--yes"],
        None,
    );
    assert_eq!(
        delete_first.code,
        0,
        "hard delete failed: {}",
        delete_first.stderr_text()
    );

    let second_add = run_memo(&db_path, &["--json", "add", "second item"], None);
    assert_eq!(
        second_add.code,
        0,
        "second add failed: {}",
        second_add.stderr_text()
    );
    let second_add_json = parse_json_stdout(&second_add);
    let second_item_id_str = second_add_json["data"]["item_id"]
        .as_str()
        .expect("second item_id should be a string");
    let second_item_id = parse_item_id(second_item_id_str).expect("second item_id should parse");

    assert!(
        second_item_id > first_item_id,
        "item_id must be monotonic and non-reused (first={first_item_id}, second={second_item_id})"
    );
}

#[test]
fn allocator_row_backfills_from_existing_max_item_id() {
    let db_path = test_db_path("allocator_row_backfills_from_existing_max_item_id");

    let first_add = run_memo(&db_path, &["--json", "add", "seed one"], None);
    assert_eq!(
        first_add.code,
        0,
        "first add failed: {}",
        first_add.stderr_text()
    );
    let first_item_id_str = parse_json_stdout(&first_add)["data"]["item_id"]
        .as_str()
        .expect("first item_id should be a string")
        .to_string();
    let first_item_id = parse_item_id(&first_item_id_str).expect("first item_id should parse");

    let second_add = run_memo(&db_path, &["--json", "add", "seed two"], None);
    assert_eq!(
        second_add.code,
        0,
        "second add failed: {}",
        second_add.stderr_text()
    );
    let second_item_id_str = parse_json_stdout(&second_add)["data"]["item_id"]
        .as_str()
        .expect("second item_id should be a string")
        .to_string();
    let second_item_id = parse_item_id(&second_item_id_str).expect("second item_id should parse");
    assert!(
        second_item_id > first_item_id,
        "setup requires monotonic item ids (first={first_item_id}, second={second_item_id})"
    );

    let conn = rusqlite::Connection::open(&db_path).expect("open db for allocator backfill setup");
    conn.execute("delete from id_allocators where name = 'inbox_items'", [])
        .expect("remove inbox_items allocator row");
    drop(conn);

    let third_add = run_memo(&db_path, &["--json", "add", "post migration"], None);
    assert_eq!(
        third_add.code,
        0,
        "third add failed: {}",
        third_add.stderr_text()
    );
    let third_item_id_str = parse_json_stdout(&third_add)["data"]["item_id"]
        .as_str()
        .expect("third item_id should be a string")
        .to_string();
    let third_item_id = parse_item_id(&third_item_id_str).expect("third item_id should parse");

    assert!(
        third_item_id > second_item_id,
        "allocator backfill must seed from current max item_id (second={second_item_id}, third={third_item_id})"
    );
}
