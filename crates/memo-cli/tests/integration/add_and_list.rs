use std::path::PathBuf;

use memo_cli::app;
use memo_cli::storage::Storage;
use memo_cli::storage::repository::{self, QueryState};
use pretty_assertions::assert_eq;

fn test_db_path(name: &str) -> PathBuf {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    dir.keep().join(format!("{name}.db"))
}

#[test]
fn add_and_list() {
    let db_path = test_db_path("add_and_list");
    let storage = Storage::new(db_path);
    storage
        .with_transaction(|tx| {
            repository::add_item(tx, "buy 1tb ssd for mom", "cli", None)?;
            repository::add_item(tx, "book pediatric dentist appointment", "cli", None)?;
            Ok(())
        })
        .expect("seed should succeed");

    let rows = storage
        .with_connection(|conn| repository::list_items(conn, QueryState::All, 20, 0))
        .expect("list should succeed");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].text_preview, "book pediatric dentist appointment");
    assert_eq!(rows[1].text_preview, "buy 1tb ssd for mom");
}

#[test]
fn add_and_list_json() {
    let db_path = test_db_path("add_and_list_json");
    let db = db_path.display().to_string();

    let add_rc = app::run_with_args([
        "memo-cli",
        "--db",
        &db,
        "--json",
        "add",
        "book two parenting books",
    ]);
    assert_eq!(add_rc, 0);

    let list_rc = app::run_with_args(["memo-cli", "--db", &db, "--json", "list", "--limit", "20"]);
    assert_eq!(list_rc, 0);
}

#[test]
fn add_with_at() {
    let db_path = test_db_path("add_with_at");
    let db = db_path.display().to_string();

    let add_rc = app::run_with_args([
        "memo-cli",
        "--db",
        &db,
        "add",
        "--at",
        "2026-02-12T10:00:00+08:00",
        "seed with explicit time",
    ]);
    assert_eq!(add_rc, 0);

    let storage = Storage::new(db_path);
    let rows = storage
        .with_connection(|conn| repository::list_items(conn, QueryState::All, 20, 0))
        .expect("list should succeed");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].created_at, "2026-02-12T02:00:00.000Z");
}

#[test]
fn add_rejects_invalid_at() {
    let db_path = test_db_path("add_rejects_invalid_at");
    let db = db_path.display().to_string();

    let add_rc = app::run_with_args([
        "memo-cli",
        "--db",
        &db,
        "add",
        "--at",
        "invalid-timestamp",
        "seed with invalid time",
    ]);
    assert_eq!(add_rc, 64);
}
