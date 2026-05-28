use super::common;

const LEDGER_FIXTURE: &str = "# Demo Execution State

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | pending | Implement A |  | first row |
| 1.2 | pending | Implement B | https://example.test/old | second row |
| 2.1 | pending | Release |  |  |
";

// One state-role payload citing only Task 1.1 and Task 1.2, decoded from this
// JSON: {"schema":"...","role":"state","data":{"tasks":[
//   {"id":"1.1","status":"in-progress","title":"selected"},
//   {"id":"1.2","status":"in-progress","title":"selected"}]}}
// The hex below is that JSON encoded byte-by-byte.
const STATE_PAYLOAD_HEX: &str = concat!(
    "7b22726f6c65223a227374617465222c2264617461223a7b227461736b73223a5b",
    "7b226964223a22312e31222c22737461747573223a22696e2d70726f6772657373",
    "227d2c7b226964223a22312e32222c22737461747573223a22696e2d70726f6772",
    "657373227d5d7d7d"
);

fn comments_json(url: &str, payload_hex: &str) -> String {
    serde_json::json!({
        "comments": [
            {
                "body": format!("body\n\n<!-- plan-issue-record-payload:hex:{payload_hex} -->\n"),
                "url": url,
            }
        ]
    })
    .to_string()
}

#[test]
fn ledger_sync_reports_match_drift_missing() {
    let repo = common::init_repo();
    let bundle = repo.path().join("docs/plans/demo");
    common::write_file(&bundle.join("demo-execution-state.md"), LEDGER_FIXTURE);

    // Two tasks (1.1, 1.2) cited by the state payload at url=https://example.test/state.
    // Task 1.1 ledger Evidence is empty → drift (issue has a URL).
    // Task 1.2 ledger Evidence cites a different URL → drift.
    // Task 2.1 not cited by any state payload → missing.
    let url = "https://example.test/state";
    let comments_path = repo.path().join("docs/plans/demo/comments.json");
    common::write_file(&comments_path, &comments_json(url, STATE_PAYLOAD_HEX));
    let body_path = repo.path().join("docs/plans/demo/body.md");
    common::write_file(&body_path, "issue body");

    let out = common::run_plan_tooling(
        repo.path(),
        &[
            "ledger-sync",
            "--bundle",
            "docs/plans/demo",
            "--body-file",
            "docs/plans/demo/body.md",
            "--comments-json",
            "docs/plans/demo/comments.json",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        out.code, 0,
        "stderr: {}\nstdout: {}",
        out.stderr, out.stdout
    );
    let value: serde_json::Value = serde_json::from_str(&out.stdout).expect("json parses");
    assert_eq!(value["ok"], serde_json::Value::Bool(true));
    let entries = value["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 3, "one entry per ledger row");
    let by_id: std::collections::HashMap<String, &serde_json::Value> = entries
        .iter()
        .map(|e| (e["task_id"].as_str().unwrap().to_string(), e))
        .collect();
    assert_eq!(by_id["1.1"]["action"], "drift");
    assert_eq!(by_id["1.1"]["issue_evidence"], url);
    assert_eq!(by_id["1.2"]["action"], "drift");
    assert_eq!(by_id["2.1"]["action"], "missing");
}

#[test]
fn ledger_sync_write_fills_empty_evidence_cells() {
    let repo = common::init_repo();
    let bundle = repo.path().join("docs/plans/demo");
    common::write_file(&bundle.join("demo-execution-state.md"), LEDGER_FIXTURE);
    let url = "https://example.test/state";
    common::write_file(
        &bundle.join("comments.json"),
        &comments_json(url, STATE_PAYLOAD_HEX),
    );
    common::write_file(&bundle.join("body.md"), "issue body");

    let out = common::run_plan_tooling(
        repo.path(),
        &[
            "ledger-sync",
            "--bundle",
            "docs/plans/demo",
            "--body-file",
            "docs/plans/demo/body.md",
            "--comments-json",
            "docs/plans/demo/comments.json",
            "--write",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let value: serde_json::Value = serde_json::from_str(&out.stdout).expect("json parses");
    let patched: Vec<String> = value["patched"]
        .as_array()
        .expect("patched array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        patched.contains(&"1.1".to_string()),
        "1.1 patched (was empty)"
    );
    assert!(
        !patched.contains(&"1.2".to_string()),
        "1.2 not patched (had value)"
    );

    let new_text = std::fs::read_to_string(bundle.join("demo-execution-state.md")).unwrap();
    assert!(
        new_text
            .contains("| 1.1 | pending | Implement A | https://example.test/state | first row |")
    );
    // Existing value preserved.
    assert!(
        new_text
            .contains("| 1.2 | pending | Implement B | https://example.test/old | second row |")
    );
}

#[test]
fn ledger_sync_requires_evidence_inputs() {
    let repo = common::init_repo();
    common::write_file(
        &repo.path().join("docs/plans/demo/demo-execution-state.md"),
        LEDGER_FIXTURE,
    );

    let out = common::run_plan_tooling(
        repo.path(),
        &[
            "ledger-sync",
            "--bundle",
            "docs/plans/demo",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 2, "usage error");
}
