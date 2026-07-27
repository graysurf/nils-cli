use std::collections::HashMap;

use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde_json::json;

use crate::support;
use support::{fixture_json, parse_json_stdout, run_memo, test_db};

#[derive(Debug, Deserialize)]
struct MemoSeedFixture {
    captures: Vec<CaptureSeed>,
    search_query: String,
    expected: ExpectedSeed,
}

#[derive(Debug, Deserialize)]
struct CaptureSeed {
    text: String,
    source: String,
    enrichment: EnrichmentSeed,
}

#[derive(Debug, Deserialize)]
struct EnrichmentSeed {
    summary: String,
    category: String,
    normalized_text: String,
    confidence: f64,
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ExpectedSeed {
    captured: i64,
    search_min_hits: usize,
}

fn load_fixture() -> MemoSeedFixture {
    let raw = fixture_json("memo_seed.json");
    serde_json::from_value(raw).expect("memo_seed fixture should be valid")
}

#[test]
fn memo_flow_capture_fetch_apply_search_report() {
    let fixture = load_fixture();
    let (_db_dir, db_path) = test_db("memo_flow_capture_fetch_apply_search_report");

    for capture in &fixture.captures {
        let add_output = run_memo(
            &db_path,
            &["--json", "add", "--source", &capture.source, &capture.text],
            None,
        );
        assert_eq!(
            add_output.code,
            0,
            "add command failed for '{}': {}",
            capture.text,
            add_output.stderr_text()
        );
    }

    let fetch_output = run_memo(&db_path, &["--json", "fetch", "--limit", "50"], None);
    assert_eq!(
        fetch_output.code,
        0,
        "fetch command failed: {}",
        fetch_output.stderr_text()
    );
    let fetch_json = parse_json_stdout(&fetch_output);
    let fetch_rows = fetch_json["data"]["items"]
        .as_array()
        .expect("fetch results should be an array");
    assert_eq!(
        fetch_rows.len(),
        fixture.captures.len(),
        "fetch should return all pending captures before apply"
    );

    let mut item_id_by_text: HashMap<String, String> = HashMap::new();
    for row in fetch_rows {
        let text = row["text"]
            .as_str()
            .expect("fetch row text should be present")
            .to_string();
        let item_id = row["item_id"]
            .as_str()
            .expect("fetch row item_id should be present")
            .to_string();
        item_id_by_text.insert(text, item_id);
    }

    let apply_items = fixture
        .captures
        .iter()
        .enumerate()
        .map(|(index, capture)| {
            let item_id = item_id_by_text.get(&capture.text).unwrap_or_else(|| {
                panic!(
                    "missing fetched item_id for fixture text '{}'",
                    capture.text
                )
            });
            json!({
                "item_id": item_id,
                "derivation_hash": format!("memo-flow-hash-{}", index + 1),
                "summary": capture.enrichment.summary,
                "category": capture.enrichment.category,
                "normalized_text": capture.enrichment.normalized_text,
                "confidence": capture.enrichment.confidence,
                "tags": capture.enrichment.tags,
                "payload": {
                    "source": "memo-flow-fixture"
                }
            })
        })
        .collect::<Vec<_>>();

    let apply_payload = json!({
        "agent_run_id": "agent-run-memo-flow",
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
        "apply command failed: {}",
        apply_output.stderr_text()
    );
    let apply_json = parse_json_stdout(&apply_output);
    assert_eq!(apply_json["data"]["processed"], fixture.expected.captured);
    assert_eq!(apply_json["data"]["accepted"], fixture.expected.captured);
    assert_eq!(apply_json["data"]["failed"], 0);

    let search_output = run_memo(
        &db_path,
        &["--json", "search", &fixture.search_query, "--limit", "20"],
        None,
    );
    assert_eq!(
        search_output.code,
        0,
        "search command failed: {}",
        search_output.stderr_text()
    );
    let search_json = parse_json_stdout(&search_output);
    let search_rows = search_json["data"]["items"]
        .as_array()
        .expect("search results should be an array");
    assert!(
        search_rows.len() >= fixture.expected.search_min_hits,
        "search results count {} was below expected minimum {}",
        search_rows.len(),
        fixture.expected.search_min_hits
    );
    assert!(
        search_rows.iter().any(|row| {
            row["preview"]
                .as_str()
                .map(|value| value.to_lowercase().contains("tokyo"))
                .unwrap_or(false)
        }),
        "search results should include at least one tokyo preview"
    );

    let report_output = run_memo(&db_path, &["--json", "report", "week"], None);
    assert_eq!(
        report_output.code,
        0,
        "report command failed: {}",
        report_output.stderr_text()
    );
    let report_json = parse_json_stdout(&report_output);
    assert_eq!(
        report_json["data"]["totals"]["captured"],
        fixture.expected.captured
    );
    assert_eq!(
        report_json["data"]["totals"]["enriched"],
        fixture.expected.captured
    );
    assert_eq!(report_json["data"]["totals"]["pending"], 0);
}
