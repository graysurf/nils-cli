use api_testing_core::cli_history::{resolve_history_file, run_history_command};
use nils_test_support::{EnvGuard, GlobalStateLock};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[test]
fn cli_history_command_only_and_empty_records() {
    let tmp = TempDir::new().unwrap();
    let history_file = tmp.path().join(".rest_history");

    std::fs::write(
        &history_file,
        "# stamp exit=0 setup_dir=.\napi-rest call \\\n  --config-dir 'setup/rest' \\\n  requests/health.request.json \\\n| jq .\n\n",
    )
    .unwrap();

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_history_command(&history_file, Some(1), true, &mut stdout, &mut stderr);
    assert_eq!(code, 0);
    let out = String::from_utf8_lossy(&stdout);
    assert!(out.contains("api-rest call"));

    std::fs::write(&history_file, "").unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_history_command(&history_file, Some(1), true, &mut stdout, &mut stderr);
    assert_eq!(code, 3);
}

#[test]
fn cli_history_missing_file_returns_error() {
    let tmp = TempDir::new().unwrap();
    let history_file = tmp.path().join(".rest_history");

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_history_command(&history_file, None, false, &mut stdout, &mut stderr);
    assert_eq!(code, 1);
    let err = String::from_utf8_lossy(&stderr);
    assert!(err.contains("History file not found"));
}

#[test]
fn cli_history_resolves_env_override_under_setup_dir() {
    let lock = GlobalStateLock::new();
    let tmp = TempDir::new().unwrap();
    let setup_dir = tmp.path().join("setup/rest");
    std::fs::create_dir_all(&setup_dir).unwrap();
    std::fs::write(setup_dir.join("endpoints.env"), "REST_URL_DEV=http://dev\n").unwrap();

    let _guard = EnvGuard::set(&lock, "REST_HISTORY_FILE", "custom.history");
    let history_file = resolve_history_file(
        tmp.path(),
        None,
        None,
        "REST_HISTORY_FILE",
        |cwd, config_dir| {
            api_testing_core::config::resolve_rest_setup_dir_for_history(cwd, config_dir)
        },
        ".rest_history",
    )
    .unwrap();

    let setup_dir_abs = std::fs::canonicalize(&setup_dir).unwrap();
    assert_eq!(history_file, setup_dir_abs.join("custom.history"));
}
