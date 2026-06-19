//! Sprint 5 integration tests for the issue atoms. Each test exercises a
//! single atom end-to-end against a stubbed gh / glab so the CLI surface +
//! envelope contract are pinned without hitting any real provider.
//!
//! The fuller per-op fixture corpus (gh + glab JSON pairs for every action)
//! lands in Sprint 7's parity harness; this module covers the acceptance
//! criteria called out in the plan:
//!
//! - `issue create --dry-run --format json` renders the plan envelope.
//! - `issue create` with an over-cap title exits DATA 65 / `title_too_long`.
//! - Each of view / close / reopen / comment / edit emits the right schema
//!   literal and snake_case envelope.

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

const FORBIDDEN_STUB: &str = "#!/bin/sh\necho 'should not run during dry-run' >&2\nexit 99\n";

fn issue_view_stub(state: &str, number: u64) -> String {
    let json = format!(
        r#"{{"number":{number},"url":"https://github.com/acme/widgets/issues/{number}","state":"{state}","title":"t","body":"b","labels":[{{"name":"bug"}}],"assignees":[{{"login":"alice"}}]}}"#
    );
    format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "issue view")
    cat <<'EOF'
{json}
EOF
    ;;
  "issue close"|"issue reopen"|"issue edit")
    :
    ;;
  api\ *)
    echo "https://github.com/acme/widgets/issues/{number}#issuecomment-123"
    ;;
  "issue create")
    echo "creating issue..."
    echo "https://github.com/acme/widgets/issues/{number}"
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
    )
}

#[test]
fn issue_create_dry_run_renders_plan_with_title_and_body_file() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "issue",
            "create",
            "--title",
            "demo title",
            "--body",
            "body text",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.issue.create.v1");
    let plan: Vec<String> = env["data"]["plan"]
        .as_array()
        .expect("plan array")
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert!(plan.iter().any(|s| s == "issue"), "{plan:?}");
    assert!(plan.iter().any(|s| s == "create"), "{plan:?}");
    let t = plan.iter().position(|s| s == "--title").unwrap();
    assert_eq!(plan[t + 1], "demo title");
    assert!(plan.iter().any(|s| s == "--body-file"), "{plan:?}");
}

#[test]
fn issue_create_title_over_70_chars_exits_data_with_title_too_long() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let long_title = "a".repeat(71);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "issue",
            "create",
            "--title",
            &long_title,
            "--body",
            "b",
        ],
    );
    assert_eq!(
        out.code, 65,
        "expected DATA 65 on title_too_long, stderr={}",
        out.stderr
    );
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "title_too_long");
}

#[test]
fn issue_view_emits_canonical_envelope() {
    let stub = StubEnv::new().gh_stub(&issue_view_stub("OPEN", 42));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "issue",
            "view",
            "42",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.issue.view.v1");
    assert_eq!(env["data"]["state"], "open");
    assert_eq!(env["data"]["number"], 42);
    assert_eq!(env["data"]["labels"][0], "bug");
    assert_eq!(env["data"]["assignees"][0], "alice");
}

#[test]
fn issue_close_runs_close_then_view_and_emits_closed_state() {
    let stub = StubEnv::new().gh_stub(&issue_view_stub("CLOSED", 7));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "issue",
            "close",
            "7",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.issue.close.v1");
    assert_eq!(env["data"]["state"], "closed");
    assert_eq!(env["data"]["number"], 7);
}

#[test]
fn issue_reopen_runs_reopen_then_view_and_emits_open_state() {
    let stub = StubEnv::new().gh_stub(&issue_view_stub("OPEN", 7));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "issue",
            "reopen",
            "7",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.issue.reopen.v1");
    assert_eq!(env["data"]["state"], "open");
}

#[test]
fn issue_comment_runs_comment_then_view_and_emits_url() {
    let stub = StubEnv::new().gh_stub(&issue_view_stub("OPEN", 11));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "issue",
            "comment",
            "11",
            "--body",
            "hello",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.issue.comment.v1");
    assert_eq!(env["data"]["number"], 11);
    assert_eq!(
        env["data"]["url"],
        "https://github.com/acme/widgets/issues/11#issuecomment-123"
    );
}

#[test]
fn issue_comment_empty_body_exits_data_with_body_missing_summary() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "issue",
            "comment",
            "1",
        ],
    );
    assert_eq!(out.code, 65, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "body_missing_summary");
}

#[test]
fn issue_edit_revalidates_title_length_when_supplied() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let long_title = "z".repeat(71);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "issue",
            "edit",
            "5",
            "--title",
            &long_title,
        ],
    );
    assert_eq!(out.code, 65, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "title_too_long");
}

#[test]
fn issue_edit_dry_run_renders_plan_with_add_label_remove_label() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "issue",
            "edit",
            "5",
            "--add-label",
            "bug",
            "--remove-label",
            "stale",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let plan: Vec<String> = env["data"]["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert!(plan.iter().any(|s| s == "--add-label"), "{plan:?}");
    assert!(plan.iter().any(|s| s == "--remove-label"), "{plan:?}");
}
