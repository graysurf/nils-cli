use std::path::Path;

use api_testing_core::cli_util;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn write_file(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, contents).expect("write");
}

#[test]
fn bool_from_env_parses_and_warns_with_label() {
    let mut stderr: Vec<u8> = Vec::new();
    let stderr_writer: &mut dyn std::io::Write = &mut stderr;
    let got = cli_util::bool_from_env(
        Some("true".to_string()),
        "GQL_FOO",
        false,
        Some("api-gql"),
        stderr_writer,
    );
    assert_eq!(got, true);

    let mut stderr: Vec<u8> = Vec::new();
    let stderr_writer: &mut dyn std::io::Write = &mut stderr;
    let got = cli_util::bool_from_env(
        Some("nope".to_string()),
        "GQL_FOO",
        true,
        Some("api-gql"),
        stderr_writer,
    );
    assert_eq!(got, false);
    let msg = String::from_utf8_lossy(&stderr);
    assert!(msg.contains("api-gql: warning: GQL_FOO must be true|false"));
    assert!(msg.contains("nope"));
}

#[test]
fn bool_from_env_parses_and_warns_without_label() {
    let mut warnings: Vec<String> = Vec::new();
    let got = cli_util::bool_from_env(
        Some("nope".to_string()),
        "REST_FOO",
        true,
        None,
        &mut warnings,
    );
    assert_eq!(got, false);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("REST_FOO must be true|false"));
    assert!(!warnings[0].contains("warning:"));
}

#[test]
fn bool_from_env_uses_default_on_empty() {
    let mut stderr: Vec<u8> = Vec::new();
    let stderr_writer: &mut dyn std::io::Write = &mut stderr;
    let got = cli_util::bool_from_env(
        Some("".to_string()),
        "REST_FOO",
        true,
        Some("api-rest"),
        stderr_writer,
    );
    assert_eq!(got, true);
    assert!(stderr.is_empty());
}

#[test]
fn parse_u64_default_enforces_min() {
    assert_eq!(cli_util::parse_u64_default(Some("".to_string()), 10, 1), 10);
    assert_eq!(
        cli_util::parse_u64_default(Some("abc".to_string()), 10, 1),
        10
    );
    assert_eq!(cli_util::parse_u64_default(Some("0".to_string()), 10, 1), 1);
    assert_eq!(
        cli_util::parse_u64_default(Some("10".to_string()), 5, 1),
        10
    );
}

#[test]
fn to_env_key_and_slugify_normalize() {
    assert_eq!(cli_util::to_env_key("prod-us"), "PROD_US");
    assert_eq!(cli_util::to_env_key("  foo@@bar  "), "FOO_BAR");
    assert_eq!(cli_util::slugify("Hello, world!"), "hello-world");
    assert_eq!(cli_util::slugify("  ___ "), "");
}

#[test]
fn maybe_relpath_and_shell_quote() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    assert_eq!(cli_util::maybe_relpath(root, root), ".");

    let child = root.join("a/b");
    std::fs::create_dir_all(&child).unwrap();
    assert_eq!(cli_util::maybe_relpath(&child, root), "a/b");

    assert_eq!(cli_util::shell_quote(""), "''");
    assert_eq!(cli_util::shell_quote("a'b"), "'a'\\''b'");
}

#[test]
fn list_available_suffixes_parses_and_sorts() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("endpoints.env");
    write_file(
        &file,
        "export REST_URL_PROD=http://prod\nREST_URL_DEV=http://dev\nREST_URL_=bad\nREST_URL_FOO-BAR=http://x\nREST_URL_TEST=http://t\nREST_URL_TEST=http://t2\n",
    );

    let suffixes = cli_util::list_available_suffixes(&file, "REST_URL_");
    assert_eq!(suffixes, vec!["dev", "prod", "test"]);
}

#[test]
fn report_dates_are_formatted() {
    let stamp = cli_util::report_stamp_now().unwrap();
    assert_eq!(stamp.len(), 13, "stamp={stamp}");

    let date = cli_util::report_date_now().unwrap();
    assert_eq!(date.len(), 10, "date={date}");
    assert!(date.contains('-'));
}

#[test]
fn history_timestamp_is_not_empty() {
    let stamp = cli_util::history_timestamp_now().unwrap();
    assert!(!stamp.is_empty());
    assert!(stamp.contains('T'));
}
