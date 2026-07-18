//! End-to-end `repo view` integration covering both backends.

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

const GH_REPO_VIEW_JSON: &str = r#"{
  "name": "nils-cli",
  "owner": { "login": "sympoies" },
  "url": "https://github.com/sympoies/nils-cli",
  "defaultBranchRef": { "name": "main" },
  "mergeCommitAllowed": false,
  "squashMergeAllowed": true,
  "rebaseMergeAllowed": false
}"#;

const GLAB_REPO_VIEW_JSON: &str = r#"{
  "path": "nils-cli",
  "namespace": { "full_path": "sympoies" },
  "web_url": "https://gitlab.com/sympoies/nils-cli",
  "default_branch": "main",
  "merge_method": "merge",
  "squash_option": "default_on"
}"#;

fn stdout_stub(body: &str) -> String {
    // Single-line "$@" passthrough silenced; we only need stdout to carry the
    // canned JSON.
    format!("#!/bin/sh\ncat <<'EOF'\n{body}\nEOF\n")
}

fn argv_bound_stub(expected: &str, body: &str) -> String {
    format!(
        "#!/bin/sh\n\
         [ \"$*\" = \"{expected}\" ] || {{ echo \"unexpected argv: $*\" >&2; exit 97; }}\n\
         cat <<'EOF'\n{body}\nEOF\n"
    )
}

#[test]
fn repo_view_github_normalizes_envelope() {
    let stub = StubEnv::new().gh_stub(&stdout_stub(GH_REPO_VIEW_JSON));
    let out = run_forge_cli(
        &stub,
        &["--provider", "github", "--format", "json", "repo", "view"],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.repo.view.v1");
    assert_eq!(envelope["data"]["owner"], "sympoies");
    assert_eq!(envelope["data"]["name"], "nils-cli");
    assert_eq!(envelope["data"]["default_branch"], "main");
    let methods = envelope["data"]["merge_methods_allowed"]
        .as_array()
        .expect("methods array");
    assert_eq!(methods, &vec![serde_json::Value::String("squash".into())]);
}

#[test]
fn repo_view_github_enterprise_binds_host_in_positional_locator() {
    let expected = "repo view internal.ghe.com/sympoies/nils-cli --json name,owner,defaultBranchRef,mergeCommitAllowed,squashMergeAllowed,rebaseMergeAllowed,url";
    let stub = StubEnv::new().gh_stub(&argv_bound_stub(expected, GH_REPO_VIEW_JSON));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--host",
            "internal.ghe.com",
            "--repo",
            "sympoies/nils-cli",
            "--format",
            "json",
            "repo",
            "view",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
}

#[test]
fn repo_view_github_custom_authority_retains_port_and_binds_environment() {
    let expected = "repo view internal.example:8443/sympoies/nils-cli --json name,owner,defaultBranchRef,mergeCommitAllowed,squashMergeAllowed,rebaseMergeAllowed,url";
    let script = format!(
        "#!/bin/sh\n\
         [ \"$GH_HOST\" = \"internal.example:8443\" ] || {{ echo \"unexpected GH_HOST: $GH_HOST\" >&2; exit 96; }}\n\
         [ \"$*\" = \"{expected}\" ] || {{ echo \"unexpected argv: $*\" >&2; exit 97; }}\n\
         cat <<'EOF'\n{GH_REPO_VIEW_JSON}\nEOF\n"
    );
    let stub = StubEnv::new().gh_stub(&script);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--host",
            "internal.example:8443",
            "--repo",
            "sympoies/nils-cli",
            "--format",
            "json",
            "repo",
            "view",
        ],
    );
    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
}

#[test]
fn repo_view_rejects_untrusted_or_injected_host_before_backend() {
    for host in [
        "gitlab.com@attacker.example",
        "gitlab.attacker.example",
        "https://github.com",
        "github.com/path",
    ] {
        let stub = StubEnv::new().gh_stub("#!/bin/sh\necho backend-called >&2\nexit 97\n");
        let out = run_forge_cli(
            &stub,
            &[
                "--host",
                host,
                "--repo",
                "sympoies/nils-cli",
                "--format",
                "json",
                "repo",
                "view",
            ],
        );
        assert_eq!(
            out.code, 64,
            "host={host} stdout={} stderr={}",
            out.stdout, out.stderr
        );
        let envelope = parse_envelope(&out.stdout);
        assert_eq!(envelope["error"]["code"], "provider_unsupported");
        assert!(!out.stderr.contains("backend-called"));
    }
}

#[test]
fn repo_view_rejects_nested_github_repo_before_backend() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\nexit 97\n");
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "owner/subgroup/repo",
            "--format",
            "json",
            "repo",
            "view",
        ],
    );
    assert_eq!(out.code, 65, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "repo_invalid");
}

#[test]
fn repo_view_rejects_malformed_repository_without_disclosing_credentials() {
    const REPOSITORY: &str = "https://alice:credential-value@github.com/owner/repo/extra.git";

    for format in ["json", "text"] {
        let stub = StubEnv::new().gh_stub("#!/bin/sh\necho backend-called >&2\nexit 97\n");
        let out = run_forge_cli(
            &stub,
            &[
                "--provider",
                "github",
                "--repo",
                REPOSITORY,
                "--format",
                format,
                "repo",
                "view",
            ],
        );

        assert_eq!(out.code, 65, "format={format} stderr={}", out.stderr);
        let combined = format!("{}{}", out.stdout, out.stderr);
        assert!(
            combined.contains("repo_invalid"),
            "format={format}: {combined}"
        );
        assert!(!combined.contains("alice"), "format={format}: {combined}");
        assert!(
            !combined.contains("credential-value"),
            "format={format}: {combined}"
        );
        assert!(
            !combined.contains(REPOSITORY),
            "format={format}: {combined}"
        );
        assert!(
            !combined.contains("backend-called"),
            "format={format}: {combined}"
        );
    }
}

#[test]
fn repo_view_gitlab_normalizes_envelope() {
    let stub = StubEnv::new().glab_stub(&stdout_stub(GLAB_REPO_VIEW_JSON));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--host",
            "gitlab.com",
            "--repo",
            "sympoies/nils-cli",
            "--format",
            "json",
            "repo",
            "view",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.repo.view.v1");
    assert_eq!(envelope["data"]["owner"], "sympoies");
    assert_eq!(envelope["data"]["name"], "nils-cli");
    assert_eq!(envelope["data"]["default_branch"], "main");
}

#[test]
fn repo_view_parity_envelope_modulo_provider_and_url_host() {
    let gh = StubEnv::new().gh_stub(&stdout_stub(GH_REPO_VIEW_JSON));
    let glab = StubEnv::new().glab_stub(&stdout_stub(GLAB_REPO_VIEW_JSON));
    let gh_out = run_forge_cli(
        &gh,
        &["--provider", "github", "--format", "json", "repo", "view"],
    );
    let glab_out = run_forge_cli(
        &glab,
        &[
            "--provider",
            "gitlab",
            "--host",
            "gitlab.com",
            "--repo",
            "sympoies/nils-cli",
            "--format",
            "json",
            "repo",
            "view",
        ],
    );
    let gh_env = parse_envelope(&gh_out.stdout);
    let glab_env = parse_envelope(&glab_out.stdout);
    // schema_version, ok, owner, name, default_branch identical.
    assert_eq!(gh_env["schema_version"], glab_env["schema_version"]);
    assert_eq!(gh_env["ok"], glab_env["ok"]);
    assert_eq!(gh_env["data"]["owner"], glab_env["data"]["owner"]);
    assert_eq!(gh_env["data"]["name"], glab_env["data"]["name"]);
    assert_eq!(
        gh_env["data"]["default_branch"],
        glab_env["data"]["default_branch"]
    );
    // data.provider and url host differ (parity admits this).
    assert_ne!(gh_env["data"]["provider"], glab_env["data"]["provider"]);
    assert_ne!(gh_env["data"]["url"], glab_env["data"]["url"]);
}
