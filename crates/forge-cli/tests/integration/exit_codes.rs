//! Sprint 1 exit-code matrix. Covers the five paths reachable in Sprint 1:
//! SUCCESS, USAGE, UNAVAILABLE, SOFTWARE, plus a second USAGE (unknown
//! subcommand) for completeness.
//!
//! `RUNTIME` is deferred to Sprint 3 (check failures); `DATA` is deferred to
//! Sprint 2 (lock-down validations on `pr create`) and Sprint 4 (config
//! loader).

use nils_common::cli_contract::exit;
use pretty_assertions::assert_eq;

use super::support::{StubEnv, run_forge_cli};

const GH_AUTH_OK: &str = "\
#!/bin/sh
cat <<'EOF' 1>&2
github.com
  ✓ Logged in to github.com account testuser-gh (keyring)
EOF
";

#[test]
fn exit_success_for_auth_status_with_stubbed_gh() {
    let stub = StubEnv::new().gh_stub(GH_AUTH_OK);
    let out = run_forge_cli(
        &stub,
        &["--provider", "github", "--format", "json", "auth", "status"],
    );
    assert_eq!(
        out.code,
        exit::SUCCESS,
        "auth status should exit SUCCESS; stderr={}",
        out.stderr
    );
}

#[test]
fn exit_usage_for_unknown_subcommand() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["bogus"]);
    assert_eq!(
        out.code,
        exit::USAGE,
        "unknown subcommand should exit USAGE 64; stderr={}",
        out.stderr
    );
}

#[test]
fn exit_usage_for_unknown_provider_host() {
    // Force a provider via remote URL parse — without a real git remote and
    // no --provider flag we expect USAGE (provider_unsupported).
    let stub = StubEnv::new()
        .env("HOME", "/tmp")
        .env("GIT_DIR", "/dev/null/nonexistent");
    let out = run_forge_cli(&stub, &["--format", "json", "auth", "status"]);
    assert_eq!(
        out.code,
        exit::USAGE,
        "missing provider must exit USAGE 64; stderr={}",
        out.stderr
    );
}

#[test]
fn exit_unavailable_for_missing_gh_backend() {
    let stub = StubEnv::new().env("FORGE_CLI_GH_BIN", "/tmp/forge-cli-nonexistent-binary-xyz");
    let out = run_forge_cli(
        &stub,
        &["--provider", "github", "--format", "json", "auth", "status"],
    );
    assert_eq!(
        out.code,
        exit::UNAVAILABLE,
        "missing gh should exit UNAVAILABLE 69; stderr={}",
        out.stderr
    );
}

#[test]
fn exit_software_for_mangled_repo_view_json() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho 'not json'\n");
    let out = run_forge_cli(
        &stub,
        &["--provider", "github", "--format", "json", "repo", "view"],
    );
    assert_eq!(
        out.code,
        exit::SOFTWARE,
        "mangled repo view JSON should exit SOFTWARE 70; stderr={}",
        out.stderr
    );
}

#[test]
fn exit_codes_use_shared_constants_only() {
    // Sanity: the test file imports the shared constants exclusively. This
    // assertion is symbolic — if a future contributor inlines a numeric
    // literal (e.g. `assert_eq!(code, 0)`) the contract lint will catch it,
    // but we also pin the constants here for documentation.
    assert_eq!(exit::SUCCESS, 0);
    assert_eq!(exit::RUNTIME, 1);
    assert_eq!(exit::USAGE, 64);
    assert_eq!(exit::DATA, 65);
    assert_eq!(exit::UNAVAILABLE, 69);
    assert_eq!(exit::SOFTWARE, 70);
}
