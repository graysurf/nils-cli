use std::path::PathBuf;

use memo_cli::storage::Storage;
use memo_cli::storage::repository::{self, QueryState};
use memo_cli::storage::search::{self, ReportPeriod};
use pretty_assertions::assert_eq;

fn test_db_path(name: &str) -> PathBuf {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    dir.keep().join(format!("{name}.db"))
}

#[test]
fn search_and_report() {
    let db_path = test_db_path("search_and_report");
    let storage = Storage::new(db_path);

    storage
        .with_transaction(|tx| {
            let first = repository::add_item(tx, "3/24 in tokyo", "cli", None)?;
            let second = repository::add_item(tx, "buy 1tb ssd for mom", "cli", None)?;

            tx.execute(
                "insert into item_derivations(
                    item_id,
                    derivation_version,
                    status,
                    is_active,
                    base_derivation_id,
                    derivation_hash,
                    agent_run_id,
                    summary,
                    category,
                    priority,
                    due_at,
                    normalized_text,
                    confidence,
                    payload_json,
                    conflict_reason
                ) values (?1, 1, 'accepted', 1, null, ?2, 'agent-run-1', ?3, ?4, null, null, ?5, 0.9, ?6, null)",
                rusqlite::params![
                    first.item_id,
                    format!("hash-{}", first.item_id),
                    "tokyo travel event",
                    "travel",
                    "tokyo travel event",
                    "{\"ok\":true}"
                ],
            )
            .map_err(memo_cli::errors::AppError::db)?;

            tx.execute(
                "insert into item_derivations(
                    item_id,
                    derivation_version,
                    status,
                    is_active,
                    base_derivation_id,
                    derivation_hash,
                    agent_run_id,
                    summary,
                    category,
                    priority,
                    due_at,
                    normalized_text,
                    confidence,
                    payload_json,
                    conflict_reason
                ) values (?1, 1, 'accepted', 1, null, ?2, 'agent-run-2', ?3, ?4, null, null, ?5, 0.8, ?6, null)",
                rusqlite::params![
                    second.item_id,
                    format!("hash-{}", second.item_id),
                    "buy ssd for mom",
                    "shopping",
                    "buy ssd for mom",
                    "{\"ok\":true}"
                ],
            )
            .map_err(memo_cli::errors::AppError::db)?;

            Ok(())
        })
        .expect("seed should succeed");

    let search_rows = storage
        .with_connection(|conn| {
            search::search_items(
                conn,
                "tokyo",
                QueryState::All,
                &[
                    search::SearchField::Raw,
                    search::SearchField::Derived,
                    search::SearchField::Tags,
                ],
                search::SearchMatchMode::Fts,
                20,
            )
        })
        .expect("search should succeed");

    assert!(!search_rows.is_empty());
    assert!(
        search_rows
            .iter()
            .any(|row| row.preview.to_lowercase().contains("tokyo"))
    );

    let report = storage
        .with_connection(|conn| search::report_summary(conn, ReportPeriod::Week))
        .expect("report should succeed");

    assert_eq!(report.period, "week");
    assert!(report.totals.captured >= 2);
}

#[test]
fn search_supports_field_filters() {
    let db_path = test_db_path("search_supports_field_filters");
    let storage = Storage::new(db_path);

    let (raw_item_id, tagged_item_id) = storage
        .with_transaction(|tx| {
            let raw_item = repository::add_item(tx, "sharedterm appears in raw text", "cli", None)?;
            let tagged_item = repository::add_item(tx, "this row has no shared term in raw text", "cli", None)?;

            tx.execute(
                "insert into item_derivations(
                    item_id,
                    derivation_version,
                    status,
                    is_active,
                    base_derivation_id,
                    derivation_hash,
                    agent_run_id,
                    summary,
                    category,
                    priority,
                    due_at,
                    normalized_text,
                    confidence,
                    payload_json,
                    conflict_reason
                ) values (?1, 1, 'accepted', 1, null, ?2, 'agent-run-tag', 'nohit', 'misc', null, null, 'nohit', 0.7, ?3, null)",
                rusqlite::params![
                    tagged_item.item_id,
                    format!("hash-{}", tagged_item.item_id),
                    "{\"ok\":true}"
                ],
            )
            .map_err(memo_cli::errors::AppError::db)?;

            let derivation_id: i64 = tx
                .query_row(
                    "select derivation_id from item_derivations where item_id = ?1 and derivation_version = 1",
                    rusqlite::params![tagged_item.item_id],
                    |row| row.get(0),
                )
                .map_err(memo_cli::errors::AppError::db_query)?;

            tx.execute(
                "insert into tags(tag_name, tag_name_norm) values ('sharedterm', 'sharedterm')",
                [],
            )
            .map_err(memo_cli::errors::AppError::db)?;
            let tag_id: i64 = tx
                .query_row(
                    "select tag_id from tags where tag_name_norm = 'sharedterm'",
                    [],
                    |row| row.get(0),
                )
                .map_err(memo_cli::errors::AppError::db_query)?;

            tx.execute(
                "insert into item_tags(derivation_id, tag_id) values (?1, ?2)",
                rusqlite::params![derivation_id, tag_id],
            )
            .map_err(memo_cli::errors::AppError::db)?;

            Ok((raw_item.item_id, tagged_item.item_id))
        })
        .expect("seed should succeed");

    let raw_rows = storage
        .with_connection(|conn| {
            search::search_items(
                conn,
                "sharedterm",
                QueryState::All,
                &[search::SearchField::Raw],
                search::SearchMatchMode::Fts,
                20,
            )
        })
        .expect("raw field search should succeed");
    assert_eq!(raw_rows.len(), 1);
    assert_eq!(raw_rows[0].item_id, raw_item_id);

    let tag_rows = storage
        .with_connection(|conn| {
            search::search_items(
                conn,
                "sharedterm",
                QueryState::All,
                &[search::SearchField::Tags],
                search::SearchMatchMode::Fts,
                20,
            )
        })
        .expect("tags field search should succeed");
    assert_eq!(tag_rows.len(), 1);
    assert_eq!(tag_rows[0].item_id, tagged_item_id);

    let raw_and_tag_rows = storage
        .with_connection(|conn| {
            search::search_items(
                conn,
                "sharedterm",
                QueryState::All,
                &[search::SearchField::Raw, search::SearchField::Tags],
                search::SearchMatchMode::Fts,
                20,
            )
        })
        .expect("raw+tags field search should succeed");
    let matched_ids = raw_and_tag_rows
        .iter()
        .map(|row| row.item_id)
        .collect::<Vec<_>>();

    assert!(matched_ids.contains(&raw_item_id));
    assert!(matched_ids.contains(&tagged_item_id));
}

#[test]
fn search_match_modes_support_prefix_and_contains() {
    let db_path = test_db_path("search_match_modes_support_prefix_and_contains");
    let storage = Storage::new(db_path);

    let item_id = storage
        .with_transaction(|tx| {
            let item = repository::add_item(tx, "123 target", "cli", None)?;
            Ok(item.item_id)
        })
        .expect("seed should succeed");

    let fts_rows = storage
        .with_connection(|conn| {
            search::search_items(
                conn,
                "12",
                QueryState::All,
                &[search::SearchField::Raw],
                search::SearchMatchMode::Fts,
                20,
            )
        })
        .expect("fts search should succeed");
    assert!(fts_rows.is_empty());

    let prefix_rows = storage
        .with_connection(|conn| {
            search::search_items(
                conn,
                "12",
                QueryState::All,
                &[search::SearchField::Raw],
                search::SearchMatchMode::Prefix,
                20,
            )
        })
        .expect("prefix search should succeed");
    assert_eq!(prefix_rows.len(), 1);
    assert_eq!(prefix_rows[0].item_id, item_id);

    let contains_rows = storage
        .with_connection(|conn| {
            search::search_items(
                conn,
                "23",
                QueryState::All,
                &[search::SearchField::Raw],
                search::SearchMatchMode::Contains,
                20,
            )
        })
        .expect("contains search should succeed");
    assert_eq!(contains_rows.len(), 1);
    assert_eq!(contains_rows[0].item_id, item_id);
}
