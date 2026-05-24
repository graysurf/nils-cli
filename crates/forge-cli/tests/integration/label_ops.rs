//! Integration tests for provider-neutral label catalog operations.

use std::fs;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

fn write_catalog() -> (TempDir, String) {
    let tempdir = TempDir::new().expect("tempdir");
    let path = tempdir.path().join("forge-labels.yaml");
    fs::write(
        &path,
        r#"schema: forge-label-catalog.v1
groups:
  - name: type
    prefix: "type::"
    exclusive: true
  - name: area
    prefix: "area::"
    exclusive: true
    allow_extensions: true
labels:
  - name: "type::bug"
    group: type
    color: d73a4a
    description: Bug report or fix.
    applies_to: [issue, pr, mr]
  - name: "area::cli"
    group: area
    color: 1d76db
    description: CLI surface.
    applies_to: [issue, pr, mr]
"#,
    )
    .expect("write catalog");
    (tempdir, path.to_string_lossy().into_owned())
}

#[test]
fn label_audit_github_reports_missing_and_drift() {
    let (_tempdir, catalog) = write_catalog();
    let stub = StubEnv::new().gh_stub(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "label list")
    cat <<'EOF'
[{"name":"type::bug","color":"ffffff","description":"old description"}]
EOF
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
    );

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "label",
            "audit",
            "--catalog",
            &catalog,
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.label.audit.v1");
    assert_eq!(env["data"]["provider"], "github");
    assert_eq!(env["data"]["status"], "fail");
    assert_eq!(env["data"]["missing"][0]["name"], "area::cli");
    assert_eq!(env["data"]["drift"][0]["name"], "type::bug");
    assert_eq!(env["data"]["drift"][0]["fields"][0]["field"], "color");
}

#[test]
fn label_audit_gitlab_accepts_json_label_list() {
    let (_tempdir, catalog) = write_catalog();
    let stub = StubEnv::new().glab_stub(
        r##"#!/bin/sh
set -e
case "$1 $2" in
  "label list")
    cat <<'EOF'
[{"id":11,"name":"type::bug","color":"#D73A4A","description":"Bug report or fix."},{"id":12,"name":"area::cli","color":"#1D76DB","description":"CLI surface."}]
EOF
    ;;
  *)
    echo "unexpected glab args: $*" >&2
    exit 99
    ;;
esac
"##,
    );

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "label",
            "audit",
            "--catalog",
            &catalog,
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.label.audit.v1");
    assert_eq!(env["data"]["provider"], "gitlab");
    assert_eq!(env["data"]["status"], "pass");
    assert_eq!(env["data"]["missing"].as_array().unwrap().len(), 0);
    assert_eq!(env["data"]["drift"].as_array().unwrap().len(), 0);
}

#[test]
fn label_ensure_dry_run_emits_create_and_update_plans() {
    let (_tempdir, catalog) = write_catalog();
    let stub = StubEnv::new().gh_stub(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "label list")
    cat <<'EOF'
[{"name":"type::bug","color":"ffffff","description":"old description"}]
EOF
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
    );

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--dry-run",
            "--format",
            "json",
            "label",
            "ensure",
            "--catalog",
            &catalog,
            "--update-existing",
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.label.ensure.v1");
    assert_eq!(env["data"]["dry_run"], true);
    let actions = env["data"]["actions"].as_array().expect("actions");
    assert_eq!(actions.len(), 2, "actions={actions:?}");
    assert_eq!(actions[0]["kind"], "create");
    assert_eq!(actions[0]["label"]["name"], "area::cli");
    assert_eq!(actions[1]["kind"], "update");
    assert_eq!(actions[1]["label"]["name"], "type::bug");
    let create_plan = actions[0]["plan"].as_array().unwrap();
    assert!(create_plan.iter().any(|v| v == "label"), "{create_plan:?}");
    assert!(create_plan.iter().any(|v| v == "create"), "{create_plan:?}");
    assert!(
        create_plan.iter().any(|v| v == "area::cli"),
        "{create_plan:?}"
    );
    assert!(create_plan.iter().any(|v| v == "--repo"), "{create_plan:?}");
}
