use pretty_assertions::assert_eq;

use crate::support;
use support::{parse_json_stdout, run_memo, test_db};

#[test]
fn report_custom_window() {
    let (_db_dir, db_path) = test_db("report_custom_window");

    let add_first = run_memo(
        &db_path,
        &[
            "--json",
            "add",
            "--at",
            "2026-02-01T12:00:00Z",
            "window item one",
        ],
        None,
    );
    assert_eq!(
        add_first.code,
        0,
        "first add failed: {}",
        add_first.stderr_text()
    );

    let add_second = run_memo(
        &db_path,
        &[
            "--json",
            "add",
            "--at",
            "2026-03-01T12:00:00Z",
            "window item two",
        ],
        None,
    );
    assert_eq!(
        add_second.code,
        0,
        "second add failed: {}",
        add_second.stderr_text()
    );

    let report = run_memo(
        &db_path,
        &[
            "--json",
            "report",
            "month",
            "--from",
            "2026-02-01T00:00:00Z",
            "--to",
            "2026-02-28T23:59:59Z",
        ],
        None,
    );
    assert_eq!(report.code, 0, "report failed: {}", report.stderr_text());
    let report_json = parse_json_stdout(&report);
    assert_eq!(report_json["ok"], true);
    assert_eq!(report_json["data"]["totals"]["captured"], 1);
}

#[test]
fn report_timezone() {
    let (_db_dir, db_path) = test_db("report_timezone");

    let add = run_memo(
        &db_path,
        &["--json", "add", "--at", "2026-02-12T10:00:00Z", "tz seed"],
        None,
    );
    assert_eq!(add.code, 0, "add failed: {}", add.stderr_text());

    let report = run_memo(
        &db_path,
        &["--json", "report", "week", "--tz", "Asia/Taipei"],
        None,
    );
    assert_eq!(report.code, 0, "report failed: {}", report.stderr_text());
    let report_json = parse_json_stdout(&report);
    assert_eq!(report_json["ok"], true);
    assert_eq!(report_json["data"]["range"]["timezone"], "Asia/Taipei");
}

#[test]
fn report_rejects_invalid_custom_range() {
    let (_db_dir, db_path) = test_db("report_rejects_invalid_custom_range");

    let report = run_memo(
        &db_path,
        &[
            "--json",
            "report",
            "week",
            "--from",
            "2026-03-01T00:00:00Z",
            "--to",
            "2026-02-01T00:00:00Z",
        ],
        None,
    );
    assert_eq!(report.code, 64);
    let report_json = parse_json_stdout(&report);
    assert_eq!(report_json["ok"], false);
    assert_eq!(report_json["error"]["code"], "invalid-time-range");
}
