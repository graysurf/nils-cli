//! Integration coverage for `--provider local` (the file-backed backend).
//!
//! Exercises the real binary end to end against a hermetic temp store: the
//! REAL issue half (create / view / comment / list / close), the seeded PR
//! read half (view / checks / comments), and the dispatcher guard that rejects
//! unsupported operations. On-disk contract:
//! `crates/plan-issue-cli/docs/specs/local-provider-contract-v1.md`.

use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::Value;

use super::support::{StubEnv, forge_cli_bin, parse_envelope};

/// Run `forge-cli --provider local --store-root <store> --repo local:demo
/// --format json <args>` and parse the JSON envelope.
fn local(store: &Path, args: &[&str]) -> Value {
    let mut full: Vec<String> = vec![
        "--provider".into(),
        "local".into(),
        "--store-root".into(),
        store.to_string_lossy().into_owned(),
        "--repo".into(),
        "local:demo".into(),
        "--format".into(),
        "json".into(),
    ];
    full.extend(args.iter().map(|s| s.to_string()));
    let arg_refs: Vec<&str> = full.iter().map(String::as_str).collect();
    let output = std::process::Command::new(forge_cli_bin())
        .args(&arg_refs)
        .output()
        .expect("spawn forge-cli");
    parse_envelope(&String::from_utf8_lossy(&output.stdout))
}

#[test]
fn issue_lifecycle_is_real_against_the_store() {
    let stub = StubEnv::new();
    let store = stub.tempdir.path();

    // create -> issue #1, open, synthetic local:// url.
    let created = local(
        store,
        &[
            "issue", "create", "--title", "Plan: x", "--body", "hello", "--label", "plan",
        ],
    );
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(created["data"]["number"], 1);
    assert_eq!(created["data"]["state"], "open");
    assert_eq!(created["data"]["url"], "local://demo/issues/1");

    // comment -> appended; view --with-comments reflects it with a
    // deterministic timestamp.
    let commented = local(store, &["issue", "comment", "1", "--body", "a note"]);
    assert_eq!(commented["ok"], true, "{commented}");

    let viewed = local(store, &["issue", "view", "1", "--with-comments"]);
    assert_eq!(viewed["data"]["body"], "hello");
    assert_eq!(viewed["data"]["labels"][0], "plan");
    assert_eq!(viewed["data"]["comments"][0]["body"], "a note");
    assert_eq!(
        viewed["data"]["comments"][0]["created_at"],
        "2026-01-01T00:00:00Z"
    );

    // edit -> retitle + add a label.
    local(
        store,
        &[
            "issue",
            "edit",
            "1",
            "--title",
            "Plan: y",
            "--add-label",
            "p1",
        ],
    );
    let edited = local(store, &["issue", "view", "1"]);
    assert_eq!(edited["data"]["title"], "Plan: y");

    // list (open) with AND label semantics returns issue 1.
    let listed = local(store, &["issue", "list", "--label", "plan,p1"]);
    let numbers: Vec<u64> = listed["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["number"].as_u64().unwrap())
        .collect();
    assert_eq!(numbers, vec![1]);

    // close -> state flips; open list no longer matches.
    local(store, &["issue", "close", "1"]);
    let closed = local(store, &["issue", "view", "1"]);
    assert_eq!(closed["data"]["state"], "closed");
    let open_after = local(store, &["issue", "list", "--label", "plan"]);
    assert!(open_after["data"]["items"].as_array().unwrap().is_empty());
}

#[test]
fn pr_read_half_serves_seeded_records() {
    let stub = StubEnv::new();
    let store = stub.tempdir.path();
    std::fs::create_dir_all(store.join("prs")).unwrap();
    std::fs::write(
        store.join("prs").join("7.json"),
        r#"{"number":7,"state":"MERGED","merged":true,"merge_sha":"abc1234",
            "checks":"success","required_state":"success","required_count":2,
            "non_required_failures":[],
            "comments":[{"body":"lgtm","html_url":"local://demo/pull/7#comment-1","author":"rev","created_at":"2026-01-01T00:00:05Z"}]}"#,
    )
    .unwrap();

    let view = local(store, &["pr", "view", "7"]);
    assert_eq!(view["data"]["state"], "merged");
    assert_eq!(view["data"]["merge_commit_sha"], "abc1234");
    assert_eq!(view["data"]["url"], "local://demo/pull/7");

    let checks = local(store, &["pr", "checks", "7"]);
    assert_eq!(checks["data"]["state"], "success");
    assert_eq!(checks["data"]["required_count"], 2);

    let comments = local(store, &["pr", "comments", "7"]);
    assert_eq!(comments["ok"], true, "{comments}");
    assert_eq!(comments["data"]["comments"][0]["body"], "lgtm");
    assert_eq!(comments["data"]["comments"][0]["author"], "rev");
}

#[test]
fn pr_checks_reflects_a_seeded_required_failure() {
    let stub = StubEnv::new();
    let store = stub.tempdir.path();
    std::fs::create_dir_all(store.join("prs")).unwrap();
    std::fs::write(
        store.join("prs").join("9.json"),
        r#"{"number":9,"state":"OPEN","merged":false,"merge_sha":null,
            "checks":"failure","required_state":"failure","required_count":1,"non_required_failures":[]}"#,
    )
    .unwrap();
    let checks = local(store, &["pr", "checks", "9"]);
    assert_eq!(checks["data"]["state"], "failure");
    assert_eq!(checks["data"]["required_count"], 1);
}

#[test]
fn unsupported_operations_are_rejected_for_local() {
    let stub = StubEnv::new();
    let store = stub.tempdir.path();
    // pr merge is not modeled by the local backend.
    let merged = local(store, &["pr", "merge", "7"]);
    assert_eq!(merged["ok"], false, "{merged}");
    assert_eq!(merged["error"]["code"], "provider_unsupported");
    // issue reopen likewise.
    let reopened = local(store, &["issue", "reopen", "1"]);
    assert_eq!(reopened["ok"], false, "{reopened}");
    assert_eq!(reopened["error"]["code"], "provider_unsupported");
}
