//! Full-chain integration tests for the `pr deliver` macro. These exercise
//! the entire execute_sequence — auth.status / repo.view / pr.create /
//! pr.wait-checks / pr.ready / pr.merge — against a comprehensive gh stub
//! that branches on argv. The dry-run path is covered by the sibling
//! `pr_deliver.rs` module; this one pins the lock-step ordering and
//! short-circuit behaviour against real subprocess wiring.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use forge_cli::backend::{BackendCall, BackendRunner, BackendSuccess};
use forge_cli::cli::{GlobalFlags, MergeMethodFlag, PrDeliverArgs, PrKindFlag, ProviderFlag};
use forge_cli::error::ForgeError;
use forge_cli::macros::pr_deliver;
use forge_cli::ops::pr_wait_checks::SystemClock;
use forge_cli::ops::review_state::{
    ReviewLoopBudget, ReviewLoopState, ReviewStatePayload, ReviewStateRecord,
};
use nils_common::cli_contract::OutputFormat;
use nils_test_support::fixtures::SubjectBoundEvidenceFixture;

use super::support::{CmdOutput, StubEnv, parse_envelope, run_forge_cli_in, write_label_catalog};

const FIXTURE_CREATE_STDOUT: &str = include_str!("../fixtures/github/pr_create/create_stdout.txt");
const FIXTURE_CHECKS_JSON: &str = include_str!("../fixtures/github/pr_checks/all_success.json");
const FIXTURE_PENDING_CHECKS_JSON: &str =
    include_str!("../fixtures/github/pr_checks/all_pending.json");
const FIXTURE_FAILED_CHECKS_JSON: &str =
    include_str!("../fixtures/github/pr_checks/mixed_failure_required.json");
const FIXTURE_EMPTY_CHECKS_JSON: &str = include_str!("../fixtures/github/pr_checks/empty.json");

fn clean_review_loop_comment(repository: &str, pr: u64, head: &str) -> String {
    let marker = ReviewStateRecord::new(
        repository,
        pr,
        head,
        0,
        None,
        ReviewStatePayload::ReviewLoop {
            state: ReviewLoopState {
                head_sha: head.to_string(),
                round: 0,
                no_progress_rounds: 0,
                budget: ReviewLoopBudget::default(),
                findings: BTreeMap::new(),
                extensions: Vec::new(),
                hard_stop: None,
            },
        },
    )
    .expect("review-loop record")
    .marker()
    .expect("review-loop marker");
    serde_json::json!({
        "author": {"login": "testuser-gh"},
        "authorAssociation": "MEMBER",
        "body": marker,
        "createdAt": "2026-07-20T12:00:00Z"
    })
    .to_string()
}

/// Full pr.view JSON used by every step that re-fetches the PR. The fixture
/// at `tests/fixtures/github/pr_create/view_response.json` was designed for
/// pr.create's bespoke parser and omits `state`/`mergeable`; the macro's
/// chain reuses `pr_view::parse_view_output` (via pr.ready / pr.merge) which
/// requires the full Sprint 2 shape, so we inline a complete payload here.
const FULL_PR_VIEW_JSON: &str = r#"{
  "number": 123,
  "url": "https://github.com/sympoies/nils-cli/pull/123",
  "state": "OPEN",
  "isDraft": false,
  "title": "feat: sample feature",
  "headRefName": "feat/sample",
  "baseRefName": "main",
  "mergeable": "MERGEABLE",
  "mergedAt": null,
  "labels": []
}"#;

/// Same as [`FULL_PR_VIEW_JSON`] plus a `body` whose description still
/// carries an unchecked GFM task-list item, used to exercise merge
/// lock-down rule 14 on the GitHub path (`extract_body` reads `body`).
const UNCHECKED_TASKS_PR_VIEW_JSON: &str = r###"{
  "number": 123,
  "url": "https://github.com/sympoies/nils-cli/pull/123",
  "state": "OPEN",
  "isDraft": false,
  "title": "feat: sample feature",
  "headRefName": "feat/sample",
  "baseRefName": "main",
  "mergeable": "MERGEABLE",
  "mergedAt": null,
  "labels": [],
  "body": "## Summary\n\nx\n\n## Test plan\n\n- [x] unit\n- [ ] run e2e suite\n"
}"###;

/// Post-merge `pr view` payload — same as [`FULL_PR_VIEW_JSON`] except the
/// state has flipped to `MERGED`. Used by stubs that need to surface a
/// merged PR after the merge step ran (and possibly exited non-zero).
const MERGED_PR_VIEW_JSON: &str = r#"{
  "number": 123,
  "url": "https://github.com/sympoies/nils-cli/pull/123",
  "state": "MERGED",
  "isDraft": false,
  "title": "feat: sample feature",
  "headRefName": "feat/sample",
  "baseRefName": "main",
  "mergeable": "MERGEABLE",
  "mergedAt": "2026-05-23T12:00:00Z",
  "labels": []
}"#;

/// Post-merge `pr view` payload carrying one closing-keyword linked issue
/// (`closingIssuesReferences`), used to exercise the deterministic
/// issue-closeout step.
const MERGED_PR_VIEW_WITH_CLOSING_JSON: &str = r#"{
  "number": 123,
  "url": "https://github.com/sympoies/nils-cli/pull/123",
  "state": "MERGED",
  "isDraft": false,
  "title": "feat: sample feature",
  "headRefName": "feat/sample",
  "baseRefName": "main",
  "mergeable": "MERGEABLE",
  "mergedAt": "2026-05-23T12:00:00Z",
  "labels": [],
  "closingIssuesReferences": [
    { "number": 4242, "url": "https://github.com/sympoies/nils-cli/issues/4242" }
  ]
}"#;

/// `issue view 4242` payload in the shape `issue_view::parse_view_output`
/// expects. The closeout step reads `state` to decide whether to close.
const LINKED_ISSUE_OPEN_JSON: &str = r#"{"number":4242,"url":"https://github.com/sympoies/nils-cli/issues/4242","state":"OPEN","title":"linked","labels":[],"assignees":[],"body":""}"#;

/// Same issue already closed — GitHub's async auto-close won the race, so the
/// closeout step must leave it alone.
const LINKED_ISSUE_CLOSED_JSON: &str = r#"{"number":4242,"url":"https://github.com/sympoies/nils-cli/issues/4242","state":"CLOSED","title":"linked","labels":[],"assignees":[],"body":""}"#;

/// One open PR for `feat/sample`, in the shape `gh pr list --json` returns.
/// Used by the adopt-path stubs to answer the macro's head-branch lookup.
const OPEN_PR_LIST_JSON: &str = r#"[{"number":123,"url":"https://github.com/sympoies/nils-cli/pull/123","state":"OPEN","title":"feat: sample feature","headRefName":"feat/sample","author":{"login":"testuser-gh"}}]"#;

/// Adoptable draft PR view: open, draft, and carrying a gate-compliant body
/// (so both the adopt-time body re-validation and the merge-time task-list
/// gate pass).
const ADOPTABLE_PR_VIEW_JSON: &str = r###"{
  "number": 123,
  "url": "https://github.com/sympoies/nils-cli/pull/123",
  "state": "OPEN",
  "isDraft": true,
  "title": "feat: sample feature",
  "headRefName": "feat/sample",
  "baseRefName": "main",
  "mergeable": "MERGEABLE",
  "mergedAt": null,
  "labels": [],
  "body": "## Summary\n\nAdopted draft.\n\n## Test plan\n\n- [x] unit\n"
}"###;

/// Post-ready view of the adopted PR: same record once `pr ready` promoted
/// the draft, so the merge step's draft gate passes.
const ADOPTED_READY_PR_VIEW_JSON: &str = r###"{
  "number": 123,
  "url": "https://github.com/sympoies/nils-cli/pull/123",
  "state": "OPEN",
  "isDraft": false,
  "title": "feat: sample feature",
  "headRefName": "feat/sample",
  "baseRefName": "main",
  "mergeable": "MERGEABLE",
  "mergedAt": null,
  "labels": [],
  "body": "## Summary\n\nAdopted draft.\n\n## Test plan\n\n- [x] unit\n"
}"###;

/// Same as [`ADOPTABLE_PR_VIEW_JSON`] but the body lacks the required
/// `## Summary` / `## Test plan` sections, so the adopt-time body
/// re-validation must fail closed.
const ADOPT_VIEW_MISSING_SECTIONS_JSON: &str = r###"{
  "number": 123,
  "url": "https://github.com/sympoies/nils-cli/pull/123",
  "state": "OPEN",
  "isDraft": true,
  "title": "feat: sample feature",
  "headRefName": "feat/sample",
  "baseRefName": "main",
  "mergeable": "MERGEABLE",
  "mergedAt": null,
  "labels": [],
  "body": "no required sections here"
}"###;

/// Build the canonical Sprint 2 git tempdir: clean worktree on `feat/sample`
/// tracking `origin/feat/sample` at the same SHA, with the remote URL set to
/// `https://<host>/<repo_slug>.git` so provider detection lands on GitHub.
fn make_git_repo() -> TempDir {
    let tempdir = TempDir::new().expect("tempdir");
    let repo = tempdir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");

    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .expect("git spawn");
        if !out.status.success() {
            panic!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        out
    };

    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "Tester"]);
    git(&["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "init\n").expect("readme");
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "initial"]);
    git(&["checkout", "-q", "-b", "feat/sample"]);
    git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/sympoies/nils-cli.git",
    ]);
    let head_sha = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string();
    let upstream_ref = repo.join(".git/refs/remotes/origin/feat/sample");
    fs::create_dir_all(upstream_ref.parent().unwrap()).unwrap();
    fs::write(&upstream_ref, format!("{head_sha}\n")).unwrap();
    git(&[
        "branch",
        "-q",
        "--set-upstream-to=origin/feat/sample",
        "feat/sample",
    ]);
    tempdir
}

fn bind_test_first_phase(phase: &str, repo: &Path, evidence: &Path) -> i32 {
    let evidence_arg = evidence.to_string_lossy().to_string();
    let repo_arg = repo.to_string_lossy().to_string();
    agent_workflow_primitives::test_first_evidence::run_with_args([
        "test-first-evidence",
        phase,
        "--out",
        &evidence_arg,
        "--project-path",
        &repo_arg,
    ])
}

fn make_subject_bound_git_repo() -> (SubjectBoundEvidenceFixture, PathBuf, String) {
    let fixture = SubjectBoundEvidenceFixture::new(
        "https://github.com/sympoies/nils-cli.git",
        &[(".forge-cli.toml", "[test_first]\nrequire = true\n")],
        bind_test_first_phase,
    );
    git(&fixture.repo, &["checkout", "-q", "-b", "feat/sample"]);
    update_upstream_ref(&fixture.repo);
    let head = git_output(&fixture.repo, &["rev-parse", "HEAD"]);
    let evidence = fixture.evidence.clone();
    (fixture, evidence, head)
}

fn update_upstream_ref(repo: &Path) {
    let head = git_output(repo, &["rev-parse", "HEAD"]);
    let upstream = repo.join(".git/refs/remotes/origin/feat/sample");
    fs::create_dir_all(upstream.parent().expect("upstream parent"))
        .expect("create upstream ref parent");
    fs::write(upstream, format!("{head}\n")).expect("upstream ref");
    git(
        repo,
        &[
            "branch",
            "-q",
            "--set-upstream-to=origin/feat/sample",
            "feat/sample",
        ],
    );
}

fn git_output(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git spawn");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git stdout")
        .trim()
        .to_string()
}

fn view_with_head_oid(template: &str, head: &str) -> String {
    template.replace(
        "\"headRefName\": \"feat/sample\",",
        &format!(
            "\"headRefName\": \"feat/sample\",\n  \"headRefOid\": \"{head}\",\n  \"headRepository\": {{ \"nameWithOwner\": \"sympoies/nils-cli\" }},"
        ),
    )
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git spawn");
    if !out.status.success() {
        panic!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn write_full_chain_stub(stub: &StubEnv) -> PathBuf {
    write_chain_stub(stub, FULL_PR_VIEW_JSON, MERGED_PR_VIEW_JSON, false)
}

fn write_catalog_mutating_chain_stub(stub: &StubEnv, catalog: &Path) -> PathBuf {
    let path = write_full_chain_stub(stub);
    let inner = stub.tempdir.path().join("gh-inner");
    fs::rename(&path, &inner).expect("rename inner gh stub");
    let body = format!(
        "#!/bin/sh\nset -e\nif [ \"$1 $2\" = \"pr list\" ]; then\n  rm -f -- '{}'\nfi\nexec '{}' \"$@\"\n",
        catalog.display(),
        inner.display(),
    );
    fs::write(&path, body).expect("write mutating gh wrapper");
    let mut perm = fs::metadata(&path).expect("metadata").permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).expect("chmod");
    path
}

fn write_provider_mutating_chain_stub(stub: &StubEnv, repo: &Path) -> PathBuf {
    let path = write_full_chain_stub(stub);
    let inner = stub.tempdir.path().join("gh-inner-provider-drift");
    fs::rename(&path, &inner).expect("rename inner gh stub");
    let body = format!(
        "#!/bin/sh\nset -e\nif [ \"$1 $2\" = \"pr list\" ]; then\n  git -C '{}' remote set-url origin https://gitlab.com/sympoies/nils-cli.git\nfi\nexec '{}' \"$@\"\n",
        repo.display(),
        inner.display(),
    );
    fs::write(&path, body).expect("write provider-mutating gh wrapper");
    let mut perm = fs::metadata(&path).expect("metadata").permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).expect("chmod");
    path
}

fn write_pr_only_label_catalog() -> (TempDir, String) {
    let tempdir = TempDir::new().expect("label catalog tempdir");
    let path = tempdir.path().join("forge-labels.yaml");
    fs::write(
        &path,
        r#"schema: forge-label-catalog.v1
groups:
  - name: type
    prefix: "type::"
    exclusive: true
labels:
  - name: "type::feature"
    group: type
    color: a2eeef
    description: Feature work.
    applies_to: [pr]
"#,
    )
    .expect("write PR-only catalog");
    (tempdir, path.to_string_lossy().into_owned())
}

/// Chain stub with a controllable `pr merge` failure mode used by the
/// idempotency-on-non-zero-exit regression test. When `merge_exits_one` is
/// true, the merge subcommand touches a sentinel file in `stub.tempdir`
/// and exits 1 with a non-fatal stderr message. Subsequent `pr view`
/// calls observe the sentinel and switch to the post-merge JSON so the
/// macro can verify the PR actually landed.
fn write_chain_stub_with_merge_exit(
    stub: &StubEnv,
    pre_view: &str,
    post_view: &str,
    merge_exits_one: bool,
) -> PathBuf {
    write_chain_stub(stub, pre_view, post_view, merge_exits_one)
}

fn write_chain_stub(
    stub: &StubEnv,
    pre_view: &str,
    post_view: &str,
    merge_exits_one: bool,
) -> PathBuf {
    write_chain_stub_with_checks(
        stub,
        pre_view,
        post_view,
        merge_exits_one,
        FIXTURE_CHECKS_JSON,
        FIXTURE_CHECKS_JSON,
    )
}

fn write_zero_required_checks_stub(stub: &StubEnv, all_checks: &str) -> PathBuf {
    write_chain_stub_with_checks(
        stub,
        FULL_PR_VIEW_JSON,
        MERGED_PR_VIEW_JSON,
        false,
        all_checks,
        "[]",
    )
}

fn write_zero_required_checks_transition_stub(
    stub: &StubEnv,
    initial_all_checks: &str,
    later_all_checks: &str,
) -> PathBuf {
    write_chain_stub_with_check_transition(
        stub,
        FULL_PR_VIEW_JSON,
        MERGED_PR_VIEW_JSON,
        false,
        initial_all_checks,
        "[]",
        Some(later_all_checks),
    )
}

fn run_zero_required_delivery(
    stub: StubEnv,
    repo_path: &Path,
    all_checks: &str,
    timeout: &str,
    no_merge: bool,
) -> (StubEnv, CmdOutput) {
    let gh_path = write_zero_required_checks_stub(&stub, all_checks);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());
    let mut args = vec![
        "--provider",
        "github",
        "--format",
        "json",
        "pr",
        "deliver",
        "--kind",
        "feature",
        "--title",
        "feat: wait for visible checks",
        "--body",
        "## Summary\n\nWait for visible checks.\n\n## Test plan\n\nVerified.\n",
        "--head",
        "feat/sample",
        "--base",
        "main",
        "--timeout",
        timeout,
    ];
    if no_merge {
        args.push("--no-merge");
    }
    let output = run_in_repo(&stub, repo_path, &args);
    (stub, output)
}

/// Same delivery, declaring up front that this repository configures no checks.
fn run_zero_required_delivery_allowing_no_checks(
    stub: StubEnv,
    repo_path: &Path,
    all_checks: &str,
    timeout: &str,
) -> (StubEnv, CmdOutput) {
    let gh_path = write_zero_required_checks_stub(&stub, all_checks);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());
    let output = run_in_repo(
        &stub,
        repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: wait for visible checks",
            "--body",
            "## Summary\n\nWait for visible checks.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            timeout,
            "--allow-no-checks",
            "--allow-no-checks-reason",
            "this fixture repository configures no checks at all",
            "--no-merge",
        ],
    );
    (stub, output)
}

fn assert_no_all_check_call_after_required(calls: &str) {
    let required_index = calls
        .lines()
        .position(|line| line.contains("--required"))
        .expect("required-only classification call");
    assert!(
        calls
            .lines()
            .skip(required_index + 1)
            .all(|line| line.contains("--required")),
        "terminal retained snapshots must not trigger a later all-check request"
    );
}

fn write_chain_stub_with_checks(
    stub: &StubEnv,
    pre_view: &str,
    post_view: &str,
    merge_exits_one: bool,
    all_checks: &str,
    required_checks: &str,
) -> PathBuf {
    write_chain_stub_with_check_transition(
        stub,
        pre_view,
        post_view,
        merge_exits_one,
        all_checks,
        required_checks,
        None,
    )
}

fn write_chain_stub_with_check_transition(
    stub: &StubEnv,
    pre_view: &str,
    post_view: &str,
    merge_exits_one: bool,
    all_checks: &str,
    required_checks: &str,
    later_all_checks: Option<&str>,
) -> PathBuf {
    let sentinel = stub.tempdir.path().join("merge-called");
    let merge_args = stub.tempdir.path().join("merge-args");
    let checks_calls = stub.tempdir.path().join("checks-calls");
    let review_response = stub.tempdir.path().join("review-response.json");
    let thread_head_sha = serde_json::from_str::<serde_json::Value>(pre_view)
        .ok()
        .and_then(|value| value["headRefOid"].as_str().map(str::to_string))
        .unwrap_or_else(|| "head123".to_string());
    let review_state_node = clean_review_loop_comment("sympoies/nils-cli", 123, &thread_head_sha);
    let merge_branch = if merge_exits_one {
        format!(
            "    echo \"$*\" > {merge_args}\n    touch {sentinel}\n    echo 'X stderr warning after merge' >&2\n    exit 1\n",
            sentinel = sentinel.display(),
            merge_args = merge_args.display(),
        )
    } else {
        format!(
            "    echo \"$*\" > {merge_args}\n    touch {sentinel}\n    :\n",
            sentinel = sentinel.display(),
            merge_args = merge_args.display(),
        )
    };
    let all_checks_branch = match later_all_checks {
        Some(later) => format!(
            r#"        if [ "$(grep -vc -- '--required' {checks_calls})" -ge 2 ]; then
          cat <<'EOF'
{later}
EOF
        else
          cat <<'EOF'
{all_checks}
EOF
        fi"#,
            checks_calls = checks_calls.display(),
            later = later,
            all_checks = all_checks,
        ),
        None => format!(
            r#"        cat <<'EOF'
{all_checks}
EOF"#,
            all_checks = all_checks,
        ),
    };
    let body = format!(
        r###"#!/bin/sh
set -e
case "$1 $2" in
  "auth status")
    cat <<'EOF' 1>&2
github.com
  ✓ Logged in to github.com account testuser-gh (keyring)
  - Token scopes: 'repo', 'read:org'
EOF
    ;;
  "repo view")
    cat <<'EOF'
{{
  "name": "nils-cli",
  "owner": {{ "login": "sympoies" }},
  "url": "https://github.com/sympoies/nils-cli",
  "defaultBranchRef": {{ "name": "main" }},
  "mergeCommitAllowed": false,
  "squashMergeAllowed": true,
  "rebaseMergeAllowed": false
}}
EOF
    ;;
  "pr list")
    echo '[]'
    ;;
  "pr create")
    cat <<'EOF'
{create}
EOF
    ;;
  "pr checks")
    echo "$*" >> {checks_calls}
    case "$*" in
      *"--required"*)
        cat <<'EOF'
{required_checks}
EOF
        ;;
      *)
{all_checks_branch}
        ;;
    esac
    ;;
  "pr ready")
    :
    ;;
  "api graphql")
    case "$*" in
      *"authorAssociation body createdAt"*)
        printf '%s\n' '{{"data":{{"viewer":{{"login":"testuser-gh"}},"repository":{{"pullRequest":{{"comments":{{"nodes":[{review_state_node}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
      *"reviews(first:"*)
        if [ ! -f {review_response} ]; then
          echo "stub: unexpected native-review query" >&2
          exit 99
        fi
        cat {review_response}
        ;;
      *)
        # Merge lock-down review-thread sweep; all resolved.
        cat <<'EOF'
{{ "data": {{ "repository": {{ "pullRequest": {{ "headRefOid": "{thread_head_sha}", "reviewThreads": {{ "nodes": [], "pageInfo": {{ "hasNextPage": false, "endCursor": null }} }} }} }} }} }}
EOF
        ;;
    esac
    ;;
  "pr merge")
{merge_branch}
    ;;
  "pr view")
    # Distinguish the post-merge merge_sha view (--json mergeCommit) from
    # regular view; for the latter, switch to the merged-state JSON once
    # the merge step has been observed via the sentinel file.
    case "$*" in
      *"--json mergeCommit"*)
        cat <<'EOF'
{{ "mergeCommit": {{ "oid": "abc123def456" }} }}
EOF
        ;;
      *)
        if [ -e {sentinel} ]; then
          cat <<'EOF'
{post_view}
EOF
        else
          cat <<'EOF'
{pre_view}
EOF
        fi
        ;;
    esac
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"###,
        create = FIXTURE_CREATE_STDOUT,
        pre_view = pre_view,
        post_view = post_view,
        all_checks_branch = all_checks_branch,
        required_checks = required_checks,
        checks_calls = checks_calls.display(),
        review_response = review_response.display(),
        review_state_node = review_state_node,
        merge_branch = merge_branch,
        sentinel = sentinel.display(),
    );
    let path = stub.tempdir.path().join("gh");
    fs::write(&path, body).expect("write gh stub");
    let mut perm = fs::metadata(&path).expect("metadata").permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).expect("chmod");
    path
}

/// Chain stub for the adopt path: `pr list` reports an existing open PR for
/// the head branch, and `pr create` is a tripwire — it touches the
/// `create-called` sentinel and exits 99 so any create attempt fails the
/// test loudly. `pr view` serves `pre_view` until the merge sentinel
/// appears, then flips to the merged payload.
fn write_adopt_chain_stub(stub: &StubEnv, pre_view: &str) -> PathBuf {
    let merge_sentinel = stub.tempdir.path().join("merge-called");
    let ready_sentinel = stub.tempdir.path().join("ready-called");
    let create_sentinel = stub.tempdir.path().join("create-called");
    let merge_args = stub.tempdir.path().join("merge-args");
    let head_sha = serde_json::from_str::<serde_json::Value>(pre_view)
        .ok()
        .and_then(|value| value["headRefOid"].as_str().map(str::to_string));
    let ready_view = head_sha
        .as_deref()
        .map(|sha| view_with_head_oid(ADOPTED_READY_PR_VIEW_JSON, sha))
        .unwrap_or_else(|| ADOPTED_READY_PR_VIEW_JSON.to_string());
    let merged_view = head_sha
        .as_deref()
        .map(|sha| view_with_head_oid(MERGED_PR_VIEW_JSON, sha))
        .unwrap_or_else(|| MERGED_PR_VIEW_JSON.to_string());
    let thread_head_sha = head_sha.as_deref().unwrap_or("head123");
    let review_state_node = clean_review_loop_comment("sympoies/nils-cli", 123, thread_head_sha);
    let body = format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "auth status")
    cat <<'EOF' 1>&2
github.com
  ✓ Logged in to github.com account testuser-gh (keyring)
  - Token scopes: 'repo', 'read:org'
EOF
    ;;
  "repo view")
    cat <<'EOF'
{{
  "name": "nils-cli",
  "owner": {{ "login": "sympoies" }},
  "url": "https://github.com/sympoies/nils-cli",
  "defaultBranchRef": {{ "name": "main" }},
  "mergeCommitAllowed": false,
  "squashMergeAllowed": true,
  "rebaseMergeAllowed": false
}}
EOF
    ;;
  "pr list")
    cat <<'EOF'
{list}
EOF
    ;;
  "pr create")
    touch {create_sentinel}
    echo "stub: pr create must not run on the adopt path" >&2
    exit 99
    ;;
  "pr checks")
    cat <<'EOF'
{checks}
EOF
    ;;
  "pr ready")
    touch {ready_sentinel}
    ;;
  "api graphql")
    case "$*" in
      *"authorAssociation body createdAt"*)
        printf '%s\n' '{{"data":{{"viewer":{{"login":"testuser-gh"}},"repository":{{"pullRequest":{{"comments":{{"nodes":[{review_state_node}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
      *)
        cat <<'EOF'
{{ "data": {{ "repository": {{ "pullRequest": {{ "headRefOid": "{thread_head_sha}", "reviewThreads": {{ "nodes": [], "pageInfo": {{ "hasNextPage": false, "endCursor": null }} }} }} }} }} }}
EOF
        ;;
    esac
    ;;
  "pr merge")
    echo "$*" > {merge_args}
    touch {merge_sentinel}
    ;;
  "pr view")
    case "$*" in
      *"--json mergeCommit"*)
        cat <<'EOF'
{{ "mergeCommit": {{ "oid": "abc123def456" }} }}
EOF
        ;;
      *)
        if [ -e {merge_sentinel} ]; then
          cat <<'EOF'
{merged}
EOF
        elif [ -e {ready_sentinel} ]; then
          cat <<'EOF'
{ready_view}
EOF
        else
          cat <<'EOF'
{pre_view}
EOF
        fi
        ;;
    esac
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
        list = OPEN_PR_LIST_JSON,
        checks = FIXTURE_CHECKS_JSON,
        merged = merged_view,
        ready_view = ready_view,
        pre_view = pre_view,
        create_sentinel = create_sentinel.display(),
        ready_sentinel = ready_sentinel.display(),
        merge_sentinel = merge_sentinel.display(),
        merge_args = merge_args.display(),
        review_state_node = review_state_node,
    );
    let path = stub.tempdir.path().join("gh");
    fs::write(&path, body).expect("write gh stub");
    let mut perm = fs::metadata(&path).expect("metadata").permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).expect("chmod");
    path
}

fn write_qualified_head_collision_stub(stub: &StubEnv, head_sha: &str) -> PathBuf {
    let create_sentinel = stub.tempdir.path().join("qualified-create-called");
    let list_args = stub.tempdir.path().join("qualified-list-args");
    let body = format!(
        r###"#!/bin/sh
set -e
case "$1 $2" in
  "auth status")
    printf '%s\n' 'github.com' 1>&2
    ;;
  "repo view")
    cat <<'EOF'
{{
  "name": "nils-cli",
  "owner": {{ "login": "sympoies" }},
  "url": "https://github.com/sympoies/nils-cli",
  "defaultBranchRef": {{ "name": "main" }},
  "mergeCommitAllowed": false,
  "squashMergeAllowed": true,
  "rebaseMergeAllowed": false
}}
EOF
    ;;
  "pr list")
    printf '%s\n' "$*" > {list_args}
    cat <<'EOF'
[{{"number":123,"url":"https://github.com/sympoies/nils-cli/pull/123","state":"OPEN","title":"feat: other fork","headRefName":"feat/sample","headRepository":{{"nameWithOwner":"other-user/nils-cli"}},"author":{{"login":"other-user"}}}}]
EOF
    ;;
  "pr create")
    touch {create_sentinel}
    printf '%s\n' 'https://github.com/sympoies/nils-cli/pull/124'
    ;;
  "pr view")
    case "$3" in
      124)
        cat <<'EOF'
{{"number":124,"url":"https://github.com/sympoies/nils-cli/pull/124","state":"OPEN","isDraft":true,"title":"feat: qualified fork","headRefName":"feat/sample","headRefOid":"{head_sha}","headRepository":{{"nameWithOwner":"fork-owner/nils-cli"}},"baseRefName":"main","mergeable":"MERGEABLE","mergedAt":null,"labels":[],"body":"## Summary\n\nCreated.\n\n## Test plan\n\nVerified.\n"}}
EOF
        ;;
      *)
        cat <<'EOF'
{{"number":123,"url":"https://github.com/sympoies/nils-cli/pull/123","state":"OPEN","isDraft":true,"title":"feat: other fork","headRefName":"feat/sample","headRefOid":"{head_sha}","headRepository":{{"nameWithOwner":"other-user/nils-cli"}},"baseRefName":"main","mergeable":"MERGEABLE","mergedAt":null,"labels":[],"body":"## Summary\n\nWrong fork.\n\n## Test plan\n\nWrong fork.\n"}}
EOF
        ;;
    esac
    ;;
  "pr checks")
    cat <<'EOF'
{checks}
EOF
    ;;
  *)
    echo "stub: unexpected gh args" >&2
    exit 99
    ;;
esac
"###,
        checks = FIXTURE_CHECKS_JSON,
        create_sentinel = create_sentinel.display(),
        list_args = list_args.display(),
    );
    stub.write_stub("gh", &body)
}

/// Full-chain stub whose post-merge `pr view` reports a closing-keyword
/// linked issue, plus `issue view` / `issue close` handlers so the
/// deterministic closeout step can run end to end. `issue_view_json` selects
/// whether the linked issue is still open (closeout must close it) or already
/// closed (closeout must skip it). `issue close` touches an
/// `issue-close-called` sentinel so tests can assert whether the backend close
/// ran; when `close_exits_one` it also exits non-zero to exercise the
/// best-effort failure path.
fn write_closeout_chain_stub(
    stub: &StubEnv,
    issue_view_json: &str,
    close_exits_one: bool,
) -> PathBuf {
    let merge_sentinel = stub.tempdir.path().join("merge-called");
    let close_sentinel = stub.tempdir.path().join("issue-close-called");
    let close_action = if close_exits_one {
        "    echo 'gh: could not close issue' >&2\n    exit 1\n"
    } else {
        "    :\n"
    };
    let thread_head_sha = "head123";
    let body = format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "auth status")
    cat <<'EOF' 1>&2
github.com
  ✓ Logged in to github.com account testuser-gh (keyring)
  - Token scopes: 'repo', 'read:org'
EOF
    ;;
  "repo view")
    cat <<'EOF'
{{
  "name": "nils-cli",
  "owner": {{ "login": "sympoies" }},
  "url": "https://github.com/sympoies/nils-cli",
  "defaultBranchRef": {{ "name": "main" }},
  "mergeCommitAllowed": false,
  "squashMergeAllowed": true,
  "rebaseMergeAllowed": false
}}
EOF
    ;;
  "pr list")
    echo '[]'
    ;;
  "pr create")
    cat <<'EOF'
{create}
EOF
    ;;
  "pr checks")
    cat <<'EOF'
{checks}
EOF
    ;;
  "pr ready")
    :
    ;;
  "api graphql")
    cat <<'EOF'
{{ "data": {{ "repository": {{ "pullRequest": {{ "headRefOid": "{thread_head_sha}", "reviewThreads": {{ "nodes": [], "pageInfo": {{ "hasNextPage": false, "endCursor": null }} }} }} }} }} }}
EOF
    ;;
  "pr merge")
    touch {merge_sentinel}
    :
    ;;
  "issue view")
    cat <<'EOF'
{issue_view}
EOF
    ;;
  "issue close")
    touch {close_sentinel}
{close_action}
    ;;
  "pr view")
    case "$*" in
      *"--json mergeCommit"*)
        cat <<'EOF'
{{ "mergeCommit": {{ "oid": "abc123def456" }} }}
EOF
        ;;
      *)
        if [ -e {merge_sentinel} ]; then
          cat <<'EOF'
{merged_closing}
EOF
        else
          cat <<'EOF'
{pre_view}
EOF
        fi
        ;;
    esac
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
        create = FIXTURE_CREATE_STDOUT,
        checks = FIXTURE_CHECKS_JSON,
        merged_closing = MERGED_PR_VIEW_WITH_CLOSING_JSON,
        pre_view = FULL_PR_VIEW_JSON,
        issue_view = issue_view_json,
        close_action = close_action,
        merge_sentinel = merge_sentinel.display(),
        close_sentinel = close_sentinel.display(),
    );
    let path = stub.tempdir.path().join("gh");
    fs::write(&path, body).expect("write gh stub");
    let mut perm = fs::metadata(&path).expect("metadata").permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).expect("chmod");
    path
}

fn run_in_repo(stub: &StubEnv, repo: &Path, args: &[&str]) -> CmdOutput {
    run_forge_cli_in(stub, args, Some(repo))
}

#[test]
fn pr_deliver_full_chain_no_merge_emits_four_steps_and_returns_success() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let gh_path = write_full_chain_stub(&stub);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nLand the new feature.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
            "--no-merge",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.pr.deliver.v1");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["kind"], "feature");
    assert_eq!(envelope["data"]["provider"], "github");
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        steps,
        vec!["auth_status", "repo_view", "create", "wait_checks"],
        "no-merge must stop after wait_checks"
    );
    assert_eq!(envelope["data"]["pr"]["merged"], false);
    assert!(envelope["data"]["pr"]["merge_sha"].is_null());
    assert_eq!(envelope["data"]["pr"]["number"], 123);
}

#[test]
fn pr_deliver_strict_labels_valid_catalog_passes_live_delivery() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");
    let (_catalog_tempdir, catalog) = write_label_catalog();

    let stub = StubEnv::new();
    let gh_path = write_full_chain_stub(&stub);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nLand the new feature.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--label",
            "type::feature",
            "--label-catalog",
            &catalog,
            "--strict-labels",
            "--timeout",
            "5s",
            "--no-merge",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["steps"][2]["step"], "create");
}

#[test]
fn pr_deliver_strict_labels_catalog_is_not_reopened_after_provider_reads() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");
    let (_catalog_tempdir, catalog) = write_label_catalog();

    let stub = StubEnv::new();
    let gh_path = write_catalog_mutating_chain_stub(&stub, Path::new(&catalog));
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nLand the new feature.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--label",
            "type::feature",
            "--label-catalog",
            &catalog,
            "--strict-labels",
            "--timeout",
            "5s",
            "--no-merge",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    assert!(
        !Path::new(&catalog).exists(),
        "catalog mutation tripwire did not run"
    );
}

#[test]
fn pr_deliver_prevalidated_labels_keep_the_initial_provider_context() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");
    let (_catalog_tempdir, catalog) = write_pr_only_label_catalog();

    let stub = StubEnv::new();
    let gh_path = write_provider_mutating_chain_stub(&stub, &repo_path);
    let stub = stub
        .env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy())
        .glab_stub("#!/bin/sh\necho 'glab must not run after GitHub preflight' >&2\nexit 97\n");

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nLand the new feature.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--label",
            "type::feature",
            "--label-catalog",
            &catalog,
            "--strict-labels",
            "--timeout",
            "5s",
            "--no-merge",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["data"]["provider"], "github");
    assert_eq!(
        git_output(&repo_path, &["remote", "get-url", "origin"]),
        "https://gitlab.com/sympoies/nils-cli.git",
        "provider mutation tripwire did not run"
    );
}

#[test]
fn pr_deliver_zero_required_pending_visible_checks_time_out_before_merge() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let merge_sentinel = stub.tempdir.path().join("merge-called");
    let (_stub, out) =
        run_zero_required_delivery(stub, &repo_path, FIXTURE_PENDING_CHECKS_JSON, "0s", false);

    assert_eq!(out.code, 69, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "checks_timeout");
    assert!(
        !merge_sentinel.exists(),
        "pending visible checks must block merge"
    );
}

#[test]
fn pr_deliver_zero_required_successful_visible_checks_use_all_check_fallback() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let checks_calls = stub.tempdir.path().join("checks-calls");
    let (_stub, out) =
        run_zero_required_delivery(stub, &repo_path, FIXTURE_CHECKS_JSON, "5s", true);

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let calls = fs::read_to_string(checks_calls).expect("checks call log");
    assert_no_all_check_call_after_required(&calls);
}

#[test]
fn pr_deliver_zero_required_pending_visible_checks_eventually_succeed() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let checks_calls = stub.tempdir.path().join("checks-calls");
    let gh_path = write_zero_required_checks_transition_stub(
        &stub,
        FIXTURE_PENDING_CHECKS_JSON,
        FIXTURE_CHECKS_JSON,
    );
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());
    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: wait for visible checks",
            "--body",
            "## Summary\n\nWait for visible checks.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
            "--no-merge",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["data"]["steps"][3]["payload"]["state"], "success");
    let calls = fs::read_to_string(checks_calls).expect("checks call log");
    let required_index = calls
        .lines()
        .position(|line| line.contains("--required"))
        .expect("required-only classification call");
    assert!(
        calls
            .lines()
            .skip(required_index + 1)
            .any(|line| !line.contains("--required")),
        "a pending retained snapshot must continue with all-check polling"
    );
}

#[test]
fn pr_deliver_zero_required_failed_visible_check_blocks_merge() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let merge_sentinel = stub.tempdir.path().join("merge-called");
    let (_stub, out) =
        run_zero_required_delivery(stub, &repo_path, FIXTURE_FAILED_CHECKS_JSON, "5s", false);

    assert_eq!(out.code, 1, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "checks_failed");
    assert!(
        !merge_sentinel.exists(),
        "failed visible checks must block merge"
    );
}

/// Contract change: this used to complete immediately with exit 0.
///
/// Zero required checks *and* zero visible rows means the provider reported
/// nothing at all for this head, which is indistinguishable from a repository
/// that configures no checks — and identical to the check-registration window
/// after a force-with-lease, where delivery previously reported success against
/// a head no CI had touched. The default now waits out the budget and fails
/// closed; a repository that genuinely has no checks says so explicitly.
#[test]
fn pr_deliver_zero_required_and_zero_visible_checks_are_refused() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let (_stub, out) =
        run_zero_required_delivery(stub, &repo_path, FIXTURE_EMPTY_CHECKS_JSON, "0s", true);

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "checks_not_registered");
}

#[test]
fn pr_deliver_zero_required_and_zero_visible_checks_complete_when_explicitly_allowed() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let checks_calls = stub.tempdir.path().join("checks-calls");
    let (_stub, out) = run_zero_required_delivery_allowing_no_checks(
        stub,
        &repo_path,
        FIXTURE_EMPTY_CHECKS_JSON,
        "5s",
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let calls = fs::read_to_string(checks_calls).expect("checks call log");
    assert_no_all_check_call_after_required(&calls);
}

#[test]
fn pr_deliver_head_flag_uses_named_branch_push_state() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");
    git(&repo_path, &["checkout", "-q", "main"]);

    let stub = StubEnv::new();
    let gh_path = write_full_chain_stub(&stub);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nLand the new feature.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
            "--no-merge",
        ],
    );
    assert_eq!(
        out.code, 0,
        "explicit --head branch is pushed even when current HEAD lacks upstream; stdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], true);
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        steps,
        vec!["auth_status", "repo_view", "create", "wait_checks"]
    );
}

#[test]
fn pr_deliver_full_chain_emits_all_seven_steps_with_merge_sha() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let gh_path = write_full_chain_stub(&stub);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nLand the new feature.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        steps,
        vec![
            "auth_status",
            "repo_view",
            "create",
            "wait_checks",
            "ready",
            "merge",
            "issue_closeout",
        ],
        "full chain must include all seven steps in order"
    );
    assert_eq!(envelope["data"]["pr"]["merged"], true);
    assert_eq!(envelope["data"]["pr"]["merge_sha"], "abc123def456");
    // The merged PR has no closing-keyword references, so closeout is a
    // no-op that still reports ok with an empty issue list.
    let closeout = &envelope["data"]["steps"][6];
    assert_eq!(closeout["ok"], true);
    assert_eq!(closeout["payload"]["issues"].as_array().unwrap().len(), 0);
    // Every step carries its atom's schema literal.
    let schemas: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["schema_version"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        schemas,
        vec![
            "cli.forge-cli.auth.status.v1",
            "cli.forge-cli.repo.view.v1",
            "cli.forge-cli.pr.create.v1",
            "cli.forge-cli.pr.checks.v1",
            "cli.forge-cli.pr.ready.v1",
            "cli.forge-cli.pr.merge.v1",
            "cli.forge-cli.issue.closeout.v1",
        ]
    );
}

#[test]
fn pr_deliver_enforces_review_convergence_and_preserves_error_detail() {
    let repo_tmp = make_git_repo();
    let repo_path = repo_tmp.path().join("repo");
    let stub = StubEnv::new();
    fs::write(
        stub.tempdir.path().join("review-response.json"),
        r#"{"data":{"repository":{"pullRequest":{"headRefOid":"head123","reviews":{"nodes":[{"id":"PRR_blocking","databaseId":9,"url":"https://github.com/sympoies/nils-cli/pull/123#pullrequestreview-9","author":{"login":"blocking-reviewer"},"state":"CHANGES_REQUESTED","commit":{"oid":"head123"},"submittedAt":"2026-07-14T04:00:00Z","body":"changes requested"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}"#,
    )
    .expect("review response");
    let pre_view = view_with_head_oid(FULL_PR_VIEW_JSON, "head123");
    let post_view = view_with_head_oid(MERGED_PR_VIEW_JSON, "head123");
    let gh_path = write_chain_stub(&stub, &pre_view, &post_view, false);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());
    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: review convergence",
            "--body",
            "## Summary\n\nReview.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--review-convergence",
            "--timeout",
            "5s",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "review_changes_requested");
    assert!(
        envelope["error"]["details"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("blocking-reviewer"))
    );
    assert!(
        !stub.tempdir.path().join("merge-called").exists(),
        "blocking review must prevent provider merge"
    );
}

#[test]
fn pr_deliver_gate_enabled_create_preserves_attested_head_through_merge_cas() {
    let (fixture, evidence, head) = make_subject_bound_git_repo();
    let repo_path = fixture.repo.clone();
    let pre_view = view_with_head_oid(FULL_PR_VIEW_JSON, &head);
    let post_view = view_with_head_oid(MERGED_PR_VIEW_JSON, &head);

    let stub = StubEnv::new();
    let merge_args = stub.tempdir.path().join("merge-args");
    let gh_path = write_chain_stub(&stub, &pre_view, &post_view, false);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());
    let evidence_arg = evidence.to_string_lossy().to_string();
    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: subject-bound delivery",
            "--body",
            "## Summary\n\nBound.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--test-first-evidence",
            &evidence_arg,
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let args = fs::read_to_string(merge_args).expect("merge args");
    assert!(
        args.contains(&format!("--match-head-commit {head}")),
        "merge CAS missing attested head: {args}"
    );
}

#[test]
fn pr_deliver_gate_enabled_adopt_preserves_attested_head_through_merge_cas() {
    let (fixture, evidence, head) = make_subject_bound_git_repo();
    let repo_path = fixture.repo.clone();
    let pre_view = view_with_head_oid(ADOPTABLE_PR_VIEW_JSON, &head);

    let stub = StubEnv::new();
    let merge_args = stub.tempdir.path().join("merge-args");
    let create_called = stub.tempdir.path().join("create-called");
    let gh_path = write_adopt_chain_stub(&stub, &pre_view);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());
    let evidence_arg = evidence.to_string_lossy().to_string();
    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: subject-bound delivery",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--test-first-evidence",
            &evidence_arg,
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    assert!(
        !create_called.exists(),
        "adopt path must not create another PR"
    );
    let args = fs::read_to_string(merge_args).expect("merge args");
    assert!(
        args.contains(&format!("--match-head-commit {head}")),
        "merge CAS missing attested head: {args}"
    );
}

struct LocalAdoptRunner {
    head: String,
}

fn local_deliver_args(evidence: &Path) -> PrDeliverArgs {
    PrDeliverArgs {
        kind: PrKindFlag::Feature,
        title: "feat: subject-bound local delivery".into(),
        body: Some("## Summary\n\nBound.\n\n## Test plan\n\nVerified.\n".into()),
        body_file: None,
        head: Some("feat/sample".into()),
        base: Some("main".into()),
        method: MergeMethodFlag::Squash,
        reviewers: Vec::new(),
        labels: Vec::new(),
        label_catalog: None,
        strict_labels: false,
        test_first_evidence: Some(evidence.to_string_lossy().to_string()),
        timeout: std::time::Duration::from_secs(5),
        no_merge: true,
        no_issue_closeout: false,
        allow_non_default_base: false,
        allow_unresolved_threads: false,
        allow_unresolved_threads_reason: None,
        review_convergence: None,
        allow_unchecked_tasks: false,
        allow_unchecked_tasks_reason: None,
        allow_no_checks: false,
        allow_no_checks_reason: None,
    }
}

impl BackendRunner for LocalAdoptRunner {
    fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
        let argv: Vec<String> = call
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let (stdout, stderr) = match (
            argv.first().map(String::as_str),
            argv.get(1).map(String::as_str),
        ) {
            (Some("auth"), Some("status")) => (
                String::new(),
                "local\n  ✓ Logged in to local account tester (test)\n".to_string(),
            ),
            (Some("repo"), Some("view")) => (
                serde_json::json!({
                    "name": "nils-cli",
                    "owner": {"login": "sympoies"},
                    "defaultBranchRef": {"name": "main"},
                    "mergeCommitAllowed": true,
                    "squashMergeAllowed": true,
                    "rebaseMergeAllowed": true,
                    "url": "https://github.com/sympoies/nils-cli"
                })
                .to_string(),
                String::new(),
            ),
            (Some("pr"), Some("list")) => (
                serde_json::json!([{
                    "number": 123,
                    "url": "local://sympoies/nils-cli/pull/123",
                    "state": "OPEN",
                    "title": "feat: subject-bound local delivery",
                    "headRefName": "feat/sample",
                    "author": {"login": "tester"}
                }])
                .to_string(),
                String::new(),
            ),
            (Some("pr"), Some("view")) => (
                serde_json::json!({
                    "number": 123,
                    "url": "local://sympoies/nils-cli/pull/123",
                    "state": "OPEN",
                    "isDraft": true,
                    "title": "feat: subject-bound local delivery",
                    "headRefName": "feat/sample",
                    "headRefOid": self.head,
                    "baseRefName": "main",
                    "mergeable": "MERGEABLE",
                    "mergedAt": null,
                    "labels": [],
                    "body": "## Summary\n\nBound.\n\n## Test plan\n\nVerified.\n"
                })
                .to_string(),
                String::new(),
            ),
            _ => panic!("unexpected local adopt backend call: {argv:?}"),
        };
        Ok(BackendSuccess { stdout, stderr })
    }
}

#[test]
fn pr_deliver_local_adopt_uses_remote_identity_for_bound_evidence() {
    let (fixture, evidence, head) = make_subject_bound_git_repo();
    let repo_path = fixture.repo.clone();
    let store = fixture.root.join("store");
    fs::create_dir_all(store.join("prs")).unwrap();
    fs::write(
        store.join("prs/123.json"),
        r#"{"number":123,"state":"OPEN","merged":false,"merge_sha":null,"checks":"success","required_state":"success","required_count":1,"non_required_failures":[]}"#,
    )
    .unwrap();

    let global = GlobalFlags {
        format: Some(OutputFormat::Json),
        remote: "origin".into(),
        provider: Some(ProviderFlag::Local),
        host: None,
        repo: None,
        store_root: Some(store),
        dry_run: false,
    };
    let code = pr_deliver::run_with(
        &LocalAdoptRunner { head },
        &SystemClock,
        &global,
        local_deliver_args(&evidence),
        OutputFormat::Json,
        &repo_path,
    )
    .expect("remote-bound evidence should pass the local adopt path");
    assert_eq!(code, nils_common::cli_contract::exit::SUCCESS);
}

#[test]
fn pr_deliver_short_circuits_when_pr_create_validation_fails_with_data_65() {
    // Title over 70 chars trips pr.create's title_length validation. The
    // macro must surface DATA 65 (not a remapped runtime code) and omit
    // later steps (wait_checks / ready / merge) from data.steps[].
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let gh_path = write_full_chain_stub(&stub);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let long_title = "a".repeat(71);
    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            &long_title,
            "--body",
            "## Summary\n\nx\n\n## Test plan\n\ny\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
        ],
    );
    assert_eq!(
        out.code, 65,
        "expected DATA 65 on title_too_long, stderr={}",
        out.stderr
    );
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "title_too_long");
    // Only auth_status + repo_view ran before the validation failed.
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(steps, vec!["auth_status", "repo_view"]);
}

#[test]
fn pr_deliver_github_merge_step_blocks_on_unchecked_task_items() {
    // Merge lock-down rule 14 on the GitHub path: the merge step parses the
    // PR body fetched via `pr view --json …,body` and must fail closed with
    // unchecked_task_items before the backend `pr merge` call runs.
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let sentinel = stub.tempdir.path().join("merge-called");
    let gh_path = write_chain_stub(
        &stub,
        UNCHECKED_TASKS_PR_VIEW_JSON,
        MERGED_PR_VIEW_JSON,
        false,
    );
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nx\n\n## Test plan\n\n- [x] unit\n- [ ] run e2e suite\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(
        out.code, 65,
        "expected DATA 65 on unchecked task items, stdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "unchecked_task_items");
    assert!(
        !sentinel.exists(),
        "backend merge must not run when the task-list gate blocks"
    );
}

#[test]
fn pr_deliver_treats_gh_exit_1_after_successful_merge_as_success() {
    // Regression: `gh pr merge` can return exit 1 even after the API merge
    // call succeeds (typically a branch-cleanup race or post-merge stderr
    // warning treated as failure). The pr.merge atom should re-check the
    // PR state and treat the chain as successful when GitHub reports the
    // PR as merged, so `forge-cli pr deliver` does not surface a phantom
    // `backend_error: gh exited with status 1` after the PR is actually on
    // main.
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let gh_path = write_chain_stub_with_merge_exit(
        &stub,
        FULL_PR_VIEW_JSON,
        MERGED_PR_VIEW_JSON,
        /*merge_exits_one=*/ true,
    );
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nLand the new feature.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(
        out.code, 0,
        "expected success when gh exits 1 but PR is merged; stdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], true);
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        steps,
        vec![
            "auth_status",
            "repo_view",
            "create",
            "wait_checks",
            "ready",
            "merge",
            "issue_closeout",
        ],
        "merge step must still be recorded as ok=true after recovery"
    );
    assert_eq!(envelope["data"]["pr"]["merged"], true);
    assert_eq!(envelope["data"]["pr"]["merge_sha"], "abc123def456");
}

#[test]
fn pr_deliver_rejects_idempotent_merge_recovery_for_a_different_head() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");
    let pre_view = view_with_head_oid(
        FULL_PR_VIEW_JSON,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    let post_view = view_with_head_oid(
        MERGED_PR_VIEW_JSON,
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    );

    let stub = StubEnv::new();
    let gh_path = write_chain_stub_with_merge_exit(
        &stub, &pre_view, &post_view, /*merge_exits_one=*/ true,
    );
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());
    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nLand.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(
        envelope["error"]["code"],
        "test_first_evidence_provider_head_mismatch"
    );
}

#[test]
fn pr_deliver_adopts_existing_open_pr_for_head_branch() {
    // Regression for the create-then-deliver dead end: a draft PR opened
    // earlier via `pr create` could never be finished by `pr deliver`
    // because the macro validated its create-step inputs (body gate) before
    // looking up the head branch. The adopt path must find the open PR,
    // skip create entirely, and run the remaining lifecycle steps — even
    // when the invocation carries no `--body`.
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let create_sentinel = stub.tempdir.path().join("create-called");
    let gh_path = write_adopt_chain_stub(&stub, ADOPTABLE_PR_VIEW_JSON);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], true);
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        steps,
        vec![
            "auth_status",
            "repo_view",
            "adopt",
            "wait_checks",
            "ready",
            "merge",
            "issue_closeout",
        ],
        "adopt must replace create and continue the lifecycle"
    );
    assert_eq!(envelope["data"]["steps"][2]["ok"], true);
    assert_eq!(
        envelope["data"]["steps"][2]["schema_version"], "cli.forge-cli.pr.view.v1",
        "adopt step payload is the adopted PR's view"
    );
    assert_eq!(envelope["data"]["pr"]["number"], 123);
    assert_eq!(envelope["data"]["pr"]["merged"], true);
    assert_eq!(envelope["data"]["pr"]["merge_sha"], "abc123def456");
    assert!(
        !create_sentinel.exists(),
        "adopt path must never call the backend pr create"
    );
}

#[test]
fn pr_deliver_github_qualified_head_does_not_adopt_same_branch_from_another_user() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");
    git(
        &repo_path,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/fork-owner/nils-cli.git",
        ],
    );
    let head_sha = git_output(&repo_path, &["rev-parse", "feat/sample"]);

    let stub = StubEnv::new();
    let create_sentinel = stub.tempdir.path().join("qualified-create-called");
    let list_args = stub.tempdir.path().join("qualified-list-args");
    let gh_path = write_qualified_head_collision_stub(&stub, &head_sha);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--repo",
            "sympoies/nils-cli",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: qualified fork",
            "--body",
            "## Summary\n\nCreated.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "fork-owner:feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
            "--no-merge",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    let steps = envelope["data"]["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .map(|step| step["step"].as_str().unwrap_or(""))
        .collect::<Vec<_>>();
    assert!(steps.contains(&"create"), "steps={steps:?}");
    assert!(!steps.contains(&"adopt"), "steps={steps:?}");
    assert!(
        create_sentinel.exists(),
        "the exact qualified head must be created"
    );
    let lookup = fs::read_to_string(list_args).expect("list args");
    assert!(
        !lookup.contains("fork-owner:feat/sample"),
        "gh pr list must not receive unsupported qualified --head syntax: {lookup}"
    );
}

#[test]
fn pr_deliver_adopt_head_flag_uses_named_branch_push_state() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");
    git(&repo_path, &["checkout", "-q", "main"]);

    let stub = StubEnv::new();
    let create_sentinel = stub.tempdir.path().join("create-called");
    let gh_path = write_adopt_chain_stub(&stub, ADOPTABLE_PR_VIEW_JSON);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], true);
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        steps,
        vec![
            "auth_status",
            "repo_view",
            "adopt",
            "wait_checks",
            "ready",
            "merge",
            "issue_closeout",
        ]
    );
    assert!(
        !create_sentinel.exists(),
        "adopt path must never call the backend pr create"
    );
}

#[test]
fn pr_deliver_missing_head_branch_reports_head_not_pushed() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let gh_path = write_full_chain_stub(&stub);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: missing branch",
            "--body",
            "## Summary\n\nLand the new feature.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/missing",
            "--base",
            "main",
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(
        out.code, 65,
        "expected DATA 65 for missing head branch, stdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "head_not_pushed");
}

#[test]
fn pr_deliver_adopt_revalidates_existing_pr_body_and_fails_closed() {
    // The adopted PR's actual body (fetched via pr view) goes through the
    // same `## Summary` / `## Test plan` gate as a create-path body. A
    // non-compliant body fails closed with DATA 65 — and unlike the
    // pre-fix behaviour, the envelope names the PR it found instead of
    // reporting number 0.
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let gh_path = write_adopt_chain_stub(&stub, ADOPT_VIEW_MISSING_SECTIONS_JSON);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(
        out.code, 65,
        "expected DATA 65 when the adopted PR body lacks required sections, stdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "body_missing_sections");
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(steps, vec!["auth_status", "repo_view", "adopt"]);
    assert_eq!(envelope["data"]["steps"][2]["ok"], false);
    assert_eq!(
        envelope["data"]["pr"]["number"], 123,
        "failure envelope must name the adopted PR, not number 0"
    );
}

#[test]
fn pr_deliver_without_body_and_no_open_pr_still_fails_create_body_gate() {
    // When the head-branch lookup finds nothing, the macro falls through to
    // the unchanged create path: a missing `--body` still trips the
    // create-step body gate after the lookup ran.
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let gh_path = write_full_chain_stub(&stub);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--head",
            "feat/sample",
            "--base",
            "main",
        ],
    );
    assert_eq!(
        out.code, 65,
        "expected DATA 65 on missing body with no adoptable PR, stdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "body_missing_sections");
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(steps, vec!["auth_status", "repo_view"]);
}

#[test]
fn pr_deliver_closes_out_open_linked_issue_after_merge() {
    // The whole point of #1052: after a successful merge, a still-open issue
    // referenced by a `Closes/Fixes #N` closing keyword (surfaced through
    // `closingIssuesReferences`) must be closed deterministically instead of
    // waiting on GitHub's asynchronous auto-close. The macro appends an
    // `issue_closeout` step and the backend `issue close` actually runs.
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let close_sentinel = stub.tempdir.path().join("issue-close-called");
    let gh_path = write_closeout_chain_stub(&stub, LINKED_ISSUE_OPEN_JSON, false);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nCloses #4242.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], true);
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        steps,
        vec![
            "auth_status",
            "repo_view",
            "create",
            "wait_checks",
            "ready",
            "merge",
            "issue_closeout",
        ],
        "closeout must run as the last step after a merge"
    );
    let closeout = &envelope["data"]["steps"][6];
    assert_eq!(closeout["ok"], true);
    assert_eq!(
        closeout["schema_version"],
        "cli.forge-cli.issue.closeout.v1"
    );
    assert_eq!(closeout["payload"]["issues"][0]["number"], 4242);
    assert_eq!(closeout["payload"]["issues"][0]["action"], "closed");
    assert!(
        close_sentinel.exists(),
        "backend `issue close` must run for a still-open linked issue"
    );
}

#[test]
fn pr_deliver_closeout_skips_already_closed_linked_issue() {
    // When GitHub's auto-close already fired (issue is CLOSED at closeout
    // time), the step records `already_closed` and never invokes the backend
    // `issue close` — the end state is closed either way, so the step is a
    // no-op that stays idempotent.
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let close_sentinel = stub.tempdir.path().join("issue-close-called");
    let gh_path = write_closeout_chain_stub(&stub, LINKED_ISSUE_CLOSED_JSON, false);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nCloses #4242.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    let closeout = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .find(|s| s["step"] == "issue_closeout")
        .expect("issue_closeout step present");
    assert_eq!(closeout["ok"], true);
    assert_eq!(closeout["payload"]["issues"][0]["number"], 4242);
    assert_eq!(closeout["payload"]["issues"][0]["action"], "already_closed");
    assert!(
        !close_sentinel.exists(),
        "backend `issue close` must NOT run for an already-closed linked issue"
    );
}

#[test]
fn pr_deliver_reports_merge_success_when_issue_close_fails() {
    // The core #1052 invariant: closeout is best-effort. When the backend
    // `issue close` fails after a landed merge, the delivery is still ok=true
    // and merged=true; only the closeout step is ok=false with an `error`
    // outcome. A future refactor that let a failed closeout fail the merge
    // would break exactly this test.
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let close_sentinel = stub.tempdir.path().join("issue-close-called");
    let gh_path = write_closeout_chain_stub(
        &stub,
        LINKED_ISSUE_OPEN_JSON,
        /*close_exits_one=*/ true,
    );
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nCloses #4242.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(
        out.code, 0,
        "a failed closeout must not fail the delivery; stdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], true, "delivery envelope must stay ok");
    assert_eq!(envelope["data"]["pr"]["merged"], true);
    assert_eq!(envelope["data"]["pr"]["merge_sha"], "abc123def456");
    let closeout = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .find(|s| s["step"] == "issue_closeout")
        .expect("issue_closeout step present");
    assert_eq!(
        closeout["ok"], false,
        "closeout step reflects the failed close"
    );
    assert_eq!(closeout["payload"]["issues"][0]["number"], 4242);
    assert_eq!(closeout["payload"]["issues"][0]["action"], "error");
    assert!(
        closeout["payload"]["issues"][0]["error"].is_string(),
        "the failed close records an error message"
    );
    assert!(
        close_sentinel.exists(),
        "the backend close was attempted before failing"
    );
}

#[test]
fn pr_deliver_no_issue_closeout_flag_skips_the_step() {
    // `--no-issue-closeout` opts out: no closeout step, no backend close call,
    // even though the merged PR references a still-open closing issue.
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let close_sentinel = stub.tempdir.path().join("issue-close-called");
    let gh_path = write_closeout_chain_stub(&stub, LINKED_ISSUE_OPEN_JSON, false);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nCloses #4242.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
            "--no-issue-closeout",
        ],
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        steps,
        vec![
            "auth_status",
            "repo_view",
            "create",
            "wait_checks",
            "ready",
            "merge",
        ],
        "--no-issue-closeout must omit the closeout step"
    );
    assert!(
        !close_sentinel.exists(),
        "no backend `issue close` when closeout is disabled"
    );
}
