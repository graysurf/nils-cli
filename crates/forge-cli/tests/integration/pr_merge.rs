//! `pr merge` integration tests covering the dry-run plan envelope and the
//! configuration-driven `keep_branch_conflict` rule. The wider lock-down chain
//! (rules 4 / 6 / 7 / 8 / 9) is unit-tested in
//! `crates/forge-cli/src/ops/pr_merge.rs` and the dedicated gate suite at
//! `tests/integration/required_check_gate.rs`; this module pins the CLI surface
//! and the per-repo `.forge-cli.toml` precedence end-to-end.

use std::fs;
use std::process::Command;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::support::{StubEnv, parse_envelope, run_forge_cli_in};

const FORBIDDEN_STUB: &str = "#!/bin/sh\necho 'should not run during dry-run' >&2\nexit 99\n";

fn make_gitlab_repo() -> TempDir {
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
    git(&[
        "remote",
        "add",
        "origin",
        "https://gitlab.example.com/group/project.git",
    ]);
    tempdir
}

fn gitlab_merge_api_stub(stub: &StubEnv) -> String {
    gitlab_merge_api_stub_with_discussions(stub, "[]")
}

/// Same stub, with a caller-provided MR discussions response so tests can
/// exercise merge lock-down rule 12 (unresolved review threads).
fn gitlab_merge_api_stub_with_discussions(stub: &StubEnv, discussions: &str) -> String {
    let sentinel = stub.tempdir.path().join("merged");
    let args_log = stub.tempdir.path().join("merge-args");
    format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "repo view")
    cat <<'EOF'
{{
  "namespace": {{ "full_path": "group" }},
  "path": "project",
  "web_url": "https://gitlab.example.com/group/project",
  "default_branch": "main",
  "merge_method": "merge",
  "squash_option": "default_on"
}}
EOF
    ;;
  "mr view")
    if [ -e {sentinel} ]; then
      cat <<'EOF'
{{
  "iid": 7,
  "web_url": "https://gitlab.example.com/group/project/-/merge_requests/7",
  "state": "merged",
  "draft": false,
  "title": "feat: sample",
  "source_branch": "feat/sample",
  "target_branch": "main",
  "merge_status": "can_be_merged",
  "sha": "abc123",
  "merge_commit_sha": "def456",
  "labels": [],
  "head_pipeline": {{ "id": 99, "status": "success", "web_url": "https://gitlab.example.com/group/project/-/pipelines/99" }}
}}
EOF
    else
      cat <<'EOF'
{{
  "iid": 7,
  "web_url": "https://gitlab.example.com/group/project/-/merge_requests/7",
  "state": "opened",
  "draft": false,
  "title": "feat: sample",
  "source_branch": "feat/sample",
  "target_branch": "main",
  "merge_status": "can_be_merged",
  "sha": "abc123",
  "merge_commit_sha": null,
  "labels": [],
  "head_pipeline": {{ "id": 99, "status": "success", "web_url": "https://gitlab.example.com/group/project/-/pipelines/99" }}
}}
EOF
    fi
    ;;
  "api --hostname")
    case "$*" in
      *"projects/group%2Fproject/pipelines/99/jobs?per_page=100"*)
        cat <<'EOF'
[
  {{
    "name": "build",
    "stage": "test",
    "status": "success",
    "allow_failure": false,
    "web_url": "https://gitlab.example.com/group/project/-/jobs/1"
  }}
]
EOF
        ;;
      *)
        echo "stub: unexpected api args: $*" >&2
        exit 99
        ;;
    esac
    ;;
  "api --paginate")
    case "$*" in
      *"projects/group%2Fproject/merge_requests/7/discussions?per_page=100"*)
        # Merge lock-down rule 12 — review-thread sweep.
        cat <<'EOF'
{discussions}
EOF
        ;;
      *)
        echo "stub: unexpected paginate api args: $*" >&2
        exit 99
        ;;
    esac
    ;;
  "api --method")
    printf '%s\n' "$*" > {args_log}
    case "$*" in
      *"PUT"*"--hostname gitlab.example.com"*"projects/group%2Fproject/merge_requests/7/merge"*"squash=true"*"should_remove_source_branch=true"*"sha=abc123"*)
        touch {sentinel}
        cat <<'EOF'
{{ "state": "merged", "merge_commit_sha": "def456" }}
EOF
        ;;
      *)
        echo "stub: unexpected merge api args: $*" >&2
        exit 99
        ;;
    esac
    ;;
  *)
    echo "stub: unexpected glab args: $*" >&2
    exit 99
    ;;
esac
"#,
        sentinel = sentinel.display(),
        args_log = args_log.display(),
        discussions = discussions,
    )
}

#[test]
fn pr_merge_dry_run_renders_squash_plan_with_delete_branch() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "merge",
            "42",
        ],
        None,
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.merge.v1");
    let plan: Vec<String> = env["data"]["plan"]
        .as_array()
        .expect("plan array")
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert!(plan.iter().any(|s| s == "merge"), "{plan:?}");
    assert!(plan.iter().any(|s| s == "42"), "{plan:?}");
    assert!(plan.iter().any(|s| s == "--squash"), "{plan:?}");
    assert!(plan.iter().any(|s| s == "--delete-branch"), "{plan:?}");
}

#[test]
fn pr_merge_dry_run_method_override_uses_merge_flag() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "merge",
            "1",
            "--method",
            "merge",
        ],
        None,
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let plan: Vec<String> = env["data"]["plan"]
        .as_array()
        .expect("plan array")
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert!(plan.iter().any(|s| s == "--merge"), "{plan:?}");
    assert!(!plan.iter().any(|s| s == "--squash"), "{plan:?}");
}

#[test]
fn pr_merge_keep_branch_drops_delete_branch_in_dry_run_plan() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "merge",
            "1",
            "--keep-branch",
        ],
        None,
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let plan: Vec<String> = env["data"]["plan"]
        .as_array()
        .expect("plan array")
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert!(!plan.iter().any(|s| s == "--delete-branch"), "{plan:?}");
}

#[test]
fn pr_merge_keep_branch_conflicts_with_config_delete_branch_true() {
    // The lock-down rule (rule 10) requires an *explicit* config opting into
    // branch deletion before --keep-branch becomes a hard error. We mock that
    // by writing a .forge-cli.toml inside a tempdir and invoking the binary
    // from there so the loader picks it up.
    let repo = TempDir::new().expect("tempdir");
    fs::write(
        repo.path().join(".forge-cli.toml"),
        "[merge]\ndelete_branch = true\n",
    )
    .expect("write config");
    fs::create_dir_all(repo.path().join(".git")).expect("fake .git");

    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "merge",
            "1",
            "--keep-branch",
        ],
        Some(repo.path()),
    );
    assert_eq!(
        out.code, 65,
        "expected DATA 65 on keep_branch_conflict, stderr={}",
        out.stderr
    );
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "keep_branch_conflict");
}

#[test]
fn pr_merge_gitlab_uses_api_merge_after_required_checks_pass() {
    let tempdir = make_gitlab_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let args_log = stub.tempdir.path().join("merge-args");
    let body = gitlab_merge_api_stub(&stub);
    let stub = stub.glab_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.merge.v1");
    assert_eq!(env["data"]["provider"], "gitlab");
    assert_eq!(env["data"]["merge_sha"], "def456");

    let args = fs::read_to_string(args_log).expect("merge args log");
    assert!(args.contains("--method PUT"), "{args}");
    assert!(args.contains("--hostname gitlab.example.com"), "{args}");
    assert!(
        args.contains("projects/group%2Fproject/merge_requests/7/merge"),
        "{args}"
    );
    assert!(args.contains("sha=abc123"), "{args}");
}

const UNRESOLVED_DISCUSSIONS_JSON: &str = r#"[
  {
    "id": "d1",
    "notes": [
      {
        "id": 21,
        "resolvable": true,
        "resolved": false,
        "body": "please address this finding",
        "author": { "username": "quality-bot" },
        "created_at": "2026-06-11T04:49:36Z",
        "position": { "new_path": "src/lib.rs" }
      }
    ]
  }
]"#;

#[test]
fn pr_merge_gitlab_blocks_on_unresolved_review_threads() {
    let tempdir = make_gitlab_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let args_log = stub.tempdir.path().join("merge-args");
    let body = gitlab_merge_api_stub_with_discussions(&stub, UNRESOLVED_DISCUSSIONS_JSON);
    let stub = stub.glab_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
        ],
        Some(&repo_path),
    );
    assert_eq!(
        out.code, 65,
        "expected DATA 65 on unresolved threads, stdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "unresolved_review_threads");
    let message = env["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("--allow-unresolved-threads"),
        "message must name the bypass flag: {message}"
    );
    // The gate fired before the backend merge call.
    assert!(
        !args_log.exists(),
        "merge API must not run when the thread gate blocks"
    );
}

#[test]
fn pr_merge_gitlab_allow_unresolved_threads_bypasses_gate() {
    let tempdir = make_gitlab_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let args_log = stub.tempdir.path().join("merge-args");
    let body = gitlab_merge_api_stub_with_discussions(&stub, UNRESOLVED_DISCUSSIONS_JSON);
    let stub = stub.glab_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--allow-unresolved-threads",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["merge_sha"], "def456");
    assert!(args_log.exists(), "bypassed merge must reach the backend");
}
