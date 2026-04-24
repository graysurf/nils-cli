use crate::support;
use std::path::{Path, PathBuf};

use api_testing_core::config::{
    resolve_gql_setup_dir_for_call, resolve_gql_setup_dir_for_history,
    resolve_rest_setup_dir_for_call, resolve_rest_setup_dir_for_history,
};
use nils_test_support::fixtures::write_text;
use pretty_assertions::assert_eq;
use support::RepoFixture;
use tempfile::TempDir;

fn canon(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).expect("canonicalize")
}

#[test]
fn rest_call_discovers_env_file_upwards() {
    let tmp = TempDir::new().expect("tmp");
    let root = canon(tmp.path());

    let config_dir = root.join("config/rest");
    std::fs::create_dir_all(config_dir.join("requests")).expect("mkdir");
    write_text(
        &config_dir.join("endpoints.env"),
        "REST_URL_LOCAL=http://example\n",
    );
    let request_path = write_text(
        &config_dir.join("requests/health.request.json"),
        r#"{"method":"GET","path":"/health"}"#,
    );

    let setup_dir =
        resolve_rest_setup_dir_for_call(&root, &root, &request_path, None).expect("resolve");

    assert_eq!(setup_dir, canon(&config_dir));
}

#[test]
fn rest_call_returns_seed_when_no_matches() {
    let tmp = TempDir::new().expect("tmp");
    let root = canon(tmp.path());
    let request_path = write_text(
        &root.join("requests/health.request.json"),
        r#"{"method":"GET","path":"/health"}"#,
    );

    let setup_dir =
        resolve_rest_setup_dir_for_call(&root, &root, &request_path, None).expect("resolve");

    assert_eq!(setup_dir, canon(request_path.parent().expect("parent")));
}

#[test]
fn rest_history_prefers_history_file_in_cwd() {
    let repo = RepoFixture::new();
    let cwd = canon(&repo.rest_setup);
    write_text(&repo.rest_setup.join(".rest_history"), "# entry\n\n");

    let setup_dir = resolve_rest_setup_dir_for_history(&cwd, None).expect("resolve");

    assert_eq!(setup_dir, cwd);
}

#[test]
fn gql_call_discovers_env_file_upwards() {
    let tmp = TempDir::new().expect("tmp");
    let root = canon(tmp.path());

    let config_dir = root.join("config/graphql");
    std::fs::create_dir_all(config_dir.join("operations")).expect("mkdir");
    write_text(&config_dir.join("jwts.env"), "GQL_JWT_SERVICE=token\n");
    let op_path = write_text(
        &config_dir.join("operations/health.graphql"),
        "query Health { __typename }\n",
    );

    let setup_dir =
        resolve_gql_setup_dir_for_call(&root, &root, Some(&op_path), None).expect("resolve");

    assert_eq!(setup_dir, canon(&config_dir));
}

#[test]
fn gql_call_explicit_config_dir_wins() {
    let repo = RepoFixture::new();
    let root = canon(&repo.root);
    let custom = repo.root.join("custom/graphql");
    std::fs::create_dir_all(&custom).expect("mkdir");
    let op_path = repo.write_operation("ops/health.graphql", "query Health { ok }\n");

    let setup_dir = resolve_gql_setup_dir_for_call(&root, &root, Some(&op_path), Some(&custom))
        .expect("resolve");

    assert_eq!(setup_dir, canon(&custom));
}

#[test]
fn gql_call_returns_seed_when_no_matches() {
    let tmp = TempDir::new().expect("tmp");
    let root = canon(tmp.path());
    let op_path = write_text(
        &root.join("ops/health.graphql"),
        "query Health { __typename }\n",
    );

    let setup_dir =
        resolve_gql_setup_dir_for_call(&root, &root, Some(&op_path), None).expect("resolve");

    assert_eq!(setup_dir, canon(op_path.parent().expect("parent")));
}

#[test]
fn gql_history_prefers_history_file_in_cwd() {
    let repo = RepoFixture::new();
    let cwd = canon(&repo.gql_setup);
    write_text(&repo.gql_setup.join(".gql_history"), "# entry\n\n");

    let setup_dir = resolve_gql_setup_dir_for_history(&cwd, &cwd, None).expect("resolve");

    assert_eq!(setup_dir, cwd);
}
