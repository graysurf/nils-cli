//! End-to-end contract coverage for the governed default-branch push.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

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

const GLAB_REPO_VIEW_JSON: &str = r#"{
  "path": "demo",
  "namespace": { "full_path": "sympoies" },
  "web_url": "https://gitlab.com/sympoies/demo",
  "default_branch": "main",
  "merge_method": "merge",
  "squash_option": "default_on"
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
    requested_head_sha: &'a str,
    symbolic_ref_exit: i32,
    ancestry_exit: i32,
    push_exit: i32,
    push_sleep: bool,
    remote_output_bytes: usize,
    timeout_ms: Option<&'a str>,
    capture_limit_bytes: Option<&'a str>,
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
            requested_head_sha: HEAD,
            symbolic_ref_exit: 0,
            ancestry_exit: 0,
            push_exit: 0,
            push_sleep: false,
            remote_output_bytes: 0,
            timeout_ms: None,
            capture_limit_bytes: None,
        }
    }
}

fn gh_stub() -> String {
    format!("#!/bin/sh\ncat <<'EOF'\n{GH_REPO_VIEW_JSON}\nEOF\n")
}

fn glab_stub() -> String {
    format!("#!/bin/sh\ncat <<'EOF'\n{GLAB_REPO_VIEW_JSON}\nEOF\n")
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
while [ "$1" = "-c" ]; do
  shift 2
done
case "$1" in
  remote)
    if [ '{remote_output_bytes}' -gt 0 ]; then
      head -c '{remote_output_bytes}' /dev/zero | tr '\000' x
    else
      printf '%s' '{push_urls}'
    fi
    ;;
  check-ref-format)
    exit 0
    ;;
  status)
    printf '%s' '{status}'
    ;;
  symbolic-ref)
    if [ '{symbolic_ref_exit}' -ne 0 ]; then
      exit '{symbolic_ref_exit}'
    fi
    printf '%s\n' '{branch}'
    ;;
  rev-parse)
    candidate=''
    for arg in "$@"; do
      candidate="$arg"
    done
    if [ "$candidate" = "HEAD^{{commit}}" ]; then
      printf '%s\n' '{head}'
    else
      printf '%s\n' '{requested_head_sha}'
    fi
    ;;
  ls-remote)
    if [ -f "$state" ]; then
      printf '%s\trefs/heads/main\n' '{after_push_remote}'
    else
      printf '%s\trefs/heads/main\n' '{initial_remote}'
    fi
    ;;
  cat-file)
    exit 0
    ;;
  merge-base)
    if [ '{ancestry_exit}' -ne 0 ]; then
      printf '%s\n' 'fatal: ancestry lookup failed' >&2
    fi
    exit '{ancestry_exit}'
    ;;
  rev-list)
    printf '%s\n' '{count}'
    ;;
  log)
    printf '%s\n' '{signature}'
    ;;
  push)
    if [ '{push_sleep}' = true ]; then
      sleep 30 &
      child_pid=$!
      printf '%s\n' "$child_pid" > "$GIT_CHILD_PID_FILE"
      wait "$child_pid"
    fi
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
        requested_head_sha = scenario.requested_head_sha,
        after_push_remote = scenario.after_push_remote,
        initial_remote = scenario.initial_remote,
        count = scenario.count,
        signature = scenario.signature,
        symbolic_ref_exit = scenario.symbolic_ref_exit,
        ancestry_exit = scenario.ancestry_exit,
        push_exit = scenario.push_exit,
        push_sleep = scenario.push_sleep,
        remote_output_bytes = scenario.remote_output_bytes,
    )
}

fn run(scenario: GitScenario<'_>) -> (StubEnv, super::support::CmdOutput) {
    run_with_options(scenario, false, "github", "HEAD")
}

fn run_with_dry_run(
    scenario: GitScenario<'_>,
    dry_run: bool,
) -> (StubEnv, super::support::CmdOutput) {
    run_with_options(scenario, dry_run, "github", "HEAD")
}

fn run_with_options(
    scenario: GitScenario<'_>,
    dry_run: bool,
    provider: &str,
    head: &str,
) -> (StubEnv, super::support::CmdOutput) {
    run_with_repo_options(scenario, dry_run, provider, head, "sympoies/demo")
}

fn run_with_repo_options(
    scenario: GitScenario<'_>,
    dry_run: bool,
    provider: &str,
    head: &str,
    repository: &str,
) -> (StubEnv, super::support::CmdOutput) {
    let stub = match provider {
        "github" => StubEnv::new().gh_stub(&gh_stub()),
        "gitlab" => StubEnv::new().glab_stub(&glab_stub()),
        "local" => StubEnv::new(),
        other => panic!("unsupported test provider: {other}"),
    };
    let reason = stub.tempdir.path().join("reason.md");
    let state = stub.tempdir.path().join("pushed");
    let log = stub.tempdir.path().join("git.log");
    let child_pid = stub.tempdir.path().join("git-child.pid");
    fs::write(
        &reason,
        "User explicitly requested direct commit and push to main for this hotfix.",
    )
    .expect("reason");
    let git_body = git_stub(&state.to_string_lossy(), &log.to_string_lossy(), scenario);
    let mut stub = stub.env("GIT_CHILD_PID_FILE", child_pid.to_string_lossy());
    if let Some(timeout_ms) = scenario.timeout_ms {
        stub = stub.env("FORGE_CLI_GIT_TIMEOUT_MS", timeout_ms);
    }
    if let Some(capture_limit_bytes) = scenario.capture_limit_bytes {
        stub = stub.env("FORGE_CLI_GIT_CAPTURE_LIMIT_BYTES", capture_limit_bytes);
    }
    let stub = stub.git_stub(&git_body);
    let reason = reason.to_string_lossy().into_owned();
    let mut args = vec![
        "--provider",
        provider,
        "--repo",
        repository,
        "--format",
        "json",
        "repo",
        "push-default",
        "--head",
        head,
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

#[derive(Clone, Copy)]
enum RealGitRace {
    None,
    Delete,
    Rewind,
}

struct RealGitScenario {
    stub: StubEnv,
    worktree: PathBuf,
    remote: PathBuf,
    base: String,
    head: String,
    rewind_target: String,
    push_options: PathBuf,
    git_log: PathBuf,
}

fn git_output(args: &[&str]) -> String {
    let output = Command::new("git").args(args).output().expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn commit_file(worktree: &Path, contents: &str, message: &str) -> String {
    fs::write(worktree.join("payload.txt"), contents).expect("write payload");
    git_output(&[
        "-C",
        worktree.to_str().expect("worktree"),
        "add",
        "payload.txt",
    ]);
    git_output(&[
        "-C",
        worktree.to_str().expect("worktree"),
        "-c",
        "user.name=Forge Test",
        "-c",
        "user.email=forge-test@example.invalid",
        "commit",
        "-m",
        message,
    ]);
    git_output(&[
        "-C",
        worktree.to_str().expect("worktree"),
        "rev-parse",
        "HEAD",
    ])
}

fn setup_real_git(race: RealGitRace, inherited_push_config: bool) -> RealGitScenario {
    let stub = StubEnv::new().gh_stub(&gh_stub());
    let worktree = stub.tempdir.path().join("worktree");
    let remote = stub.tempdir.path().join("remote.git");
    let reason = stub.tempdir.path().join("reason.md");
    let push_options = stub.tempdir.path().join("push-options.log");
    let git_log = stub.tempdir.path().join("real-git.log");
    fs::write(&reason, "Explicitly authorized direct-main hotfix.").expect("reason");

    git_output(&["init", "--bare", remote.to_str().expect("remote")]);
    git_output(&[
        "init",
        "--initial-branch=main",
        worktree.to_str().expect("worktree"),
    ]);
    let rewind_target = commit_file(&worktree, "base one\n", "base one");
    let base = commit_file(&worktree, "base two\n", "base two");
    git_output(&[
        "-C",
        worktree.to_str().expect("worktree"),
        "push",
        remote.to_str().expect("remote"),
        "HEAD:refs/heads/main",
    ]);
    git_output(&[
        "-C",
        worktree.to_str().expect("worktree"),
        "remote",
        "add",
        "origin",
        "https://github.com/sympoies/demo.git",
    ]);
    git_output(&[
        "-C",
        worktree.to_str().expect("worktree"),
        "switch",
        "-c",
        "feat/tiny-hotfix",
    ]);
    let head = commit_file(&worktree, "hotfix\n", "hotfix");

    if inherited_push_config {
        for (key, value) in [
            ("push.followTags", "true"),
            ("push.pushOption", "ci.skip"),
            ("push.recurseSubmodules", "on-demand"),
        ] {
            git_output(&[
                "-C",
                worktree.to_str().expect("worktree"),
                "config",
                key,
                value,
            ]);
        }
        git_output(&[
            "-C",
            worktree.to_str().expect("worktree"),
            "-c",
            "user.name=Forge Test",
            "-c",
            "user.email=forge-test@example.invalid",
            "tag",
            "-a",
            "unexpected-follow-tag",
            "-m",
            "must not follow",
            "HEAD",
        ]);
        git_output(&[
            "--git-dir",
            remote.to_str().expect("remote"),
            "config",
            "receive.advertisePushOptions",
            "true",
        ]);
        let hook = remote.join("hooks/pre-receive");
        fs::write(
            &hook,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"${{GIT_PUSH_OPTION_COUNT:-unset}}\" > '{}'\ncat >/dev/null\n",
                push_options.display()
            ),
        )
        .expect("write pre-receive hook");
        let mut permissions = fs::metadata(&hook).expect("hook metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).expect("chmod hook");
    }

    let wrapper = r#"#!/usr/bin/env bash
set -euo pipefail
printf '%q ' "$@" >> "$REAL_GIT_LOG"
printf '\n' >> "$REAL_GIT_LOG"
args=("$@")
if [[ " $* " == *" log -1 --format=%G? "* ]]; then
  printf 'G\n'
  exit 0
fi
if [[ " $* " == *" push --porcelain "* ]]; then
  case "${RACE_ACTION:-none}" in
    delete)
      git --git-dir="$REAL_REMOTE" update-ref -d refs/heads/main
      ;;
    rewind)
      git --git-dir="$REAL_REMOTE" update-ref refs/heads/main "$RACE_TARGET"
      ;;
  esac
fi
for i in "${!args[@]}"; do
  if [[ "${args[$i]}" == "https://github.com/sympoies/demo.git" ]]; then
    args[$i]="$REAL_REMOTE"
  fi
done
exec git "${args[@]}"
"#;
    let race_action = match race {
        RealGitRace::None => "none",
        RealGitRace::Delete => "delete",
        RealGitRace::Rewind => "rewind",
    };
    let stub = stub
        .env("REAL_REMOTE", remote.to_string_lossy())
        .env("REAL_GIT_LOG", git_log.to_string_lossy())
        .env("RACE_ACTION", race_action)
        .env("RACE_TARGET", &rewind_target)
        .git_stub(wrapper);

    RealGitScenario {
        stub,
        worktree,
        remote,
        base,
        head,
        rewind_target,
        push_options,
        git_log,
    }
}

fn run_real_git(scenario: &RealGitScenario) -> super::support::CmdOutput {
    let reason = scenario.stub.tempdir.path().join("reason.md");
    run_forge_cli_in(
        &scenario.stub,
        &[
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
            &scenario.base,
            "--reason-file",
            reason.to_str().expect("reason"),
        ],
        Some(&scenario.worktree),
    )
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
            "push --porcelain --no-follow-tags --no-recurse-submodules --no-push-option --force-with-lease=refs/heads/main:{BASE} -- https://github.com/sympoies/demo.git {HEAD}:refs/heads/main"
        )),
        "missing exact destination-bound compare-and-swap push: {log}"
    );
    assert!(
        !log.split_whitespace().any(|token| token == "--force"),
        "unconstrained force option leaked into Git argv: {log}"
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
fn push_default_returns_equivalent_verified_gitlab_receipt() {
    let scenario = GitScenario {
        push_urls: "https://gitlab.com/sympoies/demo.git\n",
        ..GitScenario::default()
    };
    let (stub, out) = run_with_options(scenario, false, "gitlab", "HEAD");
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["data"]["provider"], "gitlab");
    assert_eq!(envelope["data"]["repository"], "sympoies/demo");
    assert_eq!(envelope["data"]["observed_remote_sha"], HEAD);
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert!(log.contains(&format!(
        "push --porcelain --no-follow-tags --no-recurse-submodules --no-push-option --force-with-lease=refs/heads/main:{BASE} -- https://gitlab.com/sympoies/demo.git {HEAD}:refs/heads/main"
    )));
}

#[test]
fn push_default_exact_lease_rejects_real_remote_delete_race() {
    let scenario = setup_real_git(RealGitRace::Delete, false);
    let out = run_real_git(&scenario);
    assert_eq!(out.code, 1, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "default_push_rejected");
    let output = Command::new("git")
        .args([
            "--git-dir",
            scenario.remote.to_str().expect("remote"),
            "show-ref",
            "--verify",
            "refs/heads/main",
        ])
        .output()
        .expect("inspect remote");
    assert!(
        !output.status.success(),
        "lease must not recreate deleted ref"
    );
}

#[test]
fn push_default_exact_lease_rejects_real_remote_rewind_race() {
    let scenario = setup_real_git(RealGitRace::Rewind, false);
    let out = run_real_git(&scenario);
    assert_eq!(out.code, 1, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "default_push_rejected");
    let observed = git_output(&[
        "--git-dir",
        scenario.remote.to_str().expect("remote"),
        "rev-parse",
        "refs/heads/main",
    ]);
    assert_eq!(observed, scenario.rewind_target);
}

#[test]
fn push_default_real_missing_remote_ref_has_typed_data_error() {
    let scenario = setup_real_git(RealGitRace::None, false);
    git_output(&[
        "--git-dir",
        scenario.remote.to_str().expect("remote"),
        "update-ref",
        "-d",
        "refs/heads/main",
    ]);
    let out = run_real_git(&scenario);
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "remote_default_branch_missing");
}

#[test]
fn push_default_disables_inherited_real_git_push_expansion() {
    let scenario = setup_real_git(RealGitRace::None, true);
    let out = run_real_git(&scenario);
    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["data"]["observed_remote_sha"], scenario.head);
    let remote_head = git_output(&[
        "--git-dir",
        scenario.remote.to_str().expect("remote"),
        "rev-parse",
        "refs/heads/main",
    ]);
    assert_eq!(remote_head, scenario.head);
    let tag = Command::new("git")
        .args([
            "--git-dir",
            scenario.remote.to_str().expect("remote"),
            "show-ref",
            "--verify",
            "refs/tags/unexpected-follow-tag",
        ])
        .output()
        .expect("inspect tag");
    assert!(!tag.status.success(), "push.followTags must be disabled");
    assert_eq!(
        fs::read_to_string(&scenario.push_options)
            .expect("push option capture")
            .trim(),
        "0",
        "push.pushOption must be disabled"
    );
    let log = fs::read_to_string(&scenario.git_log).expect("real git log");
    assert!(log.contains("--no-follow-tags"));
    assert!(log.contains("--no-recurse-submodules"));
    assert!(log.contains("--no-push-option"));
    assert!(log.contains("push.followTags=false"));
    assert!(log.contains("push.pushOption="));
    assert!(log.contains("push.recurseSubmodules=no"));
}

#[test]
fn push_default_bounds_stalled_git_push_and_kills_its_process_group() {
    let started = Instant::now();
    let (stub, out) = run(GitScenario {
        push_sleep: true,
        timeout_ms: Some("75"),
        ..GitScenario::default()
    });
    assert_eq!(out.code, 69, "stdout={} stderr={}", out.stdout, out.stderr);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "bounded Git call took {:?}",
        started.elapsed()
    );
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "git_timeout");
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("push --porcelain"))
            .count(),
        1,
        "timed-out push must not retry"
    );
    assert!(!stub.tempdir.path().join("pushed").exists());
    let child_pid = fs::read_to_string(stub.tempdir.path().join("git-child.pid"))
        .expect("child pid")
        .trim()
        .to_string();
    std::thread::sleep(Duration::from_millis(25));
    let alive = Command::new("kill")
        .args(["-0", &child_pid])
        .output()
        .expect("probe child");
    assert!(
        !alive.status.success(),
        "timed-out Git descendant {child_pid} survived process-group cleanup"
    );
}

#[test]
fn push_default_bounds_git_output_before_parsing_it() {
    let (_stub, out) = run(GitScenario {
        remote_output_bytes: 2_048,
        capture_limit_bytes: Some("1024"),
        ..GitScenario::default()
    });
    assert_eq!(out.code, 69, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "git_output_limit");
}

#[test]
fn push_default_rejects_cross_provider_destination_before_push() {
    let (stub, out) = run_with_options(GitScenario::default(), false, "gitlab", "HEAD");
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "provider_mismatch");
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert!(!log.lines().any(|line| line.contains("push --porcelain")));
    assert!(!stub.tempdir.path().join("pushed").exists());
}

#[test]
fn push_default_rejects_mismatched_explicit_repository_before_provider_lookup() {
    let (stub, out) = run_with_repo_options(
        GitScenario::default(),
        true,
        "github",
        "HEAD",
        "sympoies/other",
    );
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "repository_mismatch");
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert!(!log.lines().any(|line| line.contains("ls-remote")));
    assert!(!log.lines().any(|line| line.contains("push --porcelain")));
}

#[test]
fn push_default_local_provider_is_usage_error() {
    let (_stub, out) = run_with_options(GitScenario::default(), true, "local", "HEAD");
    assert_eq!(out.code, 64, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "provider_unsupported");
}

#[test]
fn push_default_rejects_repository_metadata_from_another_host() {
    let (stub, out) = run(GitScenario {
        push_urls: "git@internal.ghe.com:sympoies/demo.git\n",
        ..GitScenario::default()
    });
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "repository_mismatch");
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert!(!log.lines().any(|line| line.contains("ls-remote")));
    assert!(!log.lines().any(|line| line.contains("push --porcelain")));
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
fn push_default_rejects_non_current_head_before_push() {
    let (stub, out) = run_with_options(
        GitScenario {
            requested_head_sha: OTHER,
            ..GitScenario::default()
        },
        false,
        "github",
        "feat/not-checked-out",
    );
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "head_not_checked_out");
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert!(!log.lines().any(|line| line.contains("push --porcelain")));
    assert!(!stub.tempdir.path().join("pushed").exists());
}

#[test]
fn push_default_accepts_full_ref_and_sha_for_checked_out_head() {
    for head in ["refs/heads/feat/tiny-hotfix", HEAD] {
        let (_stub, out) = run_with_options(GitScenario::default(), true, "github", head);
        assert_eq!(out.code, 0, "head={head} stderr={}", out.stderr);
        let envelope = parse_envelope(&out.stdout);
        assert_eq!(envelope["data"]["head"], head);
        assert_eq!(envelope["data"]["head_sha"], HEAD);
    }
}

#[test]
fn push_default_rejects_detached_head_before_push() {
    let (stub, out) = run(GitScenario {
        symbolic_ref_exit: 1,
        ..GitScenario::default()
    });
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "detached_head");
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert!(!log.lines().any(|line| line.contains("push --porcelain")));
    assert!(!stub.tempdir.path().join("pushed").exists());
}

#[test]
fn push_default_rejects_non_ancestor_expected_base_before_push() {
    let (stub, out) = run(GitScenario {
        ancestry_exit: 1,
        ..GitScenario::default()
    });
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "expected_base_not_ancestor");
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert!(!log.lines().any(|line| line.contains("push --porcelain")));
    assert!(!stub.tempdir.path().join("pushed").exists());
}

#[test]
fn push_default_reports_fatal_ancestry_lookup_as_software_error() {
    let (stub, out) = run(GitScenario {
        ancestry_exit: 128,
        ..GitScenario::default()
    });
    assert_eq!(out.code, 70, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "software_error");
    assert_eq!(envelope["error"]["message"], "git ancestry check failed");
    let log = fs::read_to_string(stub.tempdir.path().join("git.log")).expect("git log");
    assert!(!log.lines().any(|line| line.contains("push --porcelain")));
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
    assert!(!log.split_whitespace().any(|token| token == "--force"));
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

#[test]
fn push_default_error_catalog_covers_the_runtime_contract() {
    let catalog = include_str!("../../docs/specs/forge-cli-ops-v1.yaml");
    let block = catalog
        .split("  direct_default_delivery:")
        .nth(1)
        .expect("direct delivery validation")
        .split("  draft_merge_refused:")
        .next()
        .expect("direct delivery block");
    for kind in [
        "commit_signature_unverified",
        "default_branch_checkout",
        "default_push_rejected",
        "default_push_verification_failed",
        "detached_head",
        "direct_commit_count_invalid",
        "dirty_worktree",
        "expected_base_mismatch",
        "expected_base_missing",
        "expected_base_not_ancestor",
        "git_output_limit",
        "git_timeout",
        "head_not_checked_out",
        "local_path_present",
        "object_id_invalid",
        "provider_mismatch",
        "provider_unsupported",
        "push_destination_ambiguous",
        "push_destination_credentials_unsupported",
        "push_destination_missing",
        "reason_file_unreadable",
        "reason_invalid",
        "remote_default_branch_missing",
        "remote_default_lookup_failed",
        "repository_mismatch",
        "software_error",
    ] {
        assert!(
            block.contains(kind),
            "missing {kind} from direct-delivery catalog"
        );
    }
    assert!(
        block.contains("exit: DATA | RUNTIME | UNAVAILABLE | SOFTWARE | USAGE"),
        "direct-delivery exit catalog must include every emitted exit class"
    );
}
