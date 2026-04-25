use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_with};
use nils_test_support::fs::write_text;

fn api_gql_bin() -> PathBuf {
    resolve("api-gql")
}

fn run_api_gql(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default()
        .with_cwd(cwd)
        .with_env_remove("GQL_SCHEMA_FILE");
    for (k, v) in envs {
        options = options.with_env(k, v);
    }
    run_with(&api_gql_bin(), args, &options)
}

#[test]
fn schema_prints_resolved_path_when_fallback_file_exists() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    write_text(
        &setup_dir.join("schema.graphql"),
        "type Query { ok: Boolean }\n",
    );

    let out = run_api_gql(root, &["schema", "--config-dir", "setup/graphql"], &[]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let printed_raw = out.stdout_text();
    let printed = printed_raw.trim();
    assert!(!printed.is_empty());
    let p = PathBuf::from(printed);
    assert!(
        p.ends_with("schema.graphql"),
        "stdout={}",
        out.stdout_text()
    );
}

#[test]
fn schema_cat_prints_file_contents() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    write_text(
        &setup_dir.join("schema.graphql"),
        "type Query { ok: Boolean }\n",
    );

    let out = run_api_gql(
        root,
        &["schema", "--config-dir", "setup/graphql", "--cat"],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out.stdout_text().contains("type Query"));
}

#[test]
fn schema_errors_when_not_configured() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/graphql")).expect("mkdir setup");

    let out = run_api_gql(root, &["schema", "--config-dir", "setup/graphql"], &[]);
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().contains("Schema file not configured"));
    assert!(out.stderr_text().contains("schema.env"));
}

#[test]
fn schema_errors_when_schema_env_points_to_missing_file() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    write_text(
        &setup_dir.join("schema.env"),
        "GQL_SCHEMA_FILE=missing.graphql\n",
    );

    let out = run_api_gql(root, &["schema", "--config-dir", "setup/graphql"], &[]);
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().contains("Schema file not found:"));
    assert!(out.stderr_text().contains("missing.graphql"));
}

#[test]
fn schema_local_env_overrides_schema_env() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    write_text(
        &setup_dir.join("schema.env"),
        "GQL_SCHEMA_FILE=from-env.graphql\n",
    );
    write_text(
        &setup_dir.join("schema.local.env"),
        "GQL_SCHEMA_FILE=from-local.graphql\n",
    );
    write_text(
        &setup_dir.join("from-env.graphql"),
        "type Query { env: Boolean }\n",
    );
    write_text(
        &setup_dir.join("from-local.graphql"),
        "type Query { local: Boolean }\n",
    );

    let out = run_api_gql(root, &["schema", "--config-dir", "setup/graphql"], &[]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let printed_raw = out.stdout_text();
    let printed = printed_raw.trim();
    let p = PathBuf::from(printed);
    assert!(
        p.ends_with("from-local.graphql"),
        "stdout={}",
        out.stdout_text()
    );
}

#[test]
fn schema_file_flag_overrides_env_and_schema_env() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    write_text(
        &setup_dir.join("schema.env"),
        "GQL_SCHEMA_FILE=missing.graphql\n",
    );
    write_text(
        &setup_dir.join("real.graphql"),
        "type Query { ok: Boolean }\n",
    );

    let out = run_api_gql(
        root,
        &[
            "schema",
            "--config-dir",
            "setup/graphql",
            "--file",
            "real.graphql",
        ],
        &[("GQL_SCHEMA_FILE", "also-missing.graphql")],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let printed_raw = out.stdout_text();
    let printed = printed_raw.trim();
    let p = PathBuf::from(printed);
    assert!(p.ends_with("real.graphql"), "stdout={}", out.stdout_text());
}
