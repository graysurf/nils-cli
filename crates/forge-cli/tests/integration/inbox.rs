//! `forge-cli inbox` integration tests. Provider calls are fully stubbed so
//! cross-repo aggregation, partial success, and host propagation are tested
//! without live GitHub/GitLab access.

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

const GH_INBOX_STUB: &str = r#"#!/bin/sh
set -e
case "$1 $2 $3" in
  "search prs --review-requested")
    cat <<'EOF'
[{"number":7,"url":"https://github.com/acme/widgets/pull/7","title":"Review me","updatedAt":"2026-05-22T10:00:00Z","author":{"login":"alice"},"repository":{"nameWithOwner":"acme/widgets"}}]
EOF
    ;;
  "search prs --assignee")
    cat <<'EOF'
[{"number":7,"url":"https://github.com/acme/widgets/pull/7","title":"Review me","updatedAt":"2026-05-22T10:30:00Z","author":{"login":"alice"},"repository":{"nameWithOwner":"acme/widgets"}}]
EOF
    ;;
  "search issues --assignee")
    cat <<'EOF'
[{"number":8,"url":"https://github.com/acme/widgets/issues/8","title":"Assigned issue","updatedAt":"2026-05-22T09:00:00Z","author":{"login":"bob"},"repository":{"nameWithOwner":"acme/widgets"}}]
EOF
    ;;
  "search prs --author")
    cat <<'EOF'
[{"number":9,"url":"https://github.com/acme/widgets/pull/9","title":"Authored PR","updatedAt":"2026-05-21T09:00:00Z","author":{"login":"me"},"repository":{"nameWithOwner":"acme/widgets"}}]
EOF
    ;;
  "search issues --author")
    echo '[]'
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#;

const GH_EMPTY_STUB: &str = r#"#!/bin/sh
set -e
case "$1 $2" in
  "search prs"|"search issues")
    echo '[]'
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#;

const GH_FAIL_STUB: &str = r#"#!/bin/sh
echo "gh unavailable" >&2
exit 7
"#;

const GLAB_INBOX_STUB: &str = r#"#!/bin/sh
set -e
case "$*" in
  *"--hostname gitlab.example.com"*|*"--hostname gitlab.env.example"*) ;;
  *)
    echo "missing hostname: $*" >&2
    exit 98
    ;;
esac

case "$*" in
  "api user --hostname gitlab.example.com"|"api user --hostname gitlab.env.example")
    cat <<'EOF'
{"id":1435,"username":"terrylin"}
EOF
    ;;
  *"merge_requests"*"scope=assigned_to_me"*)
    cat <<'EOF'
[{"iid":21,"web_url":"https://gitlab.example.com/team/api/-/merge_requests/21","title":"Assigned MR","updated_at":"2026-05-22T08:00:00Z","author":{"username":"carol"},"references":{"full":"team/api!21"}}]
EOF
    ;;
  *"merge_requests"*"reviewer_username=terrylin"*)
    cat <<'EOF'
[{"iid":22,"web_url":"https://gitlab.example.com/team/api/-/merge_requests/22","title":"Review MR","updated_at":"2026-05-22T11:00:00Z","author":{"username":"dave"},"references":{"full":"team/api!22"}}]
EOF
    ;;
  *"merge_requests"*"author_id=1435"*)
    echo '[]'
    ;;
  *"issues"*"scope=assigned_to_me"*)
    cat <<'EOF'
[{"iid":31,"web_url":"https://gitlab.example.com/team/api/-/issues/31","title":"Assigned issue","updated_at":"2026-05-22T07:00:00Z","author":{"username":"erin"},"references":{"full":"team/api#31"}}]
EOF
    ;;
  *"issues"*"author_id=1435"*)
    echo '[]'
    ;;
  *"todos"*"state=pending"*)
    cat <<'EOF'
[{"id":55,"body":"todo body","created_at":"2026-05-22T06:00:00Z","target_url":"https://gitlab.example.com/team/api/-/issues/32","target":{"iid":32,"title":"Todo issue","web_url":"https://gitlab.example.com/team/api/-/issues/32","updated_at":"2026-05-22T06:30:00Z","author":{"username":"frank"}},"project":{"path_with_namespace":"team/api"},"author":{"username":"notifier"}}]
EOF
    ;;
  *)
    echo "unexpected glab args: $*" >&2
    exit 99
    ;;
esac
"#;

const GLAB_FAIL_STUB: &str = r#"#!/bin/sh
echo "glab unavailable" >&2
exit 8
"#;

#[test]
fn inbox_github_dedupes_reasons_and_normalizes_items() {
    let stub = StubEnv::new().gh_stub(GH_INBOX_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "inbox",
            "list",
            "--limit",
            "9",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.inbox.list.v1");
    assert_eq!(env["data"]["providers"][0]["provider"], "github");
    assert_eq!(env["data"]["limit"], 9);
    let items = env["data"]["items"].as_array().expect("items");
    assert_eq!(items.len(), 3, "dedupe should collapse review+assigned PR");
    assert_eq!(items[0]["kind"], "review");
    assert_eq!(items[0]["reasons"][0], "review");
    assert_eq!(items[0]["reasons"][1], "assigned");
    assert_eq!(items[0]["repo"], "acme/widgets");
}

#[test]
fn inbox_gitlab_passes_hostname_and_normalizes_api_rows() {
    let stub = StubEnv::new()
        .env("FORGE_CLI_INBOX_GITLAB_HOST", "gitlab.env.example")
        .glab_stub(GLAB_INBOX_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "inbox",
            "list",
            "--gitlab-host",
            "gitlab.example.com",
            "--limit",
            "7",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["providers"][0]["host"], "gitlab.example.com");
    assert_eq!(env["data"]["providers"][0]["item_count"], 4);
    assert_eq!(env["data"]["items"][0]["kind"], "review");
    assert_eq!(env["data"]["items"][0]["repo"], "team/api");
    assert_eq!(env["data"]["items"][0]["source"], "gitlab_merge_requests");
    let items = env["data"]["items"].as_array().expect("items");
    let todo = items
        .iter()
        .find(|item| item["title"] == "Todo issue")
        .expect("todo item");
    assert_eq!(todo["author"], "frank");
}

#[test]
fn inbox_gitlab_host_can_default_from_env() {
    let stub = StubEnv::new()
        .env("FORGE_CLI_INBOX_GITLAB_HOST", "gitlab.env.example")
        .glab_stub(GLAB_INBOX_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "inbox",
            "list",
            "--limit",
            "7",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["providers"][0]["host"], "gitlab.env.example");
}

#[test]
fn inbox_list_partial_success_keeps_successful_provider() {
    let stub = StubEnv::new()
        .gh_stub(GH_EMPTY_STUB)
        .glab_stub(GLAB_FAIL_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--format",
            "json",
            "inbox",
            "list",
            "--gitlab-host",
            "gitlab.example.com",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["providers"][0]["ok"], true);
    assert_eq!(env["data"]["providers"][1]["ok"], false);
    assert_eq!(
        env["data"]["providers"][1]["error"]["kind"],
        "backend_error"
    );
    assert!(
        env["warnings"][0]
            .as_str()
            .unwrap()
            .contains("provider_failed: gitlab gitlab.example.com")
    );
}

#[test]
fn inbox_contract_all_selected_providers_failed_is_nonzero() {
    let stub = StubEnv::new()
        .gh_stub(GH_FAIL_STUB)
        .glab_stub(GLAB_FAIL_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--format",
            "json",
            "inbox",
            "list",
            "--gitlab-host",
            "gitlab.example.com",
        ],
    );
    assert_eq!(out.code, 1, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "backend_error");
}

#[test]
fn inbox_status_reports_bounded_reason_counts() {
    let stub = StubEnv::new().gh_stub(GH_INBOX_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "inbox",
            "status",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.inbox.status.v1");
    assert_eq!(env["data"]["limit"], 30);
    let counts = env["data"]["counts"].as_array().expect("counts");
    assert!(counts.iter().any(|row| row["reason"] == "review"));
    assert!(counts.iter().any(|row| row["reason"] == "assigned"));
}

#[test]
fn inbox_next_returns_ranked_review_items_first() {
    let stub = StubEnv::new().gh_stub(GH_INBOX_STUB);
    let out = run_forge_cli(
        &stub,
        &["--provider", "github", "--format", "json", "inbox", "next"],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.inbox.next.v1");
    assert_eq!(env["data"]["limit"], 5);
    assert_eq!(env["data"]["query_limit"], 30);
    let items = env["data"]["items"].as_array().expect("items");
    assert!(items.len() <= 5);
    assert_eq!(items[0]["kind"], "review");
}

/// Plans from `data.providers[0].plans` rendered as joined argv strings so
/// tests can match on individual search families without hand-walking JSON
/// arrays.
fn dry_run_plan_joins(env: &serde_json::Value, provider_index: usize) -> Vec<String> {
    env["data"]["providers"][provider_index]["plans"]
        .as_array()
        .expect("plans array")
        .iter()
        .map(|plan| {
            plan.as_array()
                .expect("plan argv array")
                .iter()
                .map(|s| s.as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

#[test]
fn inbox_item_type_github_default_plans_pr_and_issue_families() {
    let out = run_forge_cli(
        &StubEnv::new(),
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "--dry-run",
            "inbox",
            "list",
            "--limit",
            "30",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let plans = dry_run_plan_joins(&env, 0);
    assert!(
        plans
            .iter()
            .any(|p| p.contains("search prs --review-requested"))
    );
    assert!(plans.iter().any(|p| p.contains("search prs --assignee")));
    assert!(plans.iter().any(|p| p.contains("search issues --assignee")));
    assert!(plans.iter().any(|p| p.contains("search prs --author")));
    assert!(plans.iter().any(|p| p.contains("search issues --author")));
}

#[test]
fn inbox_item_type_github_pr_only_skips_issue_searches() {
    let out = run_forge_cli(
        &StubEnv::new(),
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "--dry-run",
            "inbox",
            "list",
            "--item-type",
            "pr",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let plans = dry_run_plan_joins(&env, 0);
    assert!(plans.iter().all(|p| !p.contains("search issues")));
    assert!(
        plans
            .iter()
            .any(|p| p.contains("search prs --review-requested"))
    );
    assert!(plans.iter().any(|p| p.contains("search prs --assignee")));
    assert!(plans.iter().any(|p| p.contains("search prs --author")));
}

#[test]
fn inbox_item_type_github_issue_only_skips_pr_searches() {
    let out = run_forge_cli(
        &StubEnv::new(),
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "--dry-run",
            "inbox",
            "list",
            "--item-type",
            "issue",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let plans = dry_run_plan_joins(&env, 0);
    // Review-requested is PR-only; issue-only must drop it.
    assert!(plans.iter().all(|p| !p.contains("search prs")));
    assert!(plans.iter().any(|p| p.contains("search issues --assignee")));
    assert!(plans.iter().any(|p| p.contains("search issues --author")));
}

#[test]
fn inbox_item_type_gitlab_default_plans_identity_and_default_families() {
    let out = run_forge_cli(
        &StubEnv::new(),
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "--dry-run",
            "inbox",
            "list",
            "--gitlab-host",
            "gitlab.example.com",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let plans = dry_run_plan_joins(&env, 0);
    assert!(
        plans
            .iter()
            .any(|p| p.contains("api user --hostname gitlab.example.com"))
    );
    assert!(
        plans
            .iter()
            .any(|p| p.contains("merge_requests?scope=assigned_to_me"))
    );
    assert!(
        plans
            .iter()
            .any(|p| p.contains("issues?scope=assigned_to_me"))
    );
    assert!(
        plans
            .iter()
            .any(|p| p.contains("merge_requests?reviewer_username=<username>"))
    );
    assert!(
        plans
            .iter()
            .any(|p| p.contains("merge_requests?author_id=<user_id>"))
    );
    assert!(
        plans
            .iter()
            .any(|p| p.contains("issues?author_id=<user_id>"))
    );
    assert!(plans.iter().any(|p| p.contains("todos?state=pending")));
}

#[test]
fn inbox_item_type_gitlab_pr_only_skips_issue_calls() {
    let out = run_forge_cli(
        &StubEnv::new(),
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "--dry-run",
            "inbox",
            "list",
            "--gitlab-host",
            "gitlab.example.com",
            "--item-type",
            "pr",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let plans = dry_run_plan_joins(&env, 0);
    // PR-only: identity still needed (review + authored MR), no issue queries,
    // todos still scheduled because target classification happens post-fetch.
    assert!(
        plans
            .iter()
            .any(|p| p.contains("api user --hostname gitlab.example.com"))
    );
    assert!(
        plans
            .iter()
            .any(|p| p.contains("merge_requests?scope=assigned_to_me"))
    );
    assert!(
        plans
            .iter()
            .any(|p| p.contains("merge_requests?reviewer_username=<username>"))
    );
    assert!(
        plans
            .iter()
            .any(|p| p.contains("merge_requests?author_id=<user_id>"))
    );
    assert!(plans.iter().any(|p| p.contains("todos?state=pending")));
    assert!(
        plans.iter().all(|p| !p.contains("/issues?")),
        "PR-only must skip issue API calls: {plans:?}"
    );
}

#[test]
fn inbox_item_type_gitlab_issue_only_skips_mr_calls() {
    let out = run_forge_cli(
        &StubEnv::new(),
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "--dry-run",
            "inbox",
            "list",
            "--gitlab-host",
            "gitlab.example.com",
            "--item-type",
            "issue",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let plans = dry_run_plan_joins(&env, 0);
    // Issue-only: review (MR-only) is dropped. Identity still needed for
    // authored issues. No MR queries.
    assert!(
        plans
            .iter()
            .any(|p| p.contains("api user --hostname gitlab.example.com"))
    );
    assert!(
        plans
            .iter()
            .any(|p| p.contains("issues?scope=assigned_to_me"))
    );
    assert!(
        plans
            .iter()
            .any(|p| p.contains("issues?author_id=<user_id>"))
    );
    assert!(plans.iter().any(|p| p.contains("todos?state=pending")));
    assert!(
        plans.iter().all(|p| !p.contains("merge_requests")),
        "issue-only must skip MR API calls: {plans:?}"
    );
}

#[test]
fn inbox_item_type_gitlab_skips_identity_when_no_query_needs_it() {
    let out = run_forge_cli(
        &StubEnv::new(),
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "--dry-run",
            "inbox",
            "list",
            "--gitlab-host",
            "gitlab.example.com",
            "--kind",
            "assigned",
            "--kind",
            "todo",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let plans = dry_run_plan_joins(&env, 0);
    assert!(
        plans.iter().all(|p| !p.contains("api user --hostname")),
        "assigned+todo must skip identity lookup: {plans:?}"
    );
    assert!(
        plans
            .iter()
            .any(|p| p.contains("merge_requests?scope=assigned_to_me"))
    );
    assert!(
        plans
            .iter()
            .any(|p| p.contains("issues?scope=assigned_to_me"))
    );
    assert!(plans.iter().any(|p| p.contains("todos?state=pending")));
}

/// GitLab todo stub returning two todos: one targets an issue, the other
/// targets a merge request. The item-type filter should keep only the
/// matching target type post-fetch.
const GLAB_TODOS_MIXED_STUB: &str = r#"#!/bin/sh
set -e
case "$*" in
  *"api user --hostname"*)
    cat <<'EOF'
{"id":1435,"username":"terrylin"}
EOF
    ;;
  *"todos"*"state=pending"*)
    cat <<'EOF'
[{"id":55,"target_type":"Issue","target_url":"https://gitlab.example.com/team/api/-/issues/32","target":{"iid":32,"title":"Todo issue","web_url":"https://gitlab.example.com/team/api/-/issues/32","updated_at":"2026-05-22T06:30:00Z","author":{"username":"frank"}},"project":{"path_with_namespace":"team/api"}},
 {"id":56,"target_type":"MergeRequest","target_url":"https://gitlab.example.com/team/api/-/merge_requests/77","target":{"iid":77,"title":"Todo MR","web_url":"https://gitlab.example.com/team/api/-/merge_requests/77","updated_at":"2026-05-22T06:45:00Z","author":{"username":"frank"}},"project":{"path_with_namespace":"team/api"}}]
EOF
    ;;
  *)
    echo '[]'
    ;;
esac
"#;

#[test]
fn inbox_item_type_gitlab_todo_pr_only_keeps_mr_targets() {
    let stub = StubEnv::new().glab_stub(GLAB_TODOS_MIXED_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "inbox",
            "list",
            "--gitlab-host",
            "gitlab.example.com",
            "--kind",
            "todo",
            "--item-type",
            "pr",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let items = env["data"]["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "expected only MR-target todo: {items:?}");
    assert_eq!(items[0]["title"], "Todo MR");
}

#[test]
fn inbox_item_type_gitlab_todo_issue_only_keeps_issue_targets() {
    let stub = StubEnv::new().glab_stub(GLAB_TODOS_MIXED_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "inbox",
            "list",
            "--gitlab-host",
            "gitlab.example.com",
            "--kind",
            "todo",
            "--item-type",
            "issue",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let items = env["data"]["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "expected only issue-target todo: {items:?}");
    assert_eq!(items[0]["title"], "Todo issue");
}

#[test]
fn inbox_item_type_gitlab_todo_all_keeps_unclassified_targets() {
    // Stub returns a todo with neither target_type nor a classifiable URL.
    let stub = StubEnv::new().glab_stub(
        r#"#!/bin/sh
set -e
case "$*" in
  *"todos"*"state=pending"*)
    cat <<'EOF'
[{"id":99,"target_url":"https://gitlab.example.com/team/snippets/-/snippets/5","target":{"iid":5,"title":"Mystery todo","web_url":"https://gitlab.example.com/team/snippets/-/snippets/5","updated_at":"2026-05-22T07:00:00Z","author":{"username":"frank"}},"project":{"path_with_namespace":"team/snippets"}}]
EOF
    ;;
  *)
    echo '[]'
    ;;
esac
"#,
    );
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "inbox",
            "list",
            "--gitlab-host",
            "gitlab.example.com",
            "--kind",
            "todo",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let items = env["data"]["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Mystery todo");

    // PR-only drops it.
    let stub = StubEnv::new().glab_stub(
        r#"#!/bin/sh
set -e
case "$*" in
  *"todos"*"state=pending"*)
    cat <<'EOF'
[{"id":99,"target_url":"https://gitlab.example.com/team/snippets/-/snippets/5","target":{"iid":5,"title":"Mystery todo","web_url":"https://gitlab.example.com/team/snippets/-/snippets/5","updated_at":"2026-05-22T07:00:00Z","author":{"username":"frank"}},"project":{"path_with_namespace":"team/snippets"}}]
EOF
    ;;
  *)
    echo '[]'
    ;;
esac
"#,
    );
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "inbox",
            "list",
            "--gitlab-host",
            "gitlab.example.com",
            "--kind",
            "todo",
            "--item-type",
            "pr",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let items = env["data"]["items"].as_array().expect("items");
    assert!(
        items.is_empty(),
        "unclassifiable todo must be filtered in PR-only mode"
    );
}
