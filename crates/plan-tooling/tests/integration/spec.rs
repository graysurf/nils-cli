use crate::common;
use common::run_plan_tooling;

use pretty_assertions::assert_eq;

#[test]
fn spec_json_emits_class_pattern_rule_example_per_entry() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let out = run_plan_tooling(dir.path(), &["spec", "--format", "json"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    let entries = v.as_array().expect("top-level array");
    assert!(!entries.is_empty(), "spec must not be empty");
    for entry in entries {
        let obj = entry.as_object().expect("object entry");
        assert!(obj.contains_key("class"), "missing class field: {entry}");
        assert!(
            obj.contains_key("pattern"),
            "missing pattern field: {entry}"
        );
        assert!(obj.contains_key("rule"), "missing rule field: {entry}");
        assert!(
            obj.contains_key("example"),
            "missing example field: {entry}"
        );
    }
}

#[test]
fn spec_json_output_is_byte_stable() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let a = run_plan_tooling(dir.path(), &["spec", "--format", "json"]);
    let b = run_plan_tooling(dir.path(), &["spec", "--format", "json"]);
    assert_eq!(a.code, 0);
    assert_eq!(b.code, 0);
    assert_eq!(a.stdout, b.stdout, "spec json output must be byte-stable");

    let v: serde_json::Value = serde_json::from_str(&a.stdout).expect("json");
    let entries = v.as_array().expect("array");
    let classes: Vec<&str> = entries
        .iter()
        .map(|e| e["class"].as_str().unwrap_or(""))
        .collect();
    let mut sorted = classes.clone();
    sorted.sort();
    assert_eq!(classes, sorted, "entries must be sorted by class");
}

#[test]
fn spec_text_lists_each_catalog_entry_block() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let out = run_plan_tooling(dir.path(), &["spec"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    // Each entry block carries the four labels.
    assert!(out.stdout.contains("pattern:"), "got: {}", out.stdout);
    assert!(out.stdout.contains("rule:"), "got: {}", out.stdout);
    assert!(out.stdout.contains("example:"), "got: {}", out.stdout);
    // Bundle classes from F1 are present.
    assert!(
        out.stdout.contains("[bundle-primary-source-mismatch]"),
        "got: {}",
        out.stdout,
    );
}

#[test]
fn spec_version_prints_plan_tooling_version() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let out = run_plan_tooling(dir.path(), &["spec", "-V"]);
    assert_eq!(out.code, 0);
    assert!(
        out.stdout.starts_with("plan-tooling "),
        "stdout: {}",
        out.stdout,
    );
}

#[test]
fn spec_help_lists_version_flag() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let out = run_plan_tooling(dir.path(), &["spec", "--help"]);
    assert_eq!(out.code, 0);
    assert!(
        out.stderr.contains("-V, --version") || out.stderr.contains("--version"),
        "help should advertise --version, got: {}",
        out.stderr,
    );
}

#[test]
fn spec_invalid_format_is_usage_error() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let out = run_plan_tooling(dir.path(), &["spec", "--format", "yaml"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("invalid --format"));
}
