//! End-to-end tests for the TTL-zero required-check gate.
//!
//! These exercise [`forge_cli::ops::required_check_gate::ensure_required_checks_green`]
//! against a real [`ProcessRunner`] backed by a generated gh stub. Each call
//! to the helper must re-fetch from the provider — there is no caching across
//! atoms — so the tests pin that property by counting stub invocations.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use forge_cli::backend::ProcessRunner;
use forge_cli::cli::GlobalFlags;
use forge_cli::ops::required_check_gate::ensure_required_checks_green;
use forge_cli::provider::{DetectionSource, Provider, ProviderContext};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

/// FORGE_CLI_GH_BIN is process-wide; serialize tests that mutate it so
/// parallel test execution can't cross-contaminate stub paths.
fn lock_env() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn write_stub(dir: &TempDir, body: &str) -> PathBuf {
    let path = dir.path().join("gh");
    fs::write(&path, body).expect("write gh stub");
    let mut perm = fs::metadata(&path).expect("metadata").permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).expect("chmod");
    path
}

fn github_ctx() -> ProviderContext {
    ProviderContext {
        provider: Provider::GitHub,
        host: "github.com".into(),
        source: DetectionSource::Flag,
    }
}

fn run_globals() -> GlobalFlags {
    GlobalFlags {
        format: None,
        remote: "origin".into(),
        provider: None,
        repo: None,
        dry_run: false,
    }
}

const SUCCESS_JSON: &str = r#"[{"name":"build","bucket":"pass","conclusion":"success","isRequired":true,"link":"https://ci/1"},{"name":"test","bucket":"pass","conclusion":"success","isRequired":true,"link":"https://ci/2"}]"#;
const PENDING_JSON: &str = r#"[{"name":"build","bucket":"pass","conclusion":"success","isRequired":true,"link":"https://ci/1"},{"name":"test","bucket":"pending","conclusion":"","isRequired":true,"link":"https://ci/2"}]"#;
const FAILURE_JSON: &str = r#"[{"name":"build","bucket":"pass","conclusion":"success","isRequired":true,"link":"https://ci/1"},{"name":"test","bucket":"fail","conclusion":"failure","isRequired":true,"link":"https://ci/2"}]"#;

#[test]
fn all_required_green_returns_payload_with_runtime_runner() {
    let tmp = TempDir::new().unwrap();
    write_stub(
        &tmp,
        &format!(
            "#!/bin/sh\nset -e\ncase \"$1 $2\" in\n  \"pr checks\")\n    cat <<'EOF'\n{SUCCESS_JSON}\nEOF\n    ;;\n  *)\n    echo \"stub: unexpected gh args: $*\" >&2; exit 99;;\nesac\n"
        ),
    );
    // SAFETY: process-wide env var; tests within this binary share state but
    // the helper reads the var each time the runner spawns gh, so a stale
    // value would only break parallel tests in this module.
    let _guard = lock_env();
    unsafe {
        std::env::set_var("FORGE_CLI_GH_BIN", tmp.path().join("gh"));
    }

    let runner = ProcessRunner;
    let globals = run_globals();
    let ctx = github_ctx();
    let snap = ensure_required_checks_green(&runner, &globals, &ctx, "42")
        .expect("required checks must be green");

    assert_eq!(snap.state, "success");
    assert_eq!(snap.required_count, 2);
    assert_eq!(snap.success_count, 2);
}

#[test]
fn pending_required_check_exits_data_with_kind_checks_pending() {
    let tmp = TempDir::new().unwrap();
    write_stub(
        &tmp,
        &format!(
            "#!/bin/sh\nset -e\ncase \"$1 $2\" in\n  \"pr checks\")\n    cat <<'EOF'\n{PENDING_JSON}\nEOF\n    ;;\n  *)\n    echo \"stub: unexpected gh args: $*\" >&2; exit 99;;\nesac\n"
        ),
    );
    let _guard = lock_env();
    unsafe {
        std::env::set_var("FORGE_CLI_GH_BIN", tmp.path().join("gh"));
    }

    let runner = ProcessRunner;
    let err = ensure_required_checks_green(&runner, &run_globals(), &github_ctx(), "42")
        .expect_err("must surface pending");
    assert_eq!(err.kind(), "checks_pending");
    assert_eq!(err.exit_code(), 65);
}

#[test]
fn failing_required_check_exits_runtime_with_kind_checks_failed() {
    let tmp = TempDir::new().unwrap();
    write_stub(
        &tmp,
        &format!(
            "#!/bin/sh\nset -e\ncase \"$1 $2\" in\n  \"pr checks\")\n    cat <<'EOF'\n{FAILURE_JSON}\nEOF\n    ;;\n  *)\n    echo \"stub: unexpected gh args: $*\" >&2; exit 99;;\nesac\n"
        ),
    );
    let _guard = lock_env();
    unsafe {
        std::env::set_var("FORGE_CLI_GH_BIN", tmp.path().join("gh"));
    }

    let runner = ProcessRunner;
    let err = ensure_required_checks_green(&runner, &run_globals(), &github_ctx(), "42")
        .expect_err("must surface failure");
    assert_eq!(err.kind(), "checks_failed");
    assert_eq!(err.exit_code(), 1);
}

#[test]
fn helper_refetches_every_call_no_caching_across_atoms() {
    // Sprint 4's contract: even if pr.wait-checks finished <1s ago, the gate
    // must re-fetch. We prove that by counting stub invocations: two helper
    // calls in the same test should produce two spawns of gh.
    let tmp = TempDir::new().unwrap();
    let counter = tmp.path().join("counter");
    fs::write(&counter, "0").unwrap();
    let stub_dir = tmp.path().to_string_lossy().to_string();
    let body = format!(
        "#!/bin/sh\nset -e\ncase \"$1 $2\" in\n  \"pr checks\")\n    n=$(cat \"{stub_dir}/counter\")\n    echo $((n + 1)) > \"{stub_dir}/counter\"\n    cat <<'EOF'\n{SUCCESS_JSON}\nEOF\n    ;;\n  *)\n    echo \"stub: unexpected gh args: $*\" >&2; exit 99;;\nesac\n"
    );
    write_stub(&tmp, &body);
    let _guard = lock_env();
    unsafe {
        std::env::set_var("FORGE_CLI_GH_BIN", tmp.path().join("gh"));
    }

    let runner = ProcessRunner;
    let globals = run_globals();
    let ctx = github_ctx();
    ensure_required_checks_green(&runner, &globals, &ctx, "42").expect("call 1 must pass");
    ensure_required_checks_green(&runner, &globals, &ctx, "42").expect("call 2 must pass");

    let final_count: u32 = fs::read_to_string(&counter)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(final_count, 2, "gh must be re-invoked on every gate call");
}
