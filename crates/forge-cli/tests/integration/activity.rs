//! `forge-cli activity` provider seam tests.

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

const GH_ACTIVITY_STUB: &str = r#"#!/bin/sh
case "$*" in
  "api search/commits --method GET -f q=author:alice author-date:>=2026-05-01 -f sort=author-date -f order=desc -f per_page=2 --jq .items")
    cat <<'JSON'
[{"sha":"abc123","html_url":"https://github.com/acme/widget/commit/abc123","repository":{"full_name":"acme/widget"},"commit":{"message":"ship activity","author":{"name":"Alice","email":"alice@example.com","date":"2026-05-03T10:00:00Z"},"committer":{"date":"2026-05-03T10:05:00Z"}}}]
JSON
    ;;
  "api user --jq .login")
    printf 'alice\n'
    ;;
  "api users/alice/events --method GET -f per_page=2")
    cat <<'JSON'
[{"id":"1","type":"PushEvent","actor":{"login":"alice"},"repo":{"name":"acme/widget"},"payload":{"commits":[{"sha":"abc123","message":"ship activity"}]},"public":false,"created_at":"2026-05-03T10:05:00Z"}]
JSON
    ;;
  "api users/bob/events/public --method GET -f per_page=2")
    printf '[]\n'
    ;;
  "api repos/acme/widget/commits --method GET -f per_page=2 -f since=2026-06-01")
    cat <<'JSON'
[{"sha":"def456","html_url":"https://github.com/acme/widget/commit/def456","commit":{"message":"feed commit\n\nbody","author":{"name":"Bob","date":"2026-06-01T10:00:00Z"},"committer":{"date":"2026-06-01T10:05:00Z"}}},{"sha":"old999","html_url":"https://github.com/acme/widget/commit/old999","commit":{"message":"offset older commit","author":{"name":"Eve","date":"2026-06-01T10:30:00+01:00"},"committer":{"date":"2026-06-01T10:30:00+01:00"}}}]
JSON
    ;;
  "api repos/acme/widget/activity --method GET -f per_page=2 -f time_period=year")
    cat <<'JSON'
[{"before":"aaaaaaa","after":"bbbbbbb","ref":"refs/heads/stale","pushed_at":"2026-05-31T23:59:59Z","push_type":"normal","pusher":{"login":"alice"}},{"before":"1111111","after":"2222222","ref":"refs/heads/main","pushed_at":"2026-06-01T10:10:00Z","push_type":"normal","pusher":{"login":"alice"}}]
JSON
    ;;
  "api graphql -F login=alice -F maxRepositories=2 -F from=2026-05-01T00:00:00Z -f "*)
    cat <<'JSON'
{"data":{"user":{"login":"alice","contributionsCollection":{"totalCommitContributions":3,"commitContributionsByRepository":[{"repository":{"nameWithOwner":"acme/widget"},"contributions":{"nodes":[{"commitCount":2,"occurredAt":"2026-05-03T00:00:00Z"},{"commitCount":1,"occurredAt":"2026-05-01T00:00:00Z"}]}}]}}}}
JSON
    ;;
  *)
    echo "unexpected gh argv: $*" >&2
    exit 97
    ;;
esac
"#;

const GLAB_ACTIVITY_STUB: &str = r#"#!/bin/sh
case "$*" in
  "api --hostname gitlab.com projects/group%2Fwidget/repository/commits?per_page=3&since=2026-06-01")
    cat <<'JSON'
[{"id":"abc123abc123","title":"gitlab feed commit","author_name":"Dana","committed_date":"2026-06-01T10:00:00.000+00:00","web_url":"https://gitlab.com/group/widget/-/commit/abc123abc123"}]
JSON
    ;;
  "api --hostname gitlab.com projects/group%2Fwidget/events?per_page=3&sort=desc&after=2026-06-01")
    cat <<'JSON'
[{"id":7,"action_name":"pushed to","target_type":null,"author_username":"dana","created_at":"2026-06-01T10:15:00.000Z","push_data":{"ref":"main","commit_from":"1111111","commit_to":"2222222","commit_title":"push title"}},{"id":8,"action_name":"custom action","target_type":"Pipeline","author":{"username":"ops"},"created_at":"2026-06-01T10:20:00.000Z","target_title":"pipeline #1"}]
JSON
    ;;
  *)
    echo "unexpected glab argv: $*" >&2
    exit 97
    ;;
esac
"#;

#[test]
fn activity_commits_github_normalizes_search_results() {
    let stub = StubEnv::new().gh_stub(GH_ACTIVITY_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "activity",
            "commits",
            "--user",
            "alice",
            "--since",
            "2026-05-01",
            "--limit",
            "2",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(
        envelope["schema_version"],
        "cli.forge-cli.activity.commits.v1"
    );
    assert_eq!(envelope["data"]["provider"], "github");
    assert_eq!(envelope["data"]["host"], "github.com");
    assert_eq!(envelope["data"]["user"], "alice");
    assert_eq!(envelope["data"]["item_count"], 1);
    assert_eq!(envelope["data"]["limited"], false);
    assert_eq!(envelope["data"]["items"][0]["repo"], "acme/widget");
    assert_eq!(envelope["data"]["items"][0]["sha"], "abc123");
    assert_eq!(envelope["data"]["items"][0]["message"], "ship activity");
    assert_eq!(
        envelope["data"]["items"][0]["authored_at"],
        "2026-05-03T10:00:00Z"
    );
}

#[test]
fn activity_commits_ignores_irrelevant_repository_shape() {
    let stub = StubEnv::new().gh_stub(GH_ACTIVITY_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "owner/nested/repo",
            "--format",
            "json",
            "activity",
            "commits",
            "--user",
            "alice",
            "--since",
            "2026-05-01",
            "--limit",
            "2",
        ],
    );
    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["data"]["item_count"], 1);
}

#[test]
fn activity_events_github_resolves_me_and_normalizes_events() {
    let stub = StubEnv::new().gh_stub(GH_ACTIVITY_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "activity",
            "events",
            "--limit",
            "2",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(
        envelope["schema_version"],
        "cli.forge-cli.activity.events.v1"
    );
    assert_eq!(envelope["data"]["host"], "github.com");
    assert_eq!(envelope["data"]["user"], "alice");
    assert_eq!(envelope["data"]["item_count"], 1);
    assert_eq!(envelope["data"]["limited"], false);
    assert_eq!(envelope["data"]["items"][0]["event_type"], "PushEvent");
    assert_eq!(envelope["data"]["items"][0]["repo"], "acme/widget");
    assert_eq!(envelope["data"]["items"][0]["actor"], "alice");
    assert_eq!(envelope["data"]["items"][0]["public"], false);
}

#[test]
fn activity_feed_github_normalizes_repo_activity_rows() {
    let stub = StubEnv::new().gh_stub(GH_ACTIVITY_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widget",
            "--format",
            "json",
            "activity",
            "feed",
            "--since",
            "2026-06-01",
            "--limit",
            "2",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.activity.feed.v1");
    assert_eq!(envelope["data"]["provider"], "github");
    assert_eq!(envelope["data"]["repo"], "acme/widget");
    assert_eq!(envelope["data"]["item_count"], 2);
    assert_eq!(envelope["data"]["limited"], true);
    assert_eq!(envelope["data"]["items"][0]["kind"], "branch");
    assert_eq!(envelope["data"]["items"][0]["action"], "pushed");
    assert_eq!(
        envelope["data"]["items"][0]["provider_event_type"],
        "normal"
    );
    assert_eq!(
        envelope["data"]["items"][0]["target_ref"],
        "refs/heads/main"
    );
    assert_eq!(envelope["data"]["items"][0]["details"]["before"], "1111111");
    let refs = envelope["data"]["items"]
        .as_array()
        .expect("items array")
        .iter()
        .filter_map(|item| item["target_ref"].as_str())
        .collect::<Vec<_>>();
    assert!(
        !refs.contains(&"refs/heads/stale"),
        "before-since repo activity must be filtered: {envelope}"
    );
    assert_eq!(envelope["data"]["items"][1]["kind"], "commit");
    assert_eq!(envelope["data"]["items"][1]["action"], "committed");
    assert_eq!(envelope["data"]["items"][1]["title"], "feed commit");
    assert!(
        !envelope["data"]["items"]
            .as_array()
            .expect("items array")
            .iter()
            .any(|item| item["title"] == "offset older commit"),
        "offset timestamp sorting should not let older commit displace newer rows: {envelope}"
    );
}

#[test]
fn activity_feed_gitlab_normalizes_project_events_without_flattening_provider_semantics() {
    let stub = StubEnv::new().glab_stub(GLAB_ACTIVITY_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--host",
            "gitlab.com",
            "--repo",
            "group/widget",
            "--format",
            "json",
            "activity",
            "feed",
            "--since",
            "2026-06-01",
            "--limit",
            "3",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.activity.feed.v1");
    assert_eq!(envelope["data"]["provider"], "gitlab");
    assert_eq!(envelope["data"]["repo"], "group/widget");
    assert_eq!(envelope["data"]["item_count"], 3);
    assert_eq!(
        envelope["data"]["items"][0]["provider_event_type"],
        "custom action"
    );
    assert_eq!(envelope["data"]["items"][0]["kind"], "repository");
    assert_eq!(envelope["data"]["items"][0]["action"], "custom_action");
    assert_eq!(
        envelope["data"]["items"][0]["details"]["target_type"],
        "Pipeline"
    );
    assert_eq!(envelope["data"]["items"][1]["kind"], "push");
    assert_eq!(envelope["data"]["items"][1]["action"], "pushed");
    assert_eq!(envelope["data"]["items"][1]["target_ref"], "main");
    assert_eq!(
        envelope["data"]["items"][1]["details"]["commit_to"],
        "2222222"
    );
    assert_eq!(envelope["data"]["items"][2]["kind"], "commit");
}

#[test]
fn activity_summary_github_normalizes_graphql_contributions() {
    let stub = StubEnv::new().gh_stub(GH_ACTIVITY_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "activity",
            "summary",
            "--user",
            "alice",
            "--since",
            "2026-05-01",
            "--limit",
            "2",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(
        envelope["schema_version"],
        "cli.forge-cli.activity.summary.v1"
    );
    assert_eq!(envelope["data"]["host"], "github.com");
    assert_eq!(envelope["data"]["total_commit_contributions"], 3);
    assert_eq!(envelope["data"]["repository_count"], 1);
    assert_eq!(envelope["data"]["limited"], false);
    assert_eq!(envelope["data"]["repositories"][0]["repo"], "acme/widget");
    assert_eq!(
        envelope["data"]["repositories"][0]["commit_contributions"],
        3
    );
    assert_eq!(
        envelope["data"]["repositories"][0]["latest_commit_at"],
        "2026-05-03T00:00:00Z"
    );
}

#[test]
fn activity_commits_text_is_scannable() {
    let stub = StubEnv::new().gh_stub(GH_ACTIVITY_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "activity",
            "commits",
            "--user",
            "alice",
            "--since",
            "2026-05-01",
            "--limit",
            "2",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert!(
        out.stdout
            .contains("github@github.com alice commits: 1 result(s) since 2026-05-01"),
        "unexpected stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains(
            "2026-05-03T10:00:00Z acme/widget abc123 ship activity - https://github.com/acme/widget/commit/abc123"
        ),
        "unexpected stdout: {}",
        out.stdout
    );
}

#[test]
fn activity_events_text_reports_empty_results() {
    let stub = StubEnv::new().gh_stub(GH_ACTIVITY_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "activity",
            "events",
            "--user",
            "bob",
            "--public-only",
            "--limit",
            "2",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        "github@github.com bob events: 0 result(s) public_only=true"
    );
}

#[test]
fn activity_commits_dry_run_lists_github_search_plan() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\nexit 97\n");
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "activity",
            "commits",
            "--user",
            "alice",
            "--limit",
            "2",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["data"]["host"], "github.com");
    let plan = envelope["data"]["plan"].as_array().expect("plan array");
    assert_eq!(plan[1], "api");
    assert_eq!(plan[2], "search/commits");
    assert!(plan.iter().any(|v| v == "q=author:alice"));
}

#[test]
fn activity_commits_dry_run_clamps_limit_to_github_page_size() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\nexit 97\n");
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "activity",
            "commits",
            "--user",
            "alice",
            "--limit",
            "500",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    let plan = envelope["data"]["plan"].as_array().expect("plan array");
    assert!(plan.iter().any(|v| v == "per_page=100"));
}

#[test]
fn activity_feed_dry_run_lists_gitlab_project_plans_and_clamps_limit() {
    let stub = StubEnv::new().glab_stub("#!/bin/sh\nexit 97\n");
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--host",
            "gitlab.com",
            "--repo",
            "group/widget",
            "--dry-run",
            "--format",
            "json",
            "activity",
            "feed",
            "--limit",
            "500",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    let plans = envelope["data"]["plans"].as_array().expect("plans array");
    assert_eq!(plans.len(), 2);
    let commit_plan = plans[0].as_array().expect("commit plan");
    let events_plan = plans[1].as_array().expect("events plan");
    assert!(
        commit_plan
            .iter()
            .any(|v| { v == "projects/group%2Fwidget/repository/commits?per_page=100" })
    );
    assert!(
        events_plan
            .iter()
            .any(|v| { v == "projects/group%2Fwidget/events?per_page=100&sort=desc" })
    );
}

#[test]
fn activity_feed_rejects_invalid_since_before_backend() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 97\n");
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widget",
            "--dry-run",
            "--format",
            "json",
            "activity",
            "feed",
            "--since",
            "not-a-date",
        ],
    );
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "invalid_since");
    assert!(
        !out.stderr.contains("should-not-run"),
        "invalid --since must stop before invoking provider backend: {}",
        out.stderr
    );
}

#[test]
fn activity_feed_local_branch_is_activity_specific_provider_unsupported() {
    let stub = StubEnv::new();
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "local",
            "--repo",
            "local:demo",
            "--format",
            "json",
            "activity",
            "feed",
        ],
    );
    assert_eq!(out.code, 64, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "provider_unsupported");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("activity feed is unsupported"),
        "unexpected envelope: {envelope}"
    );
    assert_eq!(envelope["error"]["details"]["detail"], "provider=local");
}

#[test]
fn activity_gitlab_branch_is_provider_unsupported() {
    let stub = StubEnv::new();
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--host",
            "gitlab.com",
            "--format",
            "json",
            "activity",
            "events",
        ],
    );
    assert_eq!(out.code, 64, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "provider_unsupported");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("activity events is GitHub-only in v1"),
        "unexpected envelope: {envelope}"
    );
    assert_eq!(envelope["error"]["details"]["detail"], "provider=gitlab");
}

#[test]
fn activity_local_branch_is_activity_specific_provider_unsupported() {
    let stub = StubEnv::new();
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "local",
            "--format",
            "json",
            "activity",
            "summary",
        ],
    );
    assert_eq!(out.code, 64, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "provider_unsupported");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("activity summary is GitHub-only in v1"),
        "unexpected envelope: {envelope}"
    );
    assert_eq!(envelope["error"]["details"]["detail"], "provider=local");
}
