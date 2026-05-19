//! End-to-end `auth status` integration covering both backends with stub
//! binaries.

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

const GH_AUTH_STDERR: &str = "\
github.com
  ✓ Logged in to github.com account graysurf (keyring)
  - Active account: true
  - Token scopes: 'repo', 'read:org'
";

const GLAB_AUTH_STDERR: &str = "\
gitlab.com
  ✓ Logged in to gitlab.com as graysurf (~/.config/glab-cli/config.yml)
  ✓ Git operations for gitlab.com configured to use ssh protocol.
";

fn gh_stub_script(stderr: &str) -> String {
    format!(
        "#!/bin/sh\n\
         cat <<'EOF' 1>&2\n{stderr}EOF\n"
    )
}

#[test]
fn auth_status_github_returns_normalized_envelope() {
    let stub = StubEnv::new().gh_stub(&gh_stub_script(GH_AUTH_STDERR));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "auth",
            "status",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.auth.status.v1");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["provider"], "github");
    assert_eq!(envelope["data"]["host"], "github.com");
    assert_eq!(envelope["data"]["user"], "graysurf");
    assert_eq!(
        envelope["data"]["scopes"].as_array().unwrap().len(),
        2
    );
}

#[test]
fn auth_status_gitlab_returns_normalized_envelope() {
    let stub = StubEnv::new().glab_stub(&gh_stub_script(GLAB_AUTH_STDERR));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "auth",
            "status",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.auth.status.v1");
    assert_eq!(envelope["data"]["provider"], "gitlab");
    assert_eq!(envelope["data"]["host"], "gitlab.com");
    assert_eq!(envelope["data"]["user"], "graysurf");
    // glab auth status does not surface scopes.
    assert_eq!(
        envelope["data"]["scopes"].as_array().unwrap().len(),
        0
    );
}

#[test]
fn auth_status_envelope_is_parity_between_backends() {
    // Identical user + host on both backends; envelope differs only in
    // `data.provider` and `data.scopes` (which the spec admits since glab
    // does not surface scopes).
    let gh = StubEnv::new().gh_stub(&gh_stub_script(GH_AUTH_STDERR));
    let glab = StubEnv::new().glab_stub(&gh_stub_script(GLAB_AUTH_STDERR));
    let gh_out = run_forge_cli(
        &gh,
        &["--provider", "github", "--format", "json", "auth", "status"],
    );
    let glab_out = run_forge_cli(
        &glab,
        &["--provider", "gitlab", "--format", "json", "auth", "status"],
    );
    let gh_env = parse_envelope(&gh_out.stdout);
    let glab_env = parse_envelope(&glab_out.stdout);
    assert_eq!(gh_env["schema_version"], glab_env["schema_version"]);
    assert_eq!(gh_env["ok"], glab_env["ok"]);
    assert_eq!(gh_env["data"]["user"], glab_env["data"]["user"]);
}
