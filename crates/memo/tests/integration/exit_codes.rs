use nils_common::cli_contract::exit;
use pretty_assertions::assert_eq;

use crate::support;
use support::{run_memo, test_db};

#[test]
fn exit_code_success() {
    let (_db_dir, db_path) = test_db("exit_code_success");
    let output = run_memo(&db_path, &["add", "exit-code-success"], None);
    assert_eq!(
        output.code,
        exit::SUCCESS,
        "add should exit 0 on success: stderr={}",
        output.stderr_text()
    );
}

#[test]
fn exit_code_usage_for_invalid_arg() {
    let (_db_dir, db_path) = test_db("exit_code_usage_invalid_arg");
    // `--limit 0` is rejected by the runtime usage guard.
    let output = run_memo(&db_path, &["fetch", "--limit", "0"], None);
    assert_eq!(
        output.code,
        exit::USAGE,
        "fetch --limit 0 should return USAGE (64); stderr={}",
        output.stderr_text()
    );
}

#[test]
fn exit_code_usage_for_unknown_subcommand() {
    let (_db_dir, db_path) = test_db("exit_code_usage_unknown_subcmd");
    let output = run_memo(&db_path, &["bogus-subcommand"], None);
    assert_eq!(
        output.code,
        exit::USAGE,
        "bogus-subcommand should return USAGE (64); stderr={}",
        output.stderr_text()
    );
}

#[test]
fn exit_code_data_for_malformed_apply_payload() {
    let (_db_dir, db_path) = test_db("exit_code_data_malformed_apply");
    let output = run_memo(&db_path, &["apply", "--stdin"], Some("{}"));
    assert_eq!(
        output.code,
        exit::DATA,
        "apply with empty payload should return DATA (65); stderr={}",
        output.stderr_text()
    );
}

#[test]
fn exit_code_runtime_for_unopenable_db_path() {
    // `/dev/null/x` cannot be opened as a sqlite file — the parent is not a
    // directory — which forces a `db-open-failed` runtime error.
    let bogus_db = std::path::PathBuf::from("/dev/null/x");
    let output = run_memo(&bogus_db, &["add", "exit-code-runtime"], None);
    assert_eq!(
        output.code,
        exit::RUNTIME,
        "unopenable --db should return RUNTIME (1); stderr={}",
        output.stderr_text()
    );
}
