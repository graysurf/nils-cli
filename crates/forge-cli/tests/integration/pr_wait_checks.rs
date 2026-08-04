//! End-to-end `pr wait-checks` integration tests.
//!
//! Each test wires the gh stub to emit a *sequence* of check-snapshot JSON
//! payloads, one per poll, so the polling loop's exit-code matrix can be
//! exercised deterministically against tiny `--interval`/`--timeout` values:
//!
//! - succeeds on third poll → `SUCCESS 0`
//! - first poll required failure → `RUNTIME 1` + `error.kind=checks_failed`
//! - times out after N intervals → `UNAVAILABLE 69` + `error.kind=checks_timeout`

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

/// Build a dispatching gh stub that maintains a per-invocation counter file
/// and returns the Nth element of a snapshot sequence each time `pr checks`
/// is invoked. Once the counter passes the last in-range index it clamps to
/// the final snapshot, so callers that poll longer than the prepared
/// sequence keep seeing the trailing snapshot (essential for timeout tests).
fn gh_sequence_stub(stub: &StubEnv, sequence: &[&str]) {
    assert!(!sequence.is_empty(), "sequence must have at least one snap");
    for (idx, snap) in sequence.iter().enumerate() {
        let path = stub.tempdir.path().join(format!("snap-{idx}.json"));
        std::fs::write(&path, snap).expect("write snap");
    }
    let counter = stub.tempdir.path().join("counter");
    std::fs::write(&counter, "0").expect("write counter");
    let dir = stub.tempdir.path().to_string_lossy().to_string();
    let max_idx = sequence.len() - 1;
    let body = format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "pr checks")
    counter="{dir}/counter"
    idx=$(cat "$counter")
    case " $* " in
      *" --required "*)
        idx=$((idx - 1))
        if [ "$idx" -lt 0 ]; then
            idx=0
        fi
        ;;
      *)
        next=$((idx + 1))
        echo "$next" > "$counter"
        ;;
    esac
    if [ "$idx" -ge "{max_idx}" ]; then
        eff="{max_idx}"
    else
        eff="$idx"
    fi
    cat "{dir}/snap-$eff.json"
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
    );
    stub.write_stub("gh", &body);
}

const PENDING_SNAP: &str =
    r#"[{"name":"build","bucket":"pending","state":"IN_PROGRESS","link":"https://ci/1"}]"#;
const SUCCESS_SNAP: &str =
    r#"[{"name":"build","bucket":"pass","state":"COMPLETED","link":"https://ci/1"}]"#;
const FAILURE_SNAP: &str =
    r#"[{"name":"build","bucket":"fail","state":"COMPLETED","link":"https://ci/1"}]"#;

#[test]
fn pr_wait_checks_succeeds_when_terminal_on_third_poll() {
    let mut stub = StubEnv::new();
    gh_sequence_stub(&stub, &[PENDING_SNAP, PENDING_SNAP, SUCCESS_SNAP]);
    let gh_path = stub.tempdir.path().join("gh");
    stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "wait-checks",
            "1",
            "--interval",
            "10ms",
            "--timeout",
            "20s",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.checks.v1");
    assert_eq!(env["data"]["state"], "success");
    assert!(env["data"]["duration_ms"].as_u64().is_some());
}

#[test]
fn pr_wait_checks_required_failure_exits_runtime_with_kind_checks_failed() {
    let mut stub = StubEnv::new();
    gh_sequence_stub(&stub, &[FAILURE_SNAP]);
    let gh_path = stub.tempdir.path().join("gh");
    stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "wait-checks",
            "1",
            "--interval",
            "10ms",
            "--timeout",
            "1s",
        ],
    );
    assert_eq!(out.code, 1, "expected RUNTIME 1, stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "checks_failed");
    // Payload still carries the snapshot so callers can introspect.
    assert_eq!(env["data"]["state"], "failure");
    assert_eq!(env["data"]["failed"].as_array().unwrap().len(), 1);
}

#[test]
fn pr_wait_checks_timeout_exits_unavailable_with_kind_checks_timeout() {
    let mut stub = StubEnv::new();
    gh_sequence_stub(
        &stub,
        &[PENDING_SNAP, PENDING_SNAP, PENDING_SNAP, PENDING_SNAP],
    );
    let gh_path = stub.tempdir.path().join("gh");
    stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "wait-checks",
            "1",
            "--interval",
            "20ms",
            "--timeout",
            "100ms",
        ],
    );
    assert_eq!(
        out.code, 69,
        "expected UNAVAILABLE 69 on timeout, stderr={}",
        out.stderr
    );
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "checks_timeout");
    let duration_ms = env["data"]["duration_ms"].as_u64().expect("duration_ms");
    // Sleeping at least one interval before the timeout fires means
    // duration_ms must be > 0 and within a reasonable upper bound.
    assert!(duration_ms >= 20, "duration_ms={duration_ms}");
}

#[test]
fn pr_wait_checks_dry_run_renders_plan_envelope_without_calling_backend() {
    // The stub should never run during dry-run; configure it to exit 99 to
    // assert that.
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho 'should not run' >&2\nexit 99\n");

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "wait-checks",
            "42",
            "--interval",
            "5s",
            "--timeout",
            "1m",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.checks.v1");
    let timeout_ms = env["data"]["timeout_ms"].as_u64().expect("timeout_ms");
    let interval_ms = env["data"]["interval_ms"].as_u64().expect("interval_ms");
    assert_eq!(timeout_ms, 60_000);
    assert_eq!(interval_ms, 5_000);
}

#[test]
fn pr_wait_checks_gitlab_api_succeeds_without_version_probe() {
    let stub = StubEnv::new().glab_stub(
        r#"#!/bin/sh
set -e
case "$1" in
  "--version")
    echo "version probe should not run for API-backed wait-checks" >&2
    exit 99
    ;;
  "mr")
    if [ "$2" = "view" ]; then
      cat <<'EOF'
{
  "iid": 42,
  "web_url": "https://gitlab.com/group/project/-/merge_requests/42",
  "source_branch": "feat/sample",
  "target_branch": "main",
  "sha": "abc123",
  "head_pipeline": {
    "id": 99,
    "status": "success",
    "web_url": "https://gitlab.com/group/project/-/pipelines/99"
  }
}
EOF
      exit 0
    fi
    ;;
  "api")
    case "$*" in
      *"projects/group%2Fproject/pipelines/99/jobs?per_page=100"*)
        cat <<'EOF'
[
  {
    "name": "build",
    "stage": "test",
    "status": "success",
    "allow_failure": false,
    "web_url": "https://gitlab.com/group/project/-/jobs/1"
  }
]
EOF
        exit 0
        ;;
    esac
    ;;
esac
echo "stub: unexpected glab args: $*" >&2
exit 99
"#,
    );

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "pr",
            "wait-checks",
            "42",
            "--interval",
            "10ms",
            "--timeout",
            "1s",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.checks.v1");
    assert_eq!(env["data"]["provider"], "gitlab");
    assert_eq!(env["data"]["state"], "success");
    assert_eq!(env["data"]["required_count"], 1);
}

const EMPTY_SNAP: &str = "[]";

/// The emitter arm for a head that never registers a check: DATA 65 and its own
/// kind, not the UNAVAILABLE 69 a genuine timeout gets. The two need different
/// fixes, so automation must be able to tell them apart.
#[test]
fn pr_wait_checks_expires_as_not_registered_when_nothing_is_ever_reported() {
    let mut stub = StubEnv::new();
    gh_sequence_stub(&stub, &[EMPTY_SNAP]);
    let gh_path = stub.tempdir.path().join("gh");
    stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "wait-checks",
            "1",
            "--interval",
            "10ms",
            "--timeout",
            "30ms",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "checks_not_registered");
    // The snapshot is reported verbatim, so `data.state` is still "success"
    // while `ok` is false. Consumers must gate on ok / error.kind — this
    // asserts the combination so any future normalization is deliberate.
    assert_eq!(env["ok"], false);
    assert_eq!(env["data"]["state"], "success");
    assert_eq!(env["data"]["required_count"], 0);
}

/// The reason-free opt-out on this non-mutating op: a project that configures
/// no checks terminates immediately instead of burning the whole budget.
#[test]
fn pr_wait_checks_allow_no_checks_completes_immediately() {
    let mut stub = StubEnv::new();
    gh_sequence_stub(&stub, &[EMPTY_SNAP]);
    let gh_path = stub.tempdir.path().join("gh");
    stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "wait-checks",
            "1",
            "--interval",
            "10ms",
            "--timeout",
            "30s",
            "--allow-no-checks",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["required_count"], 0);
}
