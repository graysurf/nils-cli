use std::path::Path;

use api_testing_core::cli_endpoint::{
    EndpointConfig, list_available_env_suffixes, resolve_cli_endpoint,
};
use nils_test_support::{EnvGuard, GlobalStateLock};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn write_file(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, contents).expect("write");
}

fn endpoints_files<'a>(endpoints_env: &'a Path, endpoints_local: &'a Path) -> Vec<&'a Path> {
    if endpoints_env.is_file() {
        vec![endpoints_env, endpoints_local]
    } else {
        Vec::new()
    }
}

fn rest_config<'a>(
    explicit_url: Option<&'a str>,
    env_name: Option<&'a str>,
    endpoints_env: &'a Path,
    endpoints_local: &'a Path,
    endpoints_files: &'a [&'a Path],
) -> EndpointConfig<'a> {
    EndpointConfig {
        explicit_url,
        env_name,
        endpoints_env,
        endpoints_local,
        endpoints_files,
        url_env_var: "REST_URL",
        env_default_var: "REST_ENV_DEFAULT",
        url_prefix: "REST_URL_",
        default_url: "http://default",
        setup_dir_label: "setup/rest/",
    }
}

#[test]
fn resolve_cli_endpoint_prefers_explicit_url() {
    let tmp = TempDir::new().unwrap();
    let endpoints_env = tmp.path().join("endpoints.env");
    let endpoints_local = tmp.path().join("endpoints.local.env");
    write_file(
        &endpoints_env,
        "REST_ENV_DEFAULT=prod\nREST_URL_PROD=http://prod\n",
    );
    let endpoints_files = endpoints_files(&endpoints_env, &endpoints_local);

    let selection = resolve_cli_endpoint(rest_config(
        Some("http://explicit"),
        Some("prod"),
        &endpoints_env,
        &endpoints_local,
        &endpoints_files,
    ))
    .unwrap();

    assert_eq!(selection.url, "http://explicit");
    assert_eq!(selection.endpoint_label_used, "url");
    assert_eq!(selection.endpoint_value_used, "http://explicit");
}

#[test]
fn resolve_cli_endpoint_env_as_url_ignores_missing_endpoints() {
    let tmp = TempDir::new().unwrap();
    let endpoints_env = tmp.path().join("missing.env");
    let endpoints_local = tmp.path().join("missing.local.env");
    let endpoints_files = endpoints_files(&endpoints_env, &endpoints_local);

    let selection = resolve_cli_endpoint(rest_config(
        None,
        Some("https://example.com"),
        &endpoints_env,
        &endpoints_local,
        &endpoints_files,
    ))
    .unwrap();

    assert_eq!(selection.url, "https://example.com");
    assert_eq!(selection.endpoint_label_used, "url");
    assert_eq!(selection.endpoint_value_used, "https://example.com");
}

#[test]
fn resolve_cli_endpoint_unknown_env_lists_available() {
    let tmp = TempDir::new().unwrap();
    let endpoints_env = tmp.path().join("endpoints.env");
    let endpoints_local = tmp.path().join("endpoints.local.env");
    write_file(
        &endpoints_env,
        "REST_URL_PROD=http://prod\nREST_URL_DEV=http://dev\n",
    );
    write_file(&endpoints_local, "REST_URL_TEST=http://test\n");
    let endpoints_files = endpoints_files(&endpoints_env, &endpoints_local);

    let err = resolve_cli_endpoint(rest_config(
        None,
        Some("stage"),
        &endpoints_env,
        &endpoints_local,
        &endpoints_files,
    ))
    .unwrap_err();

    assert_eq!(
        err.to_string(),
        "Unknown --env 'stage' (available: dev prod test)"
    );
}

#[test]
fn resolve_cli_endpoint_env_default_from_env_file() {
    let lock = GlobalStateLock::new();
    let tmp = TempDir::new().unwrap();
    let endpoints_env = tmp.path().join("endpoints.env");
    let endpoints_local = tmp.path().join("endpoints.local.env");
    let _url_guard = EnvGuard::remove(&lock, "REST_URL");
    write_file(
        &endpoints_env,
        "REST_ENV_DEFAULT=prod\nREST_URL_PROD=http://prod\n",
    );
    let endpoints_files = endpoints_files(&endpoints_env, &endpoints_local);

    let selection = resolve_cli_endpoint(rest_config(
        None,
        None,
        &endpoints_env,
        &endpoints_local,
        &endpoints_files,
    ))
    .unwrap();

    assert_eq!(selection.url, "http://prod");
    assert_eq!(selection.endpoint_label_used, "env");
    assert_eq!(selection.endpoint_value_used, "prod");
}

#[test]
fn resolve_cli_endpoint_uses_url_env_var() {
    let lock = GlobalStateLock::new();
    let tmp = TempDir::new().unwrap();
    let endpoints_env = tmp.path().join("missing.env");
    let endpoints_local = tmp.path().join("missing.local.env");
    let endpoints_files = endpoints_files(&endpoints_env, &endpoints_local);
    let _guard = EnvGuard::set(&lock, "REST_URL", "http://env");

    let selection = resolve_cli_endpoint(rest_config(
        None,
        None,
        &endpoints_env,
        &endpoints_local,
        &endpoints_files,
    ))
    .unwrap();

    assert_eq!(selection.url, "http://env");
    assert_eq!(selection.endpoint_label_used, "url");
    assert_eq!(selection.endpoint_value_used, "http://env");
}

#[test]
fn list_available_env_suffixes_errors_when_missing_endpoints() {
    let tmp = TempDir::new().unwrap();
    let endpoints_env = tmp.path().join("missing.env");
    let endpoints_local = tmp.path().join("missing.local.env");

    let err = list_available_env_suffixes(
        &endpoints_env,
        &endpoints_local,
        "REST_URL_",
        "endpoints.env not found (expected under setup/rest/)",
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "endpoints.env not found (expected under setup/rest/)"
    );
}

#[test]
fn list_available_env_suffixes_local_only() {
    let tmp = TempDir::new().unwrap();
    let endpoints_env = tmp.path().join("missing.env");
    let endpoints_local = tmp.path().join("endpoints.local.env");
    write_file(
        &endpoints_local,
        "REST_URL_LOCAL=http://local\nREST_URL_DEV=http://dev\n",
    );

    let out = list_available_env_suffixes(
        &endpoints_env,
        &endpoints_local,
        "REST_URL_",
        "endpoints.env not found (expected under setup/rest/)",
    )
    .unwrap();

    assert_eq!(out, vec!["dev", "local"]);
}

#[test]
fn list_available_env_suffixes_merges_and_sorts() {
    let tmp = TempDir::new().unwrap();
    let endpoints_env = tmp.path().join("endpoints.env");
    let endpoints_local = tmp.path().join("endpoints.local.env");
    write_file(
        &endpoints_env,
        "REST_URL_PROD=http://prod\nREST_URL_DEV=http://dev\n",
    );
    write_file(
        &endpoints_local,
        "REST_URL_TEST=http://test\nREST_URL_DEV=http://dev2\n",
    );

    let out = list_available_env_suffixes(
        &endpoints_env,
        &endpoints_local,
        "REST_URL_",
        "endpoints.env not found (expected under setup/rest/)",
    )
    .unwrap();

    assert_eq!(out, vec!["dev", "prod", "test"]);
}
