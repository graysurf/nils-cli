//! Cross-provider conformance harness (local-provider rollout Task 3.1).
//!
//! Goal: prove the `Provider::Local` fake produces the *same observable
//! envelope* as the real GitHub / GitLab backends for the issue / timeline
//! half, so anything exercised against `local` is a trustworthy proxy for the
//! real providers. The local backend stays a **complement** to real-provider
//! e2e, never a replacement.
//!
//! Design: every arm runs the REAL `forge-cli` binary end to end. The `local`
//! arm uses the real file-backed [`crate::local::LocalRunner`] against a
//! hermetic temp store (genuinely stateful — create then read). The `github`
//! and `gitlab` arms use `gh` / `glab` stub scripts (wired via
//! `FORGE_CLI_GH_BIN` / `FORGE_CLI_GLAB_BIN`) that echo the canned JSON the
//! real provider would return for the equivalent state. Each arm therefore
//! drives its full pipeline (`build_*_call` → runner → `parse_*_output` →
//! envelope); the harness then strips the fields that differ *by design*
//! (`provider`, `url`, comment author/url/timestamps) and asserts the
//! remaining observable is byte-identical across all three.
//!
//! Half A (issue/timeline) is conformance-tested for **behaviour**; Half B
//! (PR/CI, locally seeded) for **shape**. The scenario subset is documented in
//! `crates/plan-issue/docs/specs/local-provider-contract-v1.md`
//! §"Conformance Scenario Subset".

use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::support::{StubEnv, parse_envelope, run_forge_cli};

// ----- existing per-op fixtures reused for the Half B shape arms -------------
const GH_CHECKS_ALL_SUCCESS: &str = include_str!("../fixtures/github/pr_checks/all_success.json");
const GLAB_VERSION_OK: &str = include_str!("../fixtures/gitlab/pr_checks/version_supported.txt");
const GLAB_CHECKS_ALL_SUCCESS: &str = include_str!("../fixtures/gitlab/pr_checks/all_success.txt");

// ----- run helpers: one per provider, all through the real binary -----------

/// Run a `--provider local` op against `store` (the scenario's temp store
/// root). The store persists across calls so multi-step scenarios (create →
/// comment → view) behave as the real stateful backend.
fn local_call(stub: &StubEnv, store: &Path, args: &[&str]) -> Value {
    let root = store.to_string_lossy().into_owned();
    let mut full: Vec<String> = vec![
        "--provider".into(),
        "local".into(),
        "--store-root".into(),
        root,
        "--repo".into(),
        "local:demo".into(),
        "--format".into(),
        "json".into(),
    ];
    full.extend(args.iter().map(|s| s.to_string()));
    let refs: Vec<&str> = full.iter().map(String::as_str).collect();
    parse_envelope(&run_forge_cli(stub, &refs).stdout)
}

/// Run a `--provider github` op against a `gh` stub with the given script body.
fn github_call(stub_body: &str, args: &[&str]) -> Value {
    let stub = StubEnv::new().gh_stub(stub_body);
    let mut argv: Vec<&str> = vec!["--provider", "github", "--format", "json"];
    argv.extend_from_slice(args);
    parse_envelope(&run_forge_cli(&stub, &argv).stdout)
}

/// Run a `--provider gitlab` op against a `glab` stub with the given script.
fn gitlab_call(stub_body: &str, args: &[&str]) -> Value {
    let stub = StubEnv::new().glab_stub(stub_body);
    let mut argv: Vec<&str> = vec!["--provider", "gitlab", "--format", "json"];
    argv.extend_from_slice(args);
    parse_envelope(&run_forge_cli(&stub, &argv).stdout)
}

// ----- observable projections (strip by-design provider differences) ---------

/// The conformance-relevant observable of an `issue view` envelope: everything
/// except `provider` and `url` (both differ by design), plus the comment
/// timeline reduced to ordered bodies (comment url/author/timestamp are
/// provider-specific and excluded).
fn issue_observable(env: &Value) -> Value {
    let d = &env["data"];
    let comment_bodies: Vec<Value> = d["comments"]
        .as_array()
        .map(|a| a.iter().map(|c| c["body"].clone()).collect())
        .unwrap_or_default();
    json!({
        "number": d["number"],
        "state": d["state"],
        "title": d["title"],
        "body": d["body"],
        "labels": d["labels"],
        "assignees": d["assignees"],
        "comment_bodies": comment_bodies,
    })
}

/// The conformance-relevant observable of an `issue list` envelope: each row
/// reduced to `{number, state, title, labels}` (url + author are
/// provider-specific and excluded).
fn list_observable(env: &Value) -> Value {
    let rows: Vec<Value> = env["data"]["items"]
        .as_array()
        .expect("items array")
        .iter()
        .map(|i| {
            json!({
                "number": i["number"],
                "state": i["state"],
                "title": i["title"],
                "labels": i["labels"],
            })
        })
        .collect();
    Value::Array(rows)
}

/// Sorted top-level `data` keys — the structural shape used for Half B.
fn data_keys(env: &Value) -> Vec<String> {
    let mut k: Vec<String> = env["data"]
        .as_object()
        .expect("data object")
        .keys()
        .cloned()
        .collect();
    k.sort();
    k
}

// ----- stub builders ---------------------------------------------------------

/// `gh` stub that answers a single `issue view` with `view_json`.
fn gh_issue_view_stub(view_json: &str) -> String {
    format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "issue view")
    cat <<'EOF'
{view_json}
EOF
    ;;
  "issue close"|"issue comment"|"issue edit"|"issue create")
    : ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99 ;;
esac
"#
    )
}

/// `glab` stub that answers `issue view` (web_url-bearing JSON) and, for
/// `--with-comments`, the follow-up `glab api .../notes` call.
fn glab_issue_stub(view_json: &str, notes_json: &str) -> String {
    format!(
        r#"#!/bin/sh
set -e
case "$1" in
  "issue")
    cat <<'EOF'
{view_json}
EOF
    ;;
  "api")
    cat <<'EOF'
{notes_json}
EOF
    ;;
  *)
    echo "stub: unexpected glab args: $*" >&2
    exit 99 ;;
esac
"#
    )
}

/// `gh` stub for `issue list`: returns `filtered_json` when the joined
/// `--label` arg contains the AND-pair `plan,p1`, else `all_json`. Faithful to
/// gh's server-side label filtering.
fn gh_issue_list_stub(all_json: &str, filtered_json: &str) -> String {
    format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "issue list")
    case " $* " in
      *"plan,p1"*)
        cat <<'EOF'
{filtered_json}
EOF
        ;;
      *)
        cat <<'EOF'
{all_json}
EOF
        ;;
    esac ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99 ;;
esac
"#
    )
}

/// `glab` stub for `issue list`: returns `filtered_json` when the repeated
/// `--label` args include `p1`, else `all_json`.
fn glab_issue_list_stub(all_json: &str, filtered_json: &str) -> String {
    format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "issue list")
    case " $* " in
      *" p1 "*)
        cat <<'EOF'
{filtered_json}
EOF
        ;;
      *)
        cat <<'EOF'
{all_json}
EOF
        ;;
    esac ;;
  *)
    echo "stub: unexpected glab args: $*" >&2
    exit 99 ;;
esac
"#
    )
}

/// `gh` stub for `pr view`.
fn gh_pr_view_stub(view_json: &str) -> String {
    format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "pr view")
    cat <<'EOF'
{view_json}
EOF
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99 ;;
esac
"#
    )
}

/// `glab` stub for `mr view`.
fn glab_mr_view_stub(view_json: &str) -> String {
    format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "mr view")
    cat <<'EOF'
{view_json}
EOF
    ;;
  *)
    echo "stub: unexpected glab args: $*" >&2
    exit 99 ;;
esac
"#
    )
}

/// `gh` stub for `pr checks`: both the all-checks and `--required` calls return
/// `checks_json` (so the required subset == all == success here).
fn gh_pr_checks_stub(checks_json: &str) -> String {
    format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "pr checks")
    cat <<'EOF'
{checks_json}
EOF
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99 ;;
esac
"#
    )
}

/// `glab` stub for `pr checks`: version probe, numeric-id branch resolution via
/// `mr view`, and `ci status` text.
fn glab_pr_checks_stub(version: &str, ci_status: &str) -> String {
    format!(
        r#"#!/bin/sh
set -e
case "$1" in
  "--version")
    cat <<'EOF'
{version}
EOF
    ;;
  "ci")
    if [ "$2" = "status" ]; then
      cat <<'EOF'
{ci_status}
EOF
    else
      echo "stub: unknown ci subcommand: $*" >&2
      exit 99
    fi ;;
  "mr")
    if [ "$2" = "view" ]; then
      printf '%s\n' '{{"iid":7,"source_branch":"feat/sample"}}'
    else
      echo "stub: unknown mr subcommand: $*" >&2
      exit 99
    fi ;;
  *)
    echo "stub: unexpected glab args: $*" >&2
    exit 99 ;;
esac
"#
    )
}

/// Seed a `prs/<n>.json` record into a store root for the Half B read arms.
fn seed_pr(store: &Path, number: u64, body: &str) {
    let dir = store.join("prs");
    std::fs::create_dir_all(&dir).expect("mkdir prs");
    std::fs::write(dir.join(format!("{number}.json")), body).expect("write pr record");
}

// ============================================================================
// Half A — issue / timeline: behavioural conformance across all three.
// ============================================================================

#[test]
fn issue_view_open_conforms_across_providers() {
    let expected = json!({
        "number": 1,
        "state": "open",
        "title": "Plan: conformance",
        "body": "scenario body",
        "labels": ["plan", "p1"],
        "assignees": [],
        "comment_bodies": [],
    });

    let stub = StubEnv::new();
    let store = stub.tempdir.path();
    local_call(
        &stub,
        store,
        &[
            "issue",
            "create",
            "--title",
            "Plan: conformance",
            "--body",
            "scenario body",
            "--label",
            "plan",
            "--label",
            "p1",
        ],
    );
    let local = local_call(&stub, store, &["issue", "view", "1"]);

    let github = github_call(
        &gh_issue_view_stub(
            r#"{"number":1,"url":"https://github.com/acme/widgets/issues/1","state":"OPEN","title":"Plan: conformance","body":"scenario body","labels":[{"name":"plan"},{"name":"p1"}],"assignees":[]}"#,
        ),
        &["issue", "view", "1"],
    );

    let gitlab = gitlab_call(
        &glab_issue_stub(
            r#"{"iid":1,"web_url":"https://gitlab.com/acme/widgets/-/issues/1","state":"opened","title":"Plan: conformance","description":"scenario body","labels":["plan","p1"],"assignees":[]}"#,
            "[]",
        ),
        &["issue", "view", "1"],
    );

    assert_eq!(issue_observable(&local), expected, "local issue view");
    assert_eq!(issue_observable(&github), expected, "github issue view");
    assert_eq!(issue_observable(&gitlab), expected, "gitlab issue view");
}

#[test]
fn issue_view_closed_conforms_across_providers() {
    let expected = json!({
        "number": 1,
        "state": "closed",
        "title": "Plan: conformance",
        "body": "scenario body",
        "labels": ["plan"],
        "assignees": [],
        "comment_bodies": [],
    });

    let stub = StubEnv::new();
    let store = stub.tempdir.path();
    local_call(
        &stub,
        store,
        &[
            "issue",
            "create",
            "--title",
            "Plan: conformance",
            "--body",
            "scenario body",
            "--label",
            "plan",
        ],
    );
    local_call(&stub, store, &["issue", "close", "1"]);
    let local = local_call(&stub, store, &["issue", "view", "1"]);

    let github = github_call(
        &gh_issue_view_stub(
            r#"{"number":1,"url":"https://github.com/acme/widgets/issues/1","state":"CLOSED","title":"Plan: conformance","body":"scenario body","labels":[{"name":"plan"}],"assignees":[]}"#,
        ),
        &["issue", "view", "1"],
    );

    let gitlab = gitlab_call(
        &glab_issue_stub(
            r#"{"iid":1,"web_url":"https://gitlab.com/acme/widgets/-/issues/1","state":"closed","title":"Plan: conformance","description":"scenario body","labels":["plan"],"assignees":[]}"#,
            "[]",
        ),
        &["issue", "view", "1"],
    );

    assert_eq!(issue_observable(&local), expected, "local closed");
    assert_eq!(issue_observable(&github), expected, "github closed");
    assert_eq!(issue_observable(&gitlab), expected, "gitlab closed");
}

#[test]
fn issue_view_with_comments_conforms_across_providers() {
    let expected = json!({
        "number": 1,
        "state": "open",
        "title": "Plan: conformance",
        "body": "scenario body",
        "labels": [],
        "assignees": [],
        "comment_bodies": ["first note", "second note"],
    });

    let stub = StubEnv::new();
    let store = stub.tempdir.path();
    local_call(
        &stub,
        store,
        &[
            "issue",
            "create",
            "--title",
            "Plan: conformance",
            "--body",
            "scenario body",
        ],
    );
    local_call(
        &stub,
        store,
        &["issue", "comment", "1", "--body", "first note"],
    );
    local_call(
        &stub,
        store,
        &["issue", "comment", "1", "--body", "second note"],
    );
    let local = local_call(&stub, store, &["issue", "view", "1", "--with-comments"]);

    let github = github_call(
        &gh_issue_view_stub(
            r#"{"number":1,"url":"https://github.com/acme/widgets/issues/1","state":"OPEN","title":"Plan: conformance","body":"scenario body","labels":[],"assignees":[],"comments":[{"author":{"login":"local"},"body":"first note","url":"https://github.com/acme/widgets/issues/1#issuecomment-1","createdAt":"2026-05-31T00:00:00Z"},{"author":{"login":"local"},"body":"second note","url":"https://github.com/acme/widgets/issues/1#issuecomment-2","createdAt":"2026-05-31T00:01:00Z"}]}"#,
        ),
        &["issue", "view", "1", "--with-comments"],
    );

    let gitlab = gitlab_call(
        &glab_issue_stub(
            r#"{"iid":1,"web_url":"https://gitlab.com/acme/widgets/-/issues/1","state":"opened","title":"Plan: conformance","description":"scenario body","labels":[],"assignees":[]}"#,
            r#"[{"id":1,"body":"first note","author":{"username":"local"},"created_at":"2026-05-31T00:00:00Z","system":false},{"id":2,"body":"second note","author":{"username":"local"},"created_at":"2026-05-31T00:01:00Z","system":false}]"#,
        ),
        &["issue", "view", "1", "--with-comments"],
    );

    assert_eq!(issue_observable(&local), expected, "local with-comments");
    assert_eq!(issue_observable(&github), expected, "github with-comments");
    assert_eq!(issue_observable(&gitlab), expected, "gitlab with-comments");
}

#[test]
fn issue_list_label_filter_conforms_across_providers() {
    let expected_all = json!([
        {"number": 1, "state": "open", "title": "Issue one", "labels": ["plan", "p1"]},
        {"number": 2, "state": "open", "title": "Issue two", "labels": ["plan"]},
    ]);
    let expected_filtered = json!([
        {"number": 1, "state": "open", "title": "Issue one", "labels": ["plan", "p1"]},
    ]);

    // local: two open issues, the first carrying the AND-pair.
    let stub = StubEnv::new();
    let store = stub.tempdir.path();
    local_call(
        &stub,
        store,
        &[
            "issue",
            "create",
            "--title",
            "Issue one",
            "--body",
            "b",
            "--label",
            "plan",
            "--label",
            "p1",
        ],
    );
    local_call(
        &stub,
        store,
        &[
            "issue",
            "create",
            "--title",
            "Issue two",
            "--body",
            "b",
            "--label",
            "plan",
        ],
    );
    let local_all = local_call(&stub, store, &["issue", "list", "--label", "plan"]);
    let local_filtered = local_call(
        &stub,
        store,
        &["issue", "list", "--label", "plan", "--label", "p1"],
    );

    let gh_all = r#"[{"number":1,"url":"https://github.com/acme/widgets/issues/1","state":"OPEN","title":"Issue one","labels":[{"name":"plan"},{"name":"p1"}],"author":{"login":"local"},"assignees":[]},{"number":2,"url":"https://github.com/acme/widgets/issues/2","state":"OPEN","title":"Issue two","labels":[{"name":"plan"}],"author":{"login":"local"},"assignees":[]}]"#;
    let gh_filtered = r#"[{"number":1,"url":"https://github.com/acme/widgets/issues/1","state":"OPEN","title":"Issue one","labels":[{"name":"plan"},{"name":"p1"}],"author":{"login":"local"},"assignees":[]}]"#;
    let gh_stub = gh_issue_list_stub(gh_all, gh_filtered);
    let github_all = github_call(&gh_stub, &["issue", "list", "--label", "plan"]);
    let github_filtered = github_call(
        &gh_stub,
        &["issue", "list", "--label", "plan", "--label", "p1"],
    );

    let glab_all = r#"[{"iid":1,"web_url":"https://gitlab.com/acme/widgets/-/issues/1","state":"opened","title":"Issue one","labels":["plan","p1"],"author":{"username":"local"},"assignees":[]},{"iid":2,"web_url":"https://gitlab.com/acme/widgets/-/issues/2","state":"opened","title":"Issue two","labels":["plan"],"author":{"username":"local"},"assignees":[]}]"#;
    let glab_filtered = r#"[{"iid":1,"web_url":"https://gitlab.com/acme/widgets/-/issues/1","state":"opened","title":"Issue one","labels":["plan","p1"],"author":{"username":"local"},"assignees":[]}]"#;
    let glab_stub = glab_issue_list_stub(glab_all, glab_filtered);
    let gitlab_all = gitlab_call(&glab_stub, &["issue", "list", "--label", "plan"]);
    let gitlab_filtered = gitlab_call(
        &glab_stub,
        &["issue", "list", "--label", "plan", "--label", "p1"],
    );

    assert_eq!(list_observable(&local_all), expected_all, "local list plan");
    assert_eq!(
        list_observable(&github_all),
        expected_all,
        "github list plan"
    );
    assert_eq!(
        list_observable(&gitlab_all),
        expected_all,
        "gitlab list plan"
    );

    assert_eq!(
        list_observable(&local_filtered),
        expected_filtered,
        "local list plan,p1"
    );
    assert_eq!(
        list_observable(&github_filtered),
        expected_filtered,
        "github list plan,p1"
    );
    assert_eq!(
        list_observable(&gitlab_filtered),
        expected_filtered,
        "gitlab list plan,p1"
    );
}

#[test]
fn issue_list_empty_conforms_across_providers() {
    let empty = json!([]);

    let stub = StubEnv::new();
    let store = stub.tempdir.path();
    // Store has no issues — listing any label yields an empty set.
    let local = local_call(&stub, store, &["issue", "list", "--label", "zznope"]);
    let github = github_call(
        &gh_issue_list_stub("[]", "[]"),
        &["issue", "list", "--label", "zznope"],
    );
    let gitlab = gitlab_call(
        &glab_issue_list_stub("[]", "[]"),
        &["issue", "list", "--label", "zznope"],
    );

    assert_eq!(list_observable(&local), empty, "local empty list");
    assert_eq!(list_observable(&github), empty, "github empty list");
    assert_eq!(list_observable(&gitlab), empty, "gitlab empty list");
}

// ============================================================================
// Half B — PR / CI: shape conformance from equivalent seeded / canned state.
// ============================================================================

#[test]
fn pr_view_seeded_merged_conforms_for_shape() {
    let stub = StubEnv::new();
    let store = stub.tempdir.path();
    seed_pr(
        store,
        7,
        r#"{"number":7,"state":"MERGED","merged":true,"merge_sha":"deadbeef","checks":"success","required_state":"success","required_count":2,"non_required_failures":[]}"#,
    );
    let local = local_call(&stub, store, &["pr", "view", "7"]);

    let github = github_call(
        &gh_pr_view_stub(
            r#"{"number":7,"url":"https://github.com/acme/widgets/pull/7","state":"CLOSED","isDraft":false,"title":"Add conformance","headRefName":"feat/x","baseRefName":"main","mergeable":"UNKNOWN","mergedAt":"2026-05-31T00:00:00Z","mergeCommit":{"oid":"deadbeef"},"labels":[]}"#,
        ),
        &["pr", "view", "7"],
    );

    let gitlab = gitlab_call(
        &glab_mr_view_stub(
            r#"{"iid":7,"web_url":"https://gitlab.com/acme/widgets/-/merge_requests/7","state":"merged","draft":false,"title":"Add conformance","source_branch":"feat/x","target_branch":"main","merge_status":"can_be_merged","merged_at":"2026-05-31T00:00:00Z","merge_commit_sha":"deadbeef","labels":[]}"#,
        ),
        &["pr", "view", "7"],
    );

    for (name, env) in [("local", &local), ("github", &github), ("gitlab", &gitlab)] {
        assert_eq!(env["ok"], true, "{name} pr view ok: {env}");
        assert_eq!(
            env["schema_version"], "cli.forge-cli.pr.view.v1",
            "{name} pr view schema"
        );
        // Seeded-equivalent state — these MUST match across providers.
        assert_eq!(env["data"]["state"], "merged", "{name} pr view state");
        assert_eq!(env["data"]["number"], 7, "{name} pr view number");
        assert_eq!(
            env["data"]["merge_commit_sha"], "deadbeef",
            "{name} pr view merge_commit_sha"
        );
    }
    // Shape: every provider emits the identical top-level `data` field set.
    assert_eq!(
        data_keys(&local),
        data_keys(&github),
        "local vs github keys"
    );
    assert_eq!(
        data_keys(&github),
        data_keys(&gitlab),
        "github vs gitlab keys"
    );
}

#[test]
fn pr_checks_seeded_success_conforms_for_shape() {
    let stub = StubEnv::new();
    let store = stub.tempdir.path();
    seed_pr(
        store,
        7,
        r#"{"number":7,"state":"OPEN","merged":false,"merge_sha":null,"checks":"success","required_state":"success","required_count":2,"non_required_failures":[]}"#,
    );
    let local = local_call(&stub, store, &["pr", "checks", "7"]);

    let github = github_call(
        &gh_pr_checks_stub(GH_CHECKS_ALL_SUCCESS),
        &["pr", "checks", "7"],
    );
    let gitlab = gitlab_call(
        &glab_pr_checks_stub(GLAB_VERSION_OK, GLAB_CHECKS_ALL_SUCCESS),
        &["pr", "checks", "7"],
    );

    for (name, env) in [("local", &local), ("github", &github), ("gitlab", &gitlab)] {
        assert_eq!(env["ok"], true, "{name} pr checks ok: {env}");
        assert_eq!(
            env["schema_version"], "cli.forge-cli.pr.checks.v1",
            "{name} pr checks schema"
        );
        assert_eq!(env["data"]["state"], "success", "{name} pr checks state");
    }
    assert_eq!(
        data_keys(&local),
        data_keys(&github),
        "local vs github keys"
    );
    assert_eq!(
        data_keys(&github),
        data_keys(&gitlab),
        "github vs gitlab keys"
    );
}

// ============================================================================
// Negative control — the harness must catch a real divergence.
// ============================================================================

#[test]
fn conformance_harness_detects_a_divergence() {
    // Same op, but the github arm reports a different title than local. The
    // observable projection must differ, proving the equality assertions above
    // would fail on a genuine local-vs-real drift (not pass vacuously).
    let stub = StubEnv::new();
    let store = stub.tempdir.path();
    local_call(
        &stub,
        store,
        &["issue", "create", "--title", "real title", "--body", "b"],
    );
    let local = local_call(&stub, store, &["issue", "view", "1"]);

    let github = github_call(
        &gh_issue_view_stub(
            r#"{"number":1,"url":"https://github.com/acme/widgets/issues/1","state":"OPEN","title":"DRIFTED title","body":"b","labels":[],"assignees":[]}"#,
        ),
        &["issue", "view", "1"],
    );

    assert_ne!(
        issue_observable(&local),
        issue_observable(&github),
        "a title drift between local and github MUST surface as a divergent observable"
    );
}
