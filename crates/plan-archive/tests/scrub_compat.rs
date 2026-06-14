//! Regression guard for the `plan_archive::scrub` backwards-compatibility
//! shim (#847 follow-up to the #846 `nils-scrub` extraction).
//!
//! The scrub implementation now lives in the shared `nils-scrub` crate, but
//! the pre-extraction `plan_archive::scrub::*` module path — including the
//! label-free `format_log(matches)` / `write_log_if_any(path, matches)` log
//! helper signatures that hard-code the `plan-archive` header — must keep
//! compiling for existing callers.

use std::fs;

use plan_archive::scrub;

#[test]
fn scrub_module_path_reexports_core_api() {
    assert!(!scrub::pattern_ids().is_empty());
    assert_eq!(scrub::PATTERN_SET, "v1");

    let result: scrub::ScrubResult =
        scrub::scrub_text("token: ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert!(result.redacted.contains(scrub::REDACTION_TOKEN));
    assert!(!result.redacted.contains("ghp_"));
    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].pattern_id, "github-token");
}

#[test]
fn label_free_log_helpers_keep_plan_archive_header() {
    let result = scrub::scrub_text("token: ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    // format_log keeps the original single-argument signature and emits the
    // historical "plan-archive" header label.
    let body = scrub::format_log(&result.matches);
    assert!(body.contains("# plan-archive scrub log"), "body:\n{body}");

    // write_log_if_any keeps the original (path, matches) signature.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snap.scrub.log");
    let wrote = scrub::write_log_if_any(&path, &result.matches).unwrap();
    assert!(wrote);

    let written = fs::read_to_string(&path).unwrap();
    assert!(written.contains("# plan-archive scrub log"));
    assert!(!written.contains("ghp_"));
}
