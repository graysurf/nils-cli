//! Offline contract coverage for governed Forgejo repository bootstrap.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nils_test_support::http::{HttpResponse, RecordedRequest, TestServer};
use pretty_assertions::{assert_eq, assert_ne};

use super::support::{CmdOutput, StubEnv, parse_envelope, run_forge_cli};

const PROVIDER: &str = "forgejo-test";
const TOKEN_ENV: &str = "FORGE_CLI_TEST_FORGEJO_TOKEN";
const TOKEN: &str = "bootstrap-secret-token";
const LOCAL_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DRIFT_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[derive(Debug)]
struct ForgeState {
    exists: bool,
    default_branch: String,
    create_calls: usize,
    patch_calls: usize,
    create_ambiguous: bool,
    signature_verified: bool,
    legacy_top_level_signature: bool,
    receipt_seen_before_create: bool,
}

impl Default for ForgeState {
    fn default() -> Self {
        Self {
            exists: false,
            default_branch: String::new(),
            create_calls: 0,
            patch_calls: 0,
            create_ambiguous: false,
            signature_verified: true,
            legacy_top_level_signature: false,
            receipt_seen_before_create: false,
        }
    }
}

struct Fixture {
    stub: StubEnv,
    server: TestServer,
    state: Arc<Mutex<ForgeState>>,
    owner: String,
    readme: PathBuf,
    license: PathBuf,
    reason: PathBuf,
    remote_sha: PathBuf,
    git_log: PathBuf,
    semantic_log: PathBuf,
    commit_marker: PathBuf,
    receipt: PathBuf,
    checkout: PathBuf,
}

impl Fixture {
    fn new(owner: &str) -> Self {
        let stub = StubEnv::new();
        let readme = stub.tempdir.path().join("README.md");
        let license = stub.tempdir.path().join("LICENSE");
        let reason = stub.tempdir.path().join("authorization.txt");
        let remote_sha = stub.tempdir.path().join("remote-sha");
        let git_log = stub.tempdir.path().join("git.log");
        let semantic_log = stub.tempdir.path().join("semantic.log");
        let commit_marker = stub.tempdir.path().join("commit-created");
        fs::write(&readme, "# Widgets\n").expect("README fixture");
        fs::write(&license, "fixture license\n").expect("license fixture");
        fs::write(
            &reason,
            "Operator authorized private repository bootstrap.\n",
        )
        .expect("authorization fixture");

        let state_home = stub.tempdir.path().join("xdg-state");
        let receipt = state_home
            .join("forge-cli/repo-bootstrap")
            .join(PROVIDER)
            .join(owner)
            .join("widgets/receipt.json");
        let checkout = receipt.parent().expect("receipt parent").join("checkout");
        let state = Arc::new(Mutex::new(ForgeState::default()));
        let handler_state = Arc::clone(&state);
        let handler_remote_sha = remote_sha.clone();
        let handler_receipt = receipt.clone();
        let owner_owned = owner.to_string();
        let server = TestServer::new(move |request| {
            handle_forgejo(
                request,
                &handler_state,
                &handler_remote_sha,
                &handler_receipt,
                &owner_owned,
            )
            .with_header("Connection", "close")
        })
        .expect("Forgejo fixture server");

        let git = stub.write_stub(
            "git-bootstrap",
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$FORGE_TEST_GIT_LOG"
case "$*" in
  *"rev-parse"*"HEAD^{commit}"*)
    if [ "${FORGE_TEST_NO_HEAD_UNTIL_COMMIT:-}" = "1" ] && [ ! -f "$FORGE_TEST_COMMIT_MARKER" ]; then
      exit 1
    fi
    printf '%s\n' "$FORGE_TEST_LOCAL_SHA"
    ;;
  *"rev-list --parents -n 1"*)
    if [ -n "${FORGE_TEST_PARENT_SHA:-}" ]; then
      printf '%s %s\n' "$FORGE_TEST_LOCAL_SHA" "$FORGE_TEST_PARENT_SHA"
    else
      printf '%s\n' "$FORGE_TEST_LOCAL_SHA"
    fi
    ;;
  *"symbolic-ref --quiet --short HEAD"*) printf '%s\n' "${FORGE_TEST_CHECKOUT_BRANCH:-main}" ;;
  *"status --porcelain=v1 -z --untracked-files=all"*)
    if [ "${FORGE_TEST_NO_HEAD_UNTIL_COMMIT:-}" = "1" ] && [ ! -f "$FORGE_TEST_COMMIT_MARKER" ]; then
      printf '%b' "${FORGE_TEST_PARTIAL_STATUS:-A  LICENSE\\0A  README.md\\0}"
    else
      printf '%b' "${FORGE_TEST_STATUS:-}"
    fi
    ;;
  *"ls-tree -r --name-only -z HEAD"*)
    printf '%b' "${FORGE_TEST_TREE_PATHS:-LICENSE\\0README.md\\0}"
    ;;
  *"log -1 --format=%G?"*) printf '%s\n' "${FORGE_TEST_SIGNATURE:-G}" ;;
  *"push "*)
    printf 'askpass_username=%s\n' "$FORGE_CLI_BOOTSTRAP_USERNAME" >> "$FORGE_TEST_GIT_LOG"
    if [ "${FORGE_TEST_PUSH_MODE:-success}" = "fail" ]; then
      echo 'simulated push failure' >&2
      exit 1
    fi
    printf '%s\n' "$FORGE_TEST_LOCAL_SHA" > "$FORGE_TEST_REMOTE_SHA_FILE"
    if [ "${FORGE_TEST_PUSH_MODE:-success}" = "ambiguous" ]; then
      echo 'simulated lost push response' >&2
      exit 1
    fi
    ;;
esac
exit 0
"#,
        );
        let semantic = stub.write_stub(
            "semantic-commit-bootstrap",
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$FORGE_TEST_SEMANTIC_LOG"
if [ "${FORGE_TEST_SEMANTIC_MODE:-success}" = "fail_before_commit" ]; then
  echo 'simulated interruption before commit' >&2
  exit 1
fi
: > "$FORGE_TEST_COMMIT_MARKER"
printf '{"schema_version":"cli.semantic-commit.commit.v1","ok":true,"commit":{"sha":"%s"}}\n' "$FORGE_TEST_LOCAL_SHA"
"#,
        );
        let stub = stub
            .env(TOKEN_ENV, TOKEN)
            .env("XDG_STATE_HOME", state_home.to_string_lossy())
            .env("FORGE_CLI_GIT_BIN", git.to_string_lossy())
            .env("FORGE_CLI_SEMANTIC_COMMIT_BIN", semantic.to_string_lossy())
            .env("FORGE_TEST_LOCAL_SHA", LOCAL_SHA)
            .env("FORGE_TEST_REMOTE_SHA_FILE", remote_sha.to_string_lossy())
            .env("FORGE_TEST_GIT_LOG", git_log.to_string_lossy())
            .env("FORGE_TEST_SEMANTIC_LOG", semantic_log.to_string_lossy())
            .env("FORGE_TEST_COMMIT_MARKER", commit_marker.to_string_lossy());
        let added = run_forge_cli(
            &stub,
            &[
                "--format",
                "json",
                "provider",
                "add",
                PROVIDER,
                "--kind",
                "forgejo",
                "--base-url",
                &server.url(),
                "--token-env",
                TOKEN_ENV,
            ],
        );
        assert_eq!(added.code, 0, "stderr={}", added.stderr);

        Self {
            stub,
            server,
            state,
            owner: owner.to_string(),
            readme,
            license,
            reason,
            remote_sha,
            git_log,
            semantic_log,
            commit_marker,
            receipt,
            checkout,
        }
    }

    fn run(&self, owner_kind: &str, resume: bool) -> CmdOutput {
        let mut args = vec![
            "--provider".to_string(),
            PROVIDER.to_string(),
            "--repo".to_string(),
            format!("{}/widgets", self.owner),
            "--format".to_string(),
            "json".to_string(),
            "repo".to_string(),
            "bootstrap".to_string(),
            "--owner-kind".to_string(),
            owner_kind.to_string(),
            "--default-branch".to_string(),
            "main".to_string(),
            "--file".to_string(),
            self.readme.to_string_lossy().into_owned(),
            "--file".to_string(),
            self.license.to_string_lossy().into_owned(),
            "--message".to_string(),
            "chore: initialize repository".to_string(),
            "--reason-file".to_string(),
            self.reason.to_string_lossy().into_owned(),
        ];
        if resume {
            args.push("--resume".to_string());
        }
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        run_forge_cli(&self.stub, &refs)
    }

    fn set_env(&mut self, key: &str, value: &str) {
        self.stub.envs.retain(|(candidate, _)| candidate != key);
        self.stub.envs.push((key.to_string(), value.to_string()));
    }
}

fn handle_forgejo(
    request: &RecordedRequest,
    state: &Arc<Mutex<ForgeState>>,
    remote_sha: &Path,
    receipt: &Path,
    owner: &str,
) -> HttpResponse {
    let repo_path = format!("/api/v1/repos/{owner}/widgets");
    let branch_path = format!("{repo_path}/branches/main");
    if request.method == "GET" && request.path == "/api/v1/version" {
        return HttpResponse::new(200, r#"{"version":"15.0.5"}"#);
    }
    if request.method == "GET" && request.path == "/api/v1/user" {
        return HttpResponse::new(200, r#"{"login":"alice"}"#);
    }
    if request.method == "GET" && request.path == repo_path {
        let state = state.lock().expect("state");
        if !state.exists {
            return HttpResponse::new(404, r#"{"message":"not found"}"#);
        }
        let host = request.header_value("host").expect("Host header");
        return HttpResponse::new(
            200,
            serde_json::json!({
                "name": "widgets",
                "owner": {"login": owner},
                "private": true,
                "empty": !remote_sha.exists(),
                "default_branch": state.default_branch,
                "clone_url": format!("http://{host}/{owner}/widgets.git"),
                "html_url": format!("http://{host}/{owner}/widgets")
            })
            .to_string(),
        );
    }
    if request.method == "POST"
        && (request.path == "/api/v1/user/repos"
            || request.path == format!("/api/v1/orgs/{owner}/repos"))
    {
        let mut state = state.lock().expect("state");
        state.create_calls += 1;
        state.receipt_seen_before_create = receipt.is_file();
        state.exists = true;
        if state.create_ambiguous {
            return HttpResponse::new(500, "lost create response");
        }
        return HttpResponse::new(201, r#"{"name":"widgets"}"#);
    }
    if request.method == "GET" && request.path == branch_path {
        return match fs::read_to_string(remote_sha) {
            Ok(sha) => HttpResponse::new(
                200,
                serde_json::json!({"name":"main","commit":{"id":sha.trim()}}).to_string(),
            ),
            Err(_) => HttpResponse::new(404, r#"{"message":"branch not found"}"#),
        };
    }
    if request.method == "PATCH" && request.path == repo_path {
        let mut state = state.lock().expect("state");
        assert!(
            remote_sha.is_file(),
            "default branch set before branch existed"
        );
        state.patch_calls += 1;
        state.default_branch = "main".to_string();
        return HttpResponse::new(200, r#"{"default_branch":"main"}"#);
    }
    if request.method == "GET"
        && request
            .path
            .starts_with(&format!("{repo_path}/git/commits/"))
    {
        let state = state.lock().expect("state");
        let verification = serde_json::json!({
            "verified": state.signature_verified,
            "reason": if state.signature_verified {"valid"} else {"unsigned"}
        });
        let body = if state.legacy_top_level_signature {
            serde_json::json!({"sha": LOCAL_SHA, "verification": verification})
        } else {
            serde_json::json!({
                "sha": LOCAL_SHA,
                "commit": {"verification": verification}
            })
        };
        return HttpResponse::new(200, body.to_string());
    }
    HttpResponse::new(
        404,
        format!("unexpected {} {}", request.method, request.path),
    )
}

#[test]
fn forgejo_bootstrap_user_creates_private_empty_repo_and_verified_signed_root() {
    let fixture = Fixture::new("alice");
    let out = fixture.run("user", false);
    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(
        envelope["schema_version"],
        "cli.forge-cli.repo.bootstrap.v1"
    );
    assert_eq!(envelope["data"]["repository"], "alice/widgets");
    assert_eq!(envelope["data"]["default_branch"], "main");
    assert_eq!(envelope["data"]["root_commit_sha"], LOCAL_SHA);
    assert_eq!(envelope["data"]["signature_verified"], true);
    assert_eq!(envelope["data"]["private"], true);
    assert!(fixture.receipt.is_file());
    assert!(fixture.checkout.join("README.md").is_file());
    assert!(fixture.checkout.join("LICENSE").is_file());
    assert_eq!(
        fs::metadata(&fixture.receipt).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let state = fixture.state.lock().unwrap();
    assert_eq!(state.create_calls, 1);
    assert_eq!(state.patch_calls, 1);
    assert!(state.receipt_seen_before_create);
    drop(state);

    let requests = fixture.server.take_requests();
    let repo_gets_before_create = requests
        .iter()
        .take_while(|request| request.method != "POST")
        .filter(|request| request.method == "GET" && request.path.ends_with("/alice/widgets"))
        .count();
    assert_eq!(repo_gets_before_create, 2, "requests={requests:?}");
    let create = requests
        .iter()
        .find(|request| request.method == "POST")
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&create.body).unwrap();
    assert_eq!(body["name"], "widgets");
    assert_eq!(body["private"], true);
    assert_eq!(body["auto_init"], false);
    assert!(body.get("default_branch").is_none());

    let semantic = fs::read_to_string(&fixture.semantic_log).unwrap();
    assert!(semantic.contains("commit"));
    assert!(semantic.contains("--automation"));
    assert!(semantic.contains("chore: initialize repository"));
    let git = fs::read_to_string(&fixture.git_log).unwrap();
    let pushes = git
        .lines()
        .filter(|line| line.contains("push "))
        .collect::<Vec<_>>();
    assert_eq!(pushes.len(), 1, "git log:\n{git}");
    assert!(!pushes[0].contains("--force"));
    assert!(pushes[0].contains("--no-follow-tags"));
    assert!(pushes[0].contains("--no-push-option"));
}

#[test]
fn forgejo_bootstrap_org_uses_org_create_endpoint() {
    let fixture = Fixture::new("acme");
    let out = fixture.run("org", false);
    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    let requests = fixture.server.take_requests();
    assert!(
        requests.iter().any(|request| {
            request.method == "POST" && request.path == "/api/v1/orgs/acme/repos"
        })
    );
    assert!(
        !requests
            .iter()
            .any(|request| { request.method == "POST" && request.path == "/api/v1/user/repos" })
    );
    assert!(
        requests
            .iter()
            .any(|request| request.method == "GET" && request.path == "/api/v1/user")
    );
    let git = fs::read_to_string(&fixture.git_log).unwrap();
    assert!(git.contains("askpass_username=alice"), "git log:\n{git}");
    assert!(!git.contains("askpass_username=acme"), "git log:\n{git}");
}

#[test]
fn forgejo_bootstrap_rejects_top_level_only_signature_verification() {
    let fixture = Fixture::new("alice");
    fixture.state.lock().unwrap().legacy_top_level_signature = true;
    let out = fixture.run("user", false);
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    assert_eq!(
        parse_envelope(&out.stdout)["error"]["code"],
        "provider_signature_unverified"
    );
    assert!(fixture.receipt.is_file());
    assert!(fixture.checkout.is_dir());
}

#[test]
fn forgejo_bootstrap_rejects_preexisting_repo_before_local_or_remote_mutation() {
    let fixture = Fixture::new("alice");
    fixture.state.lock().unwrap().exists = true;
    let out = fixture.run("user", false);
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    assert_eq!(
        parse_envelope(&out.stdout)["error"]["code"],
        "repository_exists"
    );
    assert!(!fixture.git_log.exists());
    assert!(!fixture.semantic_log.exists());
    let requests = fixture.server.take_requests();
    assert!(
        !requests
            .iter()
            .any(|request| matches!(request.method.as_str(), "POST" | "PATCH" | "DELETE"))
    );
}

#[test]
fn forgejo_bootstrap_reconciles_ambiguous_create_and_push_without_retry() {
    let mut fixture = Fixture::new("alice");
    fixture.state.lock().unwrap().create_ambiguous = true;
    fixture
        .stub
        .envs
        .push(("FORGE_TEST_PUSH_MODE".into(), "ambiguous".into()));
    let out = fixture.run("user", false);
    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    assert_eq!(parse_envelope(&out.stdout)["data"]["reconciled"], true);
    assert_eq!(fixture.state.lock().unwrap().create_calls, 1);
    let git = fs::read_to_string(&fixture.git_log).unwrap();
    assert_eq!(git.lines().filter(|line| line.contains("push ")).count(), 1);
}

#[test]
fn forgejo_bootstrap_is_idempotent_for_matching_sha_and_fails_on_remote_drift() {
    let fixture = Fixture::new("alice");
    let first = fixture.run("user", false);
    assert_eq!(
        first.code, 0,
        "stdout={} stderr={}",
        first.stdout, first.stderr
    );
    let request_count = fixture.server.take_requests().len();
    let push_count = fs::read_to_string(&fixture.git_log)
        .unwrap()
        .lines()
        .filter(|line| line.contains("push "))
        .count();

    let resumed = fixture.run("user", true);
    assert_eq!(
        resumed.code, 0,
        "stdout={} stderr={}",
        resumed.stdout, resumed.stderr
    );
    assert_eq!(parse_envelope(&resumed.stdout)["data"]["idempotent"], true);
    assert_eq!(fixture.state.lock().unwrap().create_calls, 1);
    let resumed_push_count = fs::read_to_string(&fixture.git_log)
        .unwrap()
        .lines()
        .filter(|line| line.contains("push "))
        .count();
    assert_eq!(resumed_push_count, push_count);
    assert!(fixture.server.take_requests().len() < request_count);

    fs::write(&fixture.remote_sha, format!("{DRIFT_SHA}\n")).unwrap();
    let drift = fixture.run("user", true);
    assert_eq!(
        drift.code, 65,
        "stdout={} stderr={}",
        drift.stdout, drift.stderr
    );
    assert_eq!(
        parse_envelope(&drift.stdout)["error"]["code"],
        "remote_drift"
    );
    assert!(fixture.receipt.is_file());
    assert!(fixture.checkout.is_dir());
}

#[test]
fn forgejo_bootstrap_retains_recovery_state_after_unsigned_or_failed_push() {
    let mut unsigned = Fixture::new("alice");
    unsigned
        .stub
        .envs
        .push(("FORGE_TEST_SIGNATURE".into(), "U".into()));
    let out = unsigned.run("user", false);
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    assert_eq!(
        parse_envelope(&out.stdout)["error"]["code"],
        "commit_signature_unverified"
    );
    assert!(unsigned.receipt.is_file());
    assert!(unsigned.checkout.is_dir());
    assert!(!unsigned.remote_sha.exists());
    assert_eq!(unsigned.state.lock().unwrap().patch_calls, 0);

    let mut failed_push = Fixture::new("alice");
    failed_push
        .stub
        .envs
        .push(("FORGE_TEST_PUSH_MODE".into(), "fail".into()));
    let out = failed_push.run("user", false);
    assert_eq!(out.code, 1, "stdout={} stderr={}", out.stdout, out.stderr);
    assert_eq!(
        parse_envelope(&out.stdout)["error"]["code"],
        "bootstrap_push_failed"
    );
    assert!(failed_push.receipt.is_file());
    assert!(failed_push.checkout.is_dir());
    assert_eq!(failed_push.state.lock().unwrap().create_calls, 1);
    assert!(
        !failed_push
            .server
            .take_requests()
            .iter()
            .any(|request| request.method == "DELETE")
    );
}

#[test]
fn forgejo_bootstrap_recovers_a_valid_signed_root_from_the_crash_window() {
    let mut fixture = Fixture::new("alice");
    fixture.set_env("FORGE_TEST_SIGNATURE", "U");
    let interrupted = fixture.run("user", false);
    assert_eq!(interrupted.code, 65, "stdout={}", interrupted.stdout);
    let semantic_before = fs::read_to_string(&fixture.semantic_log).unwrap();
    assert_eq!(semantic_before.lines().count(), 1);

    fixture.set_env("FORGE_TEST_SIGNATURE", "G");
    let resumed = fixture.run("user", true);
    assert_eq!(
        resumed.code, 0,
        "stdout={} stderr={}",
        resumed.stdout, resumed.stderr
    );
    assert_eq!(
        parse_envelope(&resumed.stdout)["data"]["root_commit_sha"],
        LOCAL_SHA
    );
    let semantic_after = fs::read_to_string(&fixture.semantic_log).unwrap();
    assert_eq!(
        semantic_after.lines().count(),
        1,
        "resume must recover HEAD without invoking semantic-commit again"
    );
}

#[test]
fn forgejo_bootstrap_rejects_a_signed_head_with_a_different_committed_tree() {
    let mut fixture = Fixture::new("alice");
    fixture.set_env("FORGE_TEST_SIGNATURE", "U");
    let interrupted = fixture.run("user", false);
    assert_eq!(interrupted.code, 65, "stdout={}", interrupted.stdout);

    fixture.set_env("FORGE_TEST_SIGNATURE", "G");
    fixture.set_env("FORGE_TEST_TREE_PATHS", "README.md\\0");
    let semantic_before = fs::read_to_string(&fixture.semantic_log).unwrap();
    let resumed = fixture.run("user", true);
    assert_eq!(resumed.code, 65, "stdout={}", resumed.stdout);
    assert_eq!(
        parse_envelope(&resumed.stdout)["error"]["code"],
        "bootstrap_checkout_mismatch"
    );
    assert_eq!(
        fs::read_to_string(&fixture.semantic_log).unwrap(),
        semantic_before,
        "a mismatched signed HEAD tree must fail before semantic-commit"
    );
}

#[test]
fn forgejo_bootstrap_resumes_only_a_safe_exact_no_head_partial_checkout() {
    let mut safe = Fixture::new("alice");
    safe.set_env("FORGE_TEST_NO_HEAD_UNTIL_COMMIT", "1");
    safe.set_env("FORGE_TEST_SEMANTIC_MODE", "fail_before_commit");
    let interrupted = safe.run("user", false);
    assert_ne!(interrupted.code, 0, "stdout={}", interrupted.stdout);
    assert!(!safe.commit_marker.exists());

    safe.set_env("FORGE_TEST_SEMANTIC_MODE", "success");
    let resumed = safe.run("user", true);
    assert_eq!(
        resumed.code, 0,
        "stdout={} stderr={}",
        resumed.stdout, resumed.stderr
    );
    assert_eq!(
        fs::read_to_string(&safe.semantic_log)
            .unwrap()
            .lines()
            .count(),
        2,
        "safe staged recovery retries semantic-commit exactly once"
    );

    let mut unsafe_checkout = Fixture::new("alice");
    unsafe_checkout.set_env("FORGE_TEST_NO_HEAD_UNTIL_COMMIT", "1");
    unsafe_checkout.set_env("FORGE_TEST_SEMANTIC_MODE", "fail_before_commit");
    let interrupted = unsafe_checkout.run("user", false);
    assert_ne!(interrupted.code, 0, "stdout={}", interrupted.stdout);
    unsafe_checkout.set_env("FORGE_TEST_SEMANTIC_MODE", "success");
    unsafe_checkout.set_env("FORGE_TEST_PARTIAL_STATUS", "?? README.md\\0");
    let semantic_before = fs::read_to_string(&unsafe_checkout.semantic_log).unwrap();
    let rejected = unsafe_checkout.run("user", true);
    assert_eq!(rejected.code, 65, "stdout={}", rejected.stdout);
    assert_eq!(
        parse_envelope(&rejected.stdout)["error"]["code"],
        "bootstrap_checkout_mismatch"
    );
    assert_eq!(
        fs::read_to_string(&unsafe_checkout.semantic_log).unwrap(),
        semantic_before,
        "unsafe partial checkout must fail before semantic-commit"
    );
}

#[test]
fn forgejo_bootstrap_crash_window_recovery_rejects_checkout_mismatch() {
    for mismatch in ["file", "branch", "ancestry", "signature"] {
        let mut fixture = Fixture::new("alice");
        fixture.set_env("FORGE_TEST_SIGNATURE", "U");
        let interrupted = fixture.run("user", false);
        assert_eq!(interrupted.code, 65, "mismatch={mismatch}");
        match mismatch {
            "file" => fs::write(fixture.checkout.join("README.md"), "drift\n").unwrap(),
            "branch" => fixture.set_env("FORGE_TEST_CHECKOUT_BRANCH", "other"),
            "ancestry" => fixture.set_env("FORGE_TEST_PARENT_SHA", DRIFT_SHA),
            "signature" => {}
            _ => unreachable!(),
        }
        if mismatch != "signature" {
            fixture.set_env("FORGE_TEST_SIGNATURE", "G");
        }
        let semantic_before = fs::read_to_string(&fixture.semantic_log).unwrap();
        let resumed = fixture.run("user", true);
        assert_eq!(
            resumed.code, 65,
            "mismatch={mismatch}; stdout={}",
            resumed.stdout
        );
        let code = parse_envelope(&resumed.stdout)["error"]["code"]
            .as_str()
            .unwrap()
            .to_string();
        if mismatch == "signature" {
            assert_eq!(code, "commit_signature_unverified");
        } else {
            assert_eq!(code, "bootstrap_checkout_mismatch");
        }
        assert_eq!(
            fs::read_to_string(&fixture.semantic_log).unwrap(),
            semantic_before,
            "mismatch={mismatch}: fail before a second semantic-commit invocation"
        );
    }
}

#[test]
fn forgejo_bootstrap_rejects_non_regular_inputs_before_provider_mutation() {
    let fixture = Fixture::new("alice");
    let link = fixture.stub.tempdir.path().join("linked.md");
    symlink(&fixture.readme, &link).unwrap();
    let link_text = link.to_string_lossy().into_owned();
    let reason_text = fixture.reason.to_string_lossy().into_owned();
    let repo = format!("{}/widgets", fixture.owner);
    let out = run_forge_cli(
        &fixture.stub,
        &[
            "--provider",
            PROVIDER,
            "--repo",
            &repo,
            "--format",
            "json",
            "repo",
            "bootstrap",
            "--owner-kind",
            "user",
            "--default-branch",
            "main",
            "--file",
            &link_text,
            "--message",
            "chore: initialize repository",
            "--reason-file",
            &reason_text,
        ],
    );
    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    assert_eq!(
        parse_envelope(&out.stdout)["error"]["code"],
        "bootstrap_file_invalid"
    );
    assert!(fixture.server.take_requests().is_empty());
}

#[test]
fn forgejo_bootstrap_operation_effect_is_an_explicit_network_mutation() {
    let fixture = Fixture::new("alice");
    let repo = format!("{}/widgets", fixture.owner);
    let readme = fixture.readme.to_string_lossy().into_owned();
    let reason = fixture.reason.to_string_lossy().into_owned();
    let out = run_forge_cli(
        &fixture.stub,
        &[
            "operation-effect",
            "--format",
            "json",
            "--",
            "--provider",
            PROVIDER,
            "--repo",
            &repo,
            "repo",
            "bootstrap",
            "--owner-kind",
            "user",
            "--default-branch",
            "main",
            "--file",
            &readme,
            "--message",
            "chore: initialize repository",
            "--reason-file",
            &reason,
        ],
    );
    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    let descriptor = parse_envelope(&out.stdout);
    assert_eq!(descriptor["data"]["operation"], "repo.bootstrap");
    assert_eq!(descriptor["data"]["effect"], "mutation");
    assert_eq!(descriptor["data"]["provider_effect"], "network_write");
}
