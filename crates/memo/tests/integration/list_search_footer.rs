use pretty_assertions::assert_eq;

use crate::support::{parse_json_stdout, run_memo, test_db};

fn add_items(db_path: &std::path::Path, count: usize, prefix: &str) {
    for idx in 0..count {
        let output = run_memo(db_path, &["add", &format!("{prefix} item {idx}")], None);
        assert_eq!(output.code, 0, "add failed: {}", output.stderr_text());
    }
}

#[test]
fn list_text_footer_appears_only_when_limit_is_reached() {
    let (_db_dir, db_path) = test_db("list_text_footer");
    add_items(&db_path, 3, "visible");

    let truncated = run_memo(&db_path, &["list", "--limit", "2"], None);
    assert_eq!(
        truncated.code,
        0,
        "list failed: {}",
        truncated.stderr_text()
    );
    let stdout = truncated.stdout_text();
    assert!(stdout.contains("(showing 2 items with --limit 2;"));
    assert!(stdout.contains("--offset 2"));

    let complete = run_memo(&db_path, &["list", "--limit", "5"], None);
    assert_eq!(complete.code, 0, "list failed: {}", complete.stderr_text());
    assert!(!complete.stdout_text().contains("(showing"));
}

#[test]
fn list_json_reports_truncation_state() {
    let (_db_dir, db_path) = test_db("list_json_truncated");
    add_items(&db_path, 3, "json");

    let truncated = run_memo(&db_path, &["--json", "list", "--limit", "2"], None);
    assert_eq!(
        truncated.code,
        0,
        "list failed: {}",
        truncated.stderr_text()
    );
    let truncated_json = parse_json_stdout(&truncated);
    assert_eq!(truncated_json["data"]["pagination"]["truncated"], true);

    let complete = run_memo(&db_path, &["--json", "list", "--limit", "5"], None);
    assert_eq!(complete.code, 0, "list failed: {}", complete.stderr_text());
    let complete_json = parse_json_stdout(&complete);
    assert_eq!(complete_json["data"]["pagination"]["truncated"], false);
}

#[test]
fn search_text_footer_appears_only_when_limit_is_reached() {
    let (_db_dir, db_path) = test_db("search_text_footer");
    add_items(&db_path, 3, "needle");

    let truncated = run_memo(&db_path, &["search", "needle", "--limit", "2"], None);
    assert_eq!(
        truncated.code,
        0,
        "search failed: {}",
        truncated.stderr_text()
    );
    let stdout = truncated.stdout_text();
    assert!(stdout.contains("(showing 2 matches with --limit 2;"));
    assert!(stdout.contains("use --limit <N>"));

    let complete = run_memo(&db_path, &["search", "needle", "--limit", "5"], None);
    assert_eq!(
        complete.code,
        0,
        "search failed: {}",
        complete.stderr_text()
    );
    assert!(!complete.stdout_text().contains("(showing"));
}

#[test]
fn search_json_reports_truncation_state() {
    let (_db_dir, db_path) = test_db("search_json_truncated");
    add_items(&db_path, 3, "target");

    let truncated = run_memo(
        &db_path,
        &["--json", "search", "target", "--limit", "2"],
        None,
    );
    assert_eq!(
        truncated.code,
        0,
        "search failed: {}",
        truncated.stderr_text()
    );
    let truncated_json = parse_json_stdout(&truncated);
    assert_eq!(truncated_json["data"]["meta"]["truncated"], true);

    let complete = run_memo(
        &db_path,
        &["--json", "search", "target", "--limit", "5"],
        None,
    );
    assert_eq!(
        complete.code,
        0,
        "search failed: {}",
        complete.stderr_text()
    );
    let complete_json = parse_json_stdout(&complete);
    assert_eq!(complete_json["data"]["meta"]["truncated"], false);
}
