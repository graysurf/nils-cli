//! Integration coverage for the scrub library against on-disk
//! fixture payloads.

use std::fs;
use std::path::PathBuf;

use plan_archive::scrub;

fn fixture(name: &str) -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push("fixtures");
    path.push("scrub");
    path.push(name);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {path:?}: {err}"))
}

#[test]
fn all_patterns_fixture_redacts_every_expected_id() {
    let payload = fixture("all-patterns.txt");
    let result = scrub::scrub_text(&payload);
    let ids = result.triggered_patterns();
    assert_eq!(
        ids,
        vec![
            "aws-access-key-id".to_string(),
            "bitbucket-app-password".to_string(),
            "generic-secret-kv".to_string(),
            "github-token".to_string(),
            "gitlab-token".to_string(),
            "pem-private-key".to_string(),
        ]
    );
    // The fixture must not leak any of its secret bodies into the
    // redacted output.
    let forbidden = [
        "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "glpat-AAAAAAAAAAAAAAAAAAAA",
        "ATBB1234567890abcdefXYZ",
        "AKIAIOSFODNN7EXAMPLE",
        "abcd1234efgh5678",
        "MIIE-not-a-real-key-body",
    ];
    for needle in &forbidden {
        assert!(
            !result.redacted.contains(needle),
            "leaked `{needle}` in:\n{}",
            result.redacted
        );
    }
}

#[test]
fn clean_fixture_produces_zero_matches() {
    let payload = fixture("clean.txt");
    let result = scrub::scrub_text(&payload);
    assert!(result.matches.is_empty(), "matches: {:?}", result.matches);
    assert_eq!(result.redacted, payload);
}

#[test]
fn log_format_is_stable_for_fixture() {
    let payload = fixture("all-patterns.txt");
    let result = scrub::scrub_text(&payload);
    let body = scrub::format_log(&result.matches);
    // The header pair is fixed and the summary line carries the
    // sorted distinct pattern set.
    assert!(body.starts_with("# plan-archive scrub log\n"));
    assert!(body.contains("# pattern_set: v1\n"));
    assert!(body.lines().last().unwrap().starts_with(
        "summary patterns_triggered=aws-access-key-id,bitbucket-app-password,generic-secret-kv,github-token,gitlab-token,pem-private-key"
    ));
}

#[test]
fn write_log_round_trips_through_tempfile() {
    let payload = fixture("all-patterns.txt");
    let result = scrub::scrub_text(&payload);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("20260527T012345Z.scrub.log");
    let wrote = scrub::write_log_if_any(&path, &result.matches).unwrap();
    assert!(wrote);
    let body = fs::read_to_string(&path).unwrap();
    assert_eq!(body, scrub::format_log(&result.matches));
}
