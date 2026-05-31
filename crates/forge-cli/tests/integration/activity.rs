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
fn activity_gitlab_branch_is_provider_unsupported() {
    let stub = StubEnv::new();
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
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
