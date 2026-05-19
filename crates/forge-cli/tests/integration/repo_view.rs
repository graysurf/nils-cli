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
fn repo_view_gitlab_normalizes_envelope() {
    let stub = StubEnv::new().glab_stub(&stdout_stub(GLAB_REPO_VIEW_JSON));
    let out = run_forge_cli(
        &stub,
        &["--provider", "gitlab", "--format", "json", "repo", "view"],
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
        &["--provider", "gitlab", "--format", "json", "repo", "view"],
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
