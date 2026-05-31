//! `forge-cli search` provider seam + GitHub full-text tests.

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

const GH_SEARCH_STUB: &str = r#"#!/bin/sh
case "$*" in
  "search issues rumdl --repo acme/widget --match title,body,comments --limit 30 --json number,title,url,state,repository,isPullRequest")
    cat <<'JSON'
[{"number":7,"title":"ratelimit retry only in the body","url":"https://github.com/acme/widget/issues/7","state":"open","isPullRequest":false,"repository":{"nameWithOwner":"acme/widget"}},{"number":9,"title":"a referencing pull request","url":"https://github.com/acme/widget/pull/9","state":"closed","isPullRequest":true,"repository":{"nameWithOwner":"acme/widget"}}]
JSON
    ;;
  "search prs cache --repo acme/widget --match title --limit 5 --json number,title,url,state,repository,isPullRequest")
    cat <<'JSON'
[{"number":12,"title":"add cache layer","url":"https://github.com/acme/widget/pull/12","state":"open","isPullRequest":true,"repository":{"nameWithOwner":"acme/widget"}}]
JSON
    ;;
  "search issues nomatch --repo acme/widget --match title,body,comments --limit 30 --json number,title,url,state,repository,isPullRequest")
    printf '[]\n'
    ;;
  *)
    echo "unexpected gh argv: $*" >&2
    exit 97
    ;;
esac
"#;

#[test]
fn search_issues_github_normalizes_hits_including_body_only_match() {
    let stub = StubEnv::new().gh_stub(GH_SEARCH_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widget",
            "--format",
            "json",
            "search",
            "issues",
            "rumdl",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.search.issues.v1");
    assert_eq!(envelope["data"]["provider"], "github");
    assert_eq!(envelope["data"]["host"], "github.com");
    assert_eq!(envelope["data"]["repo"], "acme/widget");
    assert_eq!(envelope["data"]["query"], "rumdl");
    assert_eq!(envelope["data"]["match_fields"][0], "title");
    assert_eq!(envelope["data"]["match_fields"][2], "comments");
    assert_eq!(envelope["data"]["item_count"], 2);
    assert_eq!(envelope["data"]["limited"], false);
    // The first hit matched only on the body — a row `issue list` cannot surface.
    assert_eq!(envelope["data"]["items"][0]["kind"], "issue");
    assert_eq!(envelope["data"]["items"][0]["number"], 7);
    assert_eq!(
        envelope["data"]["items"][0]["title"],
        "ratelimit retry only in the body"
    );
    assert_eq!(envelope["data"]["items"][1]["kind"], "pr");
    assert_eq!(envelope["data"]["items"][1]["state"], "closed");
}

#[test]
fn search_prs_github_honours_narrowed_match_and_limit() {
    let stub = StubEnv::new().gh_stub(GH_SEARCH_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widget",
            "--format",
            "json",
            "search",
            "prs",
            "cache",
            "--match",
            "title",
            "--limit",
            "5",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.search.prs.v1");
    assert_eq!(envelope["data"]["item_count"], 1);
    assert_eq!(envelope["data"]["match_fields"][0], "title");
    assert_eq!(envelope["data"]["items"][0]["kind"], "pr");
    assert_eq!(envelope["data"]["items"][0]["number"], 12);
}

#[test]
fn search_issues_empty_result_is_well_formed_envelope() {
    let stub = StubEnv::new().gh_stub(GH_SEARCH_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widget",
            "--format",
            "json",
            "search",
            "issues",
            "nomatch",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.search.issues.v1");
    assert_eq!(envelope["data"]["item_count"], 0);
    assert_eq!(envelope["data"]["limited"], false);
    assert!(
        envelope["data"]["items"]
            .as_array()
            .expect("items")
            .is_empty(),
        "expected empty items: {envelope}"
    );
}

#[test]
fn search_issues_text_is_scannable() {
    let stub = StubEnv::new().gh_stub(GH_SEARCH_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widget",
            "search",
            "issues",
            "rumdl",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert!(
        out.stdout
            .contains("github@github.com search issues acme/widget \"rumdl\": 2 result(s)"),
        "unexpected stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains(
            "issue #7 [open] acme/widget ratelimit retry only in the body - https://github.com/acme/widget/issues/7"
        ),
        "unexpected stdout: {}",
        out.stdout
    );
}

#[test]
fn search_issues_dry_run_lists_gh_search_plan() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\nexit 97\n");
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
            "search",
            "issues",
            "rumdl",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    let plan = envelope["data"]["plan"].as_array().expect("plan array");
    // plan[0] is the resolved backend executable path (the stub under test),
    // so assert on the argv tail like the activity dry-run test does.
    assert_eq!(plan[1], "search");
    assert_eq!(plan[2], "issues");
    assert_eq!(plan[3], "rumdl");
    assert!(plan.iter().any(|v| v == "--repo"));
    assert!(plan.iter().any(|v| v == "acme/widget"));
    assert!(plan.iter().any(|v| v == "title,body,comments"));
}

#[test]
fn search_gitlab_branch_is_provider_unsupported() {
    let stub = StubEnv::new();
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--repo",
            "acme/widget",
            "--format",
            "json",
            "search",
            "issues",
            "rumdl",
        ],
    );
    assert_eq!(out.code, 64, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "provider_unsupported");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("search issues is GitHub-only in v1"),
        "unexpected envelope: {envelope}"
    );
    assert_eq!(envelope["error"]["details"]["detail"], "provider=gitlab");
}

#[test]
fn search_local_branch_is_provider_unsupported() {
    let stub = StubEnv::new();
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "local",
            "--repo",
            "acme/widget",
            "--format",
            "json",
            "search",
            "prs",
            "cache",
        ],
    );
    assert_eq!(out.code, 64, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "provider_unsupported");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("search prs is GitHub-only in v1"),
        "unexpected envelope: {envelope}"
    );
    assert_eq!(envelope["error"]["details"]["detail"], "provider=local");
}
