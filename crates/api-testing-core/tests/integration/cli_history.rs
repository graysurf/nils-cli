use api_testing_core::{
    auth_env::CliAuthSource,
    cli_history::{
        RequestCallHistoryAppend, RequestCallHistoryFlag, append_request_call_history_best_effort,
        resolve_history_file, run_history_command, select_history_records,
    },
    history::{HistoryWriter, RotationPolicy},
};
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
fn cli_history_selects_tail_and_strips_metadata_for_command_only() {
    let records = vec![
        "# one\napi-rest call \\\n  one.request.json \\\n| jq .\n\n".to_string(),
        "# two\napi-rest call \\\n  two.request.json \\\n| jq .\n\n".to_string(),
    ];

    let selected = select_history_records(&records, Some(1), true);

    assert_eq!(selected.len(), 1);
    assert!(selected[0].contains("two.request.json"));
    assert!(!selected[0].contains("# two"));
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

#[test]
fn cli_history_appends_shared_request_call_record() {
    let tmp = TempDir::new().unwrap();
    let setup_dir = tmp.path().join("setup/websocket");
    std::fs::create_dir_all(&setup_dir).unwrap();
    let history_file = tmp.path().join(".ws_history");
    let history_writer = HistoryWriter::new(
        history_file.clone(),
        RotationPolicy {
            max_mb: 10,
            keep: 5,
        },
    );
    let format_flag = [RequestCallHistoryFlag::raw("format", "json")];
    let auth_source = CliAuthSource::TokenProfile;

    append_request_call_history_best_effort(
        RequestCallHistoryAppend {
            enabled: true,
            history_writer: &history_writer,
            exit_code: 0,
            setup_dir: &setup_dir,
            invocation_dir: tmp.path(),
            command_name: "api-websocket",
            endpoint_label_used: "url",
            endpoint_value_used: "ws://127.0.0.1:9001/ws",
            log_url: false,
            auth_source: &auth_source,
            token_name_for_log: "svc",
            request_arg: "/abs/requests/health.ws.json",
            extra_flags: &format_flag,
            warning_label: "api-websocket",
        },
        &mut std::io::sink(),
    );

    let text = std::fs::read_to_string(&history_file).unwrap();
    assert!(text.contains("api-websocket call \\"));
    assert!(text.contains("url=<omitted>"));
    assert!(text.contains("token=svc"));
    assert!(text.contains("--token 'svc'"));
    assert!(text.contains("--format json"));
    assert!(!text.contains("--url "));
}
