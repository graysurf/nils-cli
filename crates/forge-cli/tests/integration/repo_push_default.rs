//! End-to-end contract coverage for the governed default-branch push.

use std::fs;

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli_in};

const BASE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OTHER: &str = "cccccccccccccccccccccccccccccccccccccccc";

const GH_REPO_VIEW_JSON: &str = r#"{
  "name": "demo",
  "owner": { "login": "sympoies" },
  "url": "https://github.com/sympoies/demo",
  "defaultBranchRef": { "name": "main" },
  "mergeCommitAllowed": false,
  "squashMergeAllowed": true,
  "rebaseMergeAllowed": false
}"#;

#[derive(Clone, Copy)]
struct GitScenario<'a> {
    branch: &'a str,
    status: &'a str,
    push_urls: &'a str,
    initial_remote: &'a str,
    after_push_remote: &'a str,
    count: &'a str,
    signature: &'a str,
    push_exit: i32,
}

impl Default for GitScenario<'static> {
    fn default() -> Self {
        Self {
            branch: "feat/tiny-hotfix",
            status: "",
            push_urls: "https://github.com/sympoies/demo.git\n",
            initial_remote: BASE,
            after_push_remote: HEAD,
            count: "1",
            signature: "G",
            push_exit: 0,
        }
    }
}

fn gh_stub() -> String {
    format!("#!/bin/sh\ncat <<'EOF'\n{GH_REPO_VIEW_JSON}\nEOF\n")
}

fn git_stub(state: &str, log: &str, scenario: GitScenario<'_>) -> String {
    format!(
        r#"#!/bin/sh
state='{state}'
log='{log}'
printf '%s\n' "$*" >> "$log"
if [ "$1" = "-C" ]; then
  shift 2
fi
case "$1" in
  remote)
    printf '%s' '{push_urls}'
    ;;
  check-ref-format)
    exit 0
    ;;
  status)
    printf '%s' '{status}'
    ;;
  symbolic-ref)
    printf '%s\n' '{branch}'
    ;;
  rev-parse)
    printf '%s\n' '{head}'
    ;;
  ls-remote)
    if [ -f "$state" ]; then
      printf '%s\trefs/heads/main\n' '{after_push_remote}'
    else
      printf '%s\trefs/heads/main\n' '{initial_remote}'
    fi
    ;;
  cat-file|merge-base)
    exit 0
    ;;
  rev-list)
    printf '%s\n' '{count}'
    ;;
  log)
    printf '%s\n' '{signature}'
    ;;
  push)
    if [ '{push_exit}' -ne 0 ]; then
      printf '%s\n' 'rejected: non-fast-forward' >&2
      exit '{push_exit}'
    fi
    : > "$state"
    printf '%s\n' 'ok refs/heads/main'
    ;;
  *)
    printf 'unexpected git argv: %s\n' "$*" >&2
    exit 97
    ;;
esac
"#,
        state = state,
        log = log,
        status = scenario.status,
        push_urls = scenario.push_urls,
        branch = scenario.branch,
        head = HEAD,
        after_push_remote = scenario.after_push_remote,
        initial_remote = scenario.initial_remote,
        count = scenario.count,
        signature = scenario.signature,
        push_exit = scenario.push_exit,
    )
}

fn run(scenario: GitScenario<'_>) -> (StubEnv, super::support::CmdOutput) {
    run_with_dry_run(scenario, false)
}

fn run_with_dry_run(
    scenario: GitScenario<'_>,
    dry_run: bool,
) -> (StubEnv, super::support::CmdOutput) {
    let stub = StubEnv::new().gh_stub(&gh_stub());
    let reason = stub.tempdir.path().join("reason.md");
    let state = stub.tempdir.path().join("pushed");
    let log = stub.tempdir.path().join("git.log");
    fs::write(
        &reason,
        "User explicitly requested direct commit and push to main for this hotfix.",
    )
    .expect("reason");
    let git_body = git_stub(&state.to_string_lossy(), &log.to_string_lossy(), scenario);
    let stub = stub.git_stub(&git_body);
    let reason = reason.to_string_lossy().into_owned();
    let mut args = vec![
        "--provider",
        "github",
        "--repo",
        "sympoies/demo",
        "--format",
        "json",
        "repo",
        "push-default",
        "--head",
        "HEAD",
        "--expected-base",
        BASE,
        "--reason-file",
        &reason,
    ];
    if dry_run {
        args.insert(0, "--dry-run");
    }
    let out = run_forge_cli_in(&stub, &args, Some(stub.tempdir.path()));
    (stub, out)
}

#[test]
fn push_default_dry_run_preflights_without_pushing() {
    let (stub, out) = run_with_dry_run(GitScenario::default(), true);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["data"]["pushed"], false);
    assert_eq!(envelope["data"]["observed_remote_sha"], BASE);
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert!(!log.lines().any(|line| line.contains("push --porcelain")));
    assert!(!stub.tempdir.path().join("pushed").exists());
}

#[test]
fn push_default_returns_verified_remote_receipt_without_force() {
    let (stub, out) = run(GitScenario::default());
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(
        envelope["schema_version"],
        "cli.forge-cli.repo.push-default.v1"
    );
    assert_eq!(envelope["data"]["pushed"], true);
    assert_eq!(envelope["data"]["expected_base"], BASE);
    assert_eq!(envelope["data"]["head_sha"], HEAD);
    assert_eq!(envelope["data"]["observed_remote_sha"], HEAD);
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert!(
        log.contains(&format!(
            "push --porcelain -- https://github.com/sympoies/demo.git {HEAD}:refs/heads/main"
        )),
        "missing exact destination-bound normal push: {log}"
    );
    assert!(
        !log.contains("--force"),
        "force option leaked into Git argv: {log}"
    );
    assert_eq!(
        log.lines()
            .filter(|line| {
                line.contains(
                    "ls-remote --exit-code -- https://github.com/sympoies/demo.git refs/heads/main",
                )
            })
            .count(),
        2,
        "base and read-back must use the validated push URL: {log}"
    );
    assert!(!log.contains("ls-remote --exit-code -- origin"));
}

#[test]
fn push_default_rejects_stale_expected_base_before_push() {
    let (stub, out) = run(GitScenario {
        initial_remote: OTHER,
        ..GitScenario::default()
    });
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "expected_base_mismatch");
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert!(!log.lines().any(|line| line.contains(" push ") || line.starts_with("-C") && line.contains(" push")));
    assert!(!stub.tempdir.path().join("pushed").exists());
}

#[test]
fn push_default_rejects_unsigned_commit() {
    let (_stub, out) = run(GitScenario {
        signature: "N",
        ..GitScenario::default()
    });
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "commit_signature_unverified");
}

#[test]
fn push_default_rejects_multiple_commits() {
    let (_stub, out) = run(GitScenario {
        count: "2",
        ..GitScenario::default()
    });
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "direct_commit_count_invalid");
}

#[test]
fn push_default_rejects_default_branch_checkout() {
    let (_stub, out) = run(GitScenario {
        branch: "main",
        ..GitScenario::default()
    });
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "default_branch_checkout");
}

#[test]
fn push_default_rejects_dirty_checkout() {
    let (_stub, out) = run(GitScenario {
        status: " M src/lib.rs\n",
        ..GitScenario::default()
    });
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "dirty_worktree");
}

#[test]
fn push_default_binds_the_actual_push_destination_repository() {
    let (stub, out) = run(GitScenario {
        push_urls: "https://github.com/sympoies/other.git\n",
        ..GitScenario::default()
    });
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "repository_mismatch");
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert!(log.contains("remote get-url --push --all -- origin"));
    assert!(!log.lines().any(|line| line.contains("push --porcelain")));
}

#[test]
fn push_default_rejects_multiple_push_destinations() {
    let (stub, out) = run(GitScenario {
        push_urls: concat!(
            "https://github.com/sympoies/demo.git\n",
            "https://github.com/sympoies/mirror.git\n"
        ),
        ..GitScenario::default()
    });
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "push_destination_ambiguous");
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert!(!log.lines().any(|line| line.contains("push --porcelain")));
}

#[test]
fn push_default_rejects_http_push_url_userinfo() {
    let (stub, out) = run(GitScenario {
        push_urls: "https://build-user@github.com/sympoies/demo.git\n",
        ..GitScenario::default()
    });
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(
        envelope["error"]["code"],
        "push_destination_credentials_unsupported"
    );
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert!(!log.lines().any(|line| line.contains("push --porcelain")));
}

#[test]
fn push_default_reports_normal_push_race_without_retrying_force() {
    let (stub, out) = run(GitScenario {
        push_exit: 1,
        ..GitScenario::default()
    });
    assert_eq!(out.code, 1, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "default_push_rejected");
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("push --porcelain"))
            .count(),
        1
    );
    assert!(!log.contains("--force"));
}

#[test]
fn push_default_fails_closed_when_post_push_readback_differs() {
    let (_stub, out) = run(GitScenario {
        after_push_remote: OTHER,
        ..GitScenario::default()
    });
    assert_eq!(out.code, 1, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(
        envelope["error"]["code"],
        "default_push_verification_failed"
    );
}
