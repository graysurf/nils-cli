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
    if [ "$idx" -ge "{max_idx}" ]; then
        eff="{max_idx}"
    else
        eff="$idx"
    fi
    next=$((idx + 1))
    echo "$next" > "$counter"
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

const PENDING_SNAP: &str = r#"[{"name":"build","bucket":"pending","conclusion":"","isRequired":true,"link":"https://ci/1"}]"#;
const SUCCESS_SNAP: &str = r#"[{"name":"build","bucket":"pass","conclusion":"success","isRequired":true,"link":"https://ci/1"}]"#;
const FAILURE_SNAP: &str = r#"[{"name":"build","bucket":"fail","conclusion":"failure","isRequired":true,"link":"https://ci/1"}]"#;

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
            "5s",
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
