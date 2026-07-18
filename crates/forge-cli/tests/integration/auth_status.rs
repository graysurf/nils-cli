//! End-to-end `auth status` integration covering both backends with stub
//! binaries.

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

const GH_AUTH_STDERR: &str = "\
github.com
  ✓ Logged in to github.com account testuser-gh (keyring)
  - Active account: true
  - Token scopes: 'repo', 'read:org'
";

const GLAB_AUTH_STDERR: &str = "\
gitlab.com
  ✓ Logged in to gitlab.com as testuser-glab (~/.config/glab-cli/config.yml)
  ✓ Git operations for gitlab.com configured to use ssh protocol.
";

fn gh_stub_script(stderr: &str) -> String {
    format!(
        "#!/bin/sh\n\
         cat <<'EOF' 1>&2\n{stderr}EOF\n"
    )
}

fn argv_bound_stub(expected: &str, stderr: &str) -> String {
    format!(
        "#!/bin/sh\n\
         [ \"$*\" = \"{expected}\" ] || {{ echo \"unexpected argv: $*\" >&2; exit 97; }}\n\
         cat <<'EOF' 1>&2\n{stderr}EOF\n"
    )
}

#[test]
fn auth_status_github_returns_normalized_envelope() {
    let stub = StubEnv::new().gh_stub(&gh_stub_script(GH_AUTH_STDERR));
    let out = run_forge_cli(
        &stub,
        &["--provider", "github", "--format", "json", "auth", "status"],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.auth.status.v1");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["provider"], "github");
    assert_eq!(envelope["data"]["host"], "github.com");
    assert!(envelope["data"]["user"].is_string());
    assert_eq!(envelope["data"]["scopes"].as_array().unwrap().len(), 2);
}

#[test]
fn auth_status_ignores_irrelevant_repo_shape_and_overrides_ambient_host() {
    let stub = StubEnv::new()
        .env("GH_HOST", "attacker.ghe.example")
        .gh_stub(&format!(
            "#!/bin/sh\n[ \"$GH_HOST\" = \"github.com\" ] || {{ echo \"unexpected GH_HOST: $GH_HOST\" >&2; exit 97; }}\ncat <<'EOF' 1>&2\n{GH_AUTH_STDERR}EOF\n"
        ));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "owner/nested/repo",
            "--format",
            "json",
            "auth",
            "status",
        ],
    );
    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["data"]["host"], "github.com");
}

#[test]
fn auth_status_github_enterprise_binds_hostname() {
    let stderr =
        "internal.ghe.com\n  ✓ Logged in to internal.ghe.com account testuser-gh (keyring)\n";
    let stub = StubEnv::new().gh_stub(&argv_bound_stub(
        "auth status --hostname internal.ghe.com",
        stderr,
    ));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--host",
            "internal.ghe.com",
            "--format",
            "json",
            "auth",
            "status",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["data"]["host"], "internal.ghe.com");
}

#[test]
fn auth_status_gitlab_returns_normalized_envelope() {
    let stub = StubEnv::new().glab_stub(&gh_stub_script(GLAB_AUTH_STDERR));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--host",
            "gitlab.com",
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
    assert!(envelope["data"]["user"].is_string());
    // glab auth status does not surface scopes.
    assert_eq!(envelope["data"]["scopes"].as_array().unwrap().len(), 0);
}

#[test]
fn auth_status_envelope_is_parity_between_backends() {
    // Envelope differs only in `data.provider` and `data.scopes` (which the
    // spec admits since glab does not surface scopes). The username is not
    // compared because each backend has its own identity surface.
    let gh = StubEnv::new().gh_stub(&gh_stub_script(GH_AUTH_STDERR));
    let glab = StubEnv::new().glab_stub(&gh_stub_script(GLAB_AUTH_STDERR));
    let gh_out = run_forge_cli(
        &gh,
        &["--provider", "github", "--format", "json", "auth", "status"],
    );
    let glab_out = run_forge_cli(
        &glab,
        &[
            "--provider",
            "gitlab",
            "--host",
            "gitlab.com",
            "--format",
            "json",
            "auth",
            "status",
        ],
    );
    let gh_env = parse_envelope(&gh_out.stdout);
    let glab_env = parse_envelope(&glab_out.stdout);
    assert_eq!(gh_env["schema_version"], glab_env["schema_version"]);
    assert_eq!(gh_env["ok"], glab_env["ok"]);
}
