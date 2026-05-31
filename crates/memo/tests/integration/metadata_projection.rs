use std::collections::HashMap;

use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde_json::json;

use crate::support;
use support::{fixture_json, parse_json_stdout, run_memo, test_db_path};

#[derive(Debug, Deserialize)]
struct FormatCase {
    text: String,
    expected_content_type: String,
    expected_validation_status: String,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    format_cases: Vec<FormatCase>,
}

fn load_fixture() -> Fixture {
    let raw = fixture_json("memo_seed.json");
    serde_json::from_value(raw).expect("fixture should parse")
}

#[test]
fn metadata_projection_search() {
    let db_path = test_db_path("metadata_projection_search");

    let add_output = run_memo(&db_path, &["--json", "add", "{\"task\":}"], None);
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
        .expect("item_id should exist");

    let apply_payload = json!({
        "items": [{
            "item_id": item_id,
            "derivation_hash": "metadata-projection-search",
            "summary": "invalid json metadata projection",
            "payload": {"source":"metadata-projection"}
        }]
    });
    let apply_output = run_memo(
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

    let search_output = run_memo(&db_path, &["--json", "search", "task"], None);
    assert_eq!(
        search_output.code,
        0,
        "search failed: {}",
        search_output.stderr_text()
    );
    let search_json = parse_json_stdout(&search_output);
    let first = &search_json["data"]["items"][0];
    assert_eq!(first["content_type"], "json");
    assert_eq!(first["validation_status"], "invalid");
}

#[test]
fn metadata_projection_report() {
    let db_path = test_db_path("metadata_projection_report");
    let fixture = load_fixture();

    for case in &fixture.format_cases {
        let add_output = run_memo(&db_path, &["--json", "add", &case.text], None);
        assert_eq!(
            add_output.code,
            0,
            "add failed for {}: {}",
            case.text,
            add_output.stderr_text()
        );
    }

    let fetch_output = run_memo(&db_path, &["--json", "fetch", "--limit", "100"], None);
    assert_eq!(
        fetch_output.code,
        0,
        "fetch failed: {}",
        fetch_output.stderr_text()
    );
    let fetch_json = parse_json_stdout(&fetch_output);
    let rows = fetch_json["data"]["items"]
        .as_array()
        .expect("fetch results should be an array");

    let apply_items = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            json!({
                "item_id": row["item_id"],
                "derivation_hash": format!("metadata-projection-report-{index}"),
                "summary": row["text"],
                "payload": {"source":"metadata-projection"}
            })
        })
        .collect::<Vec<_>>();
    let apply_payload = json!({
        "agent_run_id": "metadata-projection-report",
        "items": apply_items
    });

    let apply_output = run_memo(
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

    let list_output = run_memo(
        &db_path,
        &["--json", "list", "--state", "enriched", "--limit", "100"],
        None,
    );
    assert_eq!(
        list_output.code,
        0,
        "list failed: {}",
        list_output.stderr_text()
    );
    let list_json = parse_json_stdout(&list_output);
    let list_rows = list_json["data"]["items"]
        .as_array()
        .expect("list results should be an array");
    let mut seen_pairs = HashMap::new();
    for row in list_rows {
        let content_type = row["content_type"]
            .as_str()
            .expect("content_type should be present");
        let validation_status = row["validation_status"]
            .as_str()
            .expect("validation_status should be present");
        *seen_pairs
            .entry((content_type.to_string(), validation_status.to_string()))
            .or_insert(0_i64) += 1;
    }

    for case in &fixture.format_cases {
        assert!(
            seen_pairs.contains_key(&(
                case.expected_content_type.clone(),
                case.expected_validation_status.clone()
            )),
            "missing metadata pair: {} / {}",
            case.expected_content_type,
            case.expected_validation_status
        );
    }

    let report_output = run_memo(&db_path, &["--json", "report", "week"], None);
    assert_eq!(
        report_output.code,
        0,
        "report failed: {}",
        report_output.stderr_text()
    );
    let report_json = parse_json_stdout(&report_output);

    let content_types = report_json["data"]["top_content_types"]
        .as_array()
        .expect("top_content_types should be an array");
    let mut content_type_names = Vec::new();
    for row in content_types {
        if let Some(name) = row["name"].as_str() {
            content_type_names.push(name.to_string());
        }
    }
    for expected in ["url", "json", "yaml", "xml", "markdown", "text"] {
        assert!(
            content_type_names.iter().any(|name| name == expected),
            "missing expected content type in report: {expected}"
        );
    }

    let status_totals = report_json["data"]["validation_status_totals"]
        .as_array()
        .expect("validation_status_totals should be an array");
    let mut status_names = Vec::new();
    for row in status_totals {
        if let Some(name) = row["name"].as_str() {
            status_names.push(name.to_string());
        }
    }
    for expected in ["valid", "invalid", "skipped"] {
        assert!(
            status_names.iter().any(|name| name == expected),
            "missing expected validation status in report: {expected}"
        );
    }

    let conn = rusqlite::Connection::open(&db_path).expect("open db for tag checks");
    let fmt_tag_count: i64 = conn
        .query_row(
            "select count(*)
             from item_tags it
             join tags t on t.tag_id = it.tag_id
             where t.tag_name_norm like 'fmt:%'",
            [],
            |row| row.get(0),
        )
        .expect("fmt tag count");
    let val_tag_count: i64 = conn
        .query_row(
            "select count(*)
             from item_tags it
             join tags t on t.tag_id = it.tag_id
             where t.tag_name_norm like 'val:%'",
            [],
            |row| row.get(0),
        )
        .expect("val tag count");
    assert!(fmt_tag_count > 0, "expected fmt:* tags to be persisted");
    assert!(val_tag_count > 0, "expected val:* tags to be persisted");
}
