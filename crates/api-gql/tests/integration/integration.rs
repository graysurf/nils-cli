use std::path::Path;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_with};
use nils_test_support::fs::write_text;
use nils_test_support::http::{HttpResponse, RecordedRequest, TestServer};

const LOGIN_TOKEN_JWT: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.e30.sig";

fn api_gql_bin() -> std::path::PathBuf {
    resolve("api-gql")
}

fn base_cmd_options(cwd: &Path) -> CmdOptions {
    let mut options = CmdOptions::default().with_cwd(cwd);
    for key in [
        "ACCESS_TOKEN",
        "SERVICE_TOKEN",
        "GQL_ALLOW_EMPTY_ENABLED",
        "GQL_ENV_DEFAULT",
        "GQL_HISTORY_ENABLED",
        "GQL_HISTORY_FILE",
        "GQL_HISTORY_LOG_URL_ENABLED",
        "GQL_JWT_NAME",
        "GQL_JWT_VALIDATE_ENABLED",
        "GQL_JWT_VALIDATE_LEEWAY_SECONDS",
        "GQL_JWT_VALIDATE_STRICT",
        "GQL_REPORT_COMMAND_LOG_URL_ENABLED",
        "GQL_REPORT_DIR",
        "GQL_REPORT_INCLUDE_COMMAND_ENABLED",
        "GQL_SCHEMA_FILE",
        "GQL_URL",
        "GQL_VARS_MIN_LIMIT",
    ] {
        options = options.with_env_remove(key);
    }
    options
}

fn run_api_gql(cwd: &Path, args: &[&str]) -> CmdOutput {
    let options = base_cmd_options(cwd);
    run_with(&api_gql_bin(), args, &options)
}

fn run_api_gql_with(cwd: &Path, args: &[&str], options: CmdOptions) -> CmdOutput {
    let options = options.with_cwd(cwd);
    run_with(&api_gql_bin(), args, &options)
}

fn body_has_login_query(body: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let Some(q) = v.get("query").and_then(|q| q.as_str()) else {
        return false;
    };
    q.contains("login")
}

fn start_server() -> TestServer {
    TestServer::new(
        |req: &RecordedRequest| match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/graphql") => {
                if body_has_login_query(&req.body) {
                    let body =
                        format!(r#"{{"data":{{"login":{{"accessToken":"{LOGIN_TOKEN_JWT}"}}}}}}"#);
                    HttpResponse::new(200, body).with_header("Content-Type", "application/json")
                } else {
                    HttpResponse::new(200, r#"{"data":{"ok":true}}"#)
                        .with_header("Content-Type", "application/json")
                }
            }
            ("POST", "/graphql500") => HttpResponse::new(500, r#"{"error":"no"}"#)
                .with_header("Content-Type", "application/json"),
            _ => HttpResponse::new(404, "not found"),
        },
    )
    .expect("start test server")
}

#[test]
fn call_vars_min_limit_bumps_nested_limit_fields() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();

    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let server = start_server();
    write_text(
        &setup_dir.join("endpoints.env"),
        &format!("GQL_URL_LOCAL={}/graphql\n", server.url()),
    );

    let op = root.join("ops/q.graphql");
    write_text(&op, "query Q($limit: Int) { ok }\n");

    let vars = root.join("ops/q.variables.json");
    write_text(
        &vars,
        r#"{"limit":1,"nested":{"limit":2},"arr":[{"limit":3},{"limit":10}]}"#,
    );

    let out = run_api_gql(
        root,
        &[
            "call",
            "--config-dir",
            "setup/graphql",
            "--env",
            "local",
            "ops/q.graphql",
            "ops/q.variables.json",
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let stdout_json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout json");
    assert_eq!(stdout_json["data"]["ok"], true);

    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].path, "/graphql");
    let sent: serde_json::Value = serde_json::from_slice(&reqs[0].body).expect("sent json");
    assert_eq!(sent["variables"]["limit"], 5);
    assert_eq!(sent["variables"]["nested"]["limit"], 5);
    assert_eq!(sent["variables"]["arr"][0]["limit"], 5);
    assert_eq!(sent["variables"]["arr"][1]["limit"], 10);
}

#[test]
fn call_non_2xx_exits_1() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/graphql")).expect("mkdir setup");
    let server = start_server();

    let op = root.join("q.graphql");
    write_text(&op, "query Q { ok }\n");

    let out = run_api_gql(
        root,
        &[
            "call",
            "--config-dir",
            "setup/graphql",
            "--url",
            &format!("{}/graphql500", server.url()),
            "q.graphql",
        ],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr_text()
            .contains("HTTP request failed with status")
    );
}

#[test]
fn call_jwt_profile_auto_login_injects_bearer_token() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();

    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let server = start_server();
    write_text(
        &setup_dir.join("endpoints.env"),
        &format!("GQL_URL_LOCAL={}/graphql\n", server.url()),
    );
    write_text(&setup_dir.join("jwts.env"), "GQL_JWT_ADMIN=\n");

    // Selected profile is missing => auto-login uses login.<profile>.graphql
    write_text(
        &setup_dir.join("login.admin.graphql"),
        "query Login { login { accessToken } }\n",
    );

    let op = root.join("q.graphql");
    write_text(&op, "query Q { ok }\n");

    let out = run_api_gql(
        root,
        &[
            "call",
            "--config-dir",
            "setup/graphql",
            "--env",
            "local",
            "--jwt",
            "admin",
            "q.graphql",
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 2);

    assert!(
        reqs[0].header_value("authorization").is_none(),
        "login should not include Authorization"
    );

    let auth = reqs[1].header_value("authorization").unwrap_or_default();
    assert_eq!(auth, format!("Bearer {LOGIN_TOKEN_JWT}"));
}

#[test]
fn report_blocks_empty_by_default() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();

    let op = root.join("q.graphql");
    write_text(&op, "query Q { ok }\n");

    let resp = root.join("resp.json");
    write_text(&resp, r#"{"data":{"pageInfo":{"hasNextPage":false}}}"#);

    let out = run_api_gql(
        root,
        &[
            "report",
            "--case",
            "empty",
            "--op",
            "q.graphql",
            "--response",
            "resp.json",
        ],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().contains("no data records"));
}

#[test]
fn report_allow_empty_writes_report() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();

    let op = root.join("q.graphql");
    write_text(&op, "query Q { ok }\n");

    let resp = root.join("resp.json");
    write_text(&resp, r#"{"data":{"pageInfo":{"hasNextPage":false}}}"#);

    let out = run_api_gql(
        root,
        &[
            "report",
            "--case",
            "empty",
            "--op",
            "q.graphql",
            "--response",
            "resp.json",
            "--allow-empty",
        ],
    );
    assert_eq!(out.code, 0);
    let report_path = out.stdout_text().trim().to_string();
    assert!(!report_path.is_empty());
    assert!(Path::new(&report_path).is_file());
    let contents = std::fs::read_to_string(&report_path).expect("read report");
    assert!(contents.contains("# API Test Report"));
}

#[test]
fn report_run_executes_and_writes_report() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();

    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let server = start_server();
    write_text(
        &setup_dir.join("endpoints.env"),
        &format!("GQL_URL_LOCAL={}/graphql\n", server.url()),
    );

    let op = root.join("q.graphql");
    write_text(&op, "query Q { ok }\n");

    let out = run_api_gql(
        root,
        &[
            "report",
            "--case",
            "run",
            "--op",
            "q.graphql",
            "--run",
            "--env",
            "local",
            "--config-dir",
            "setup/graphql",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let report_path = out.stdout_text().trim().to_string();
    assert!(!report_path.is_empty());
    let contents = std::fs::read_to_string(&report_path).expect("read report");
    assert!(contents.contains("Result: PASS"));
    assert!(contents.contains("\"ok\": true"));
}

#[test]
fn report_response_stdin_writes_report() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();

    let op = root.join("q.graphql");
    write_text(&op, "query Q { ok }\n");

    let response_body = r#"{"data":{"items":[{"id":1}]}}"#;
    let out = run_api_gql_with(
        root,
        &[
            "report",
            "--case",
            "stdin",
            "--op",
            "q.graphql",
            "--response",
            "-",
        ],
        base_cmd_options(root).with_stdin_str(response_body),
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let report_path = out.stdout_text().trim().to_string();
    let contents = std::fs::read_to_string(&report_path).expect("read report");
    assert!(contents.contains("\"id\": 1"));
}

#[test]
fn report_rejects_non_json_response_without_allow_empty() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();

    let op = root.join("q.graphql");
    write_text(&op, "query Q { ok }\n");

    let resp = root.join("resp.txt");
    write_text(&resp, "not-json");

    let out = run_api_gql(
        root,
        &[
            "report",
            "--case",
            "bad-json",
            "--op",
            "q.graphql",
            "--response",
            "resp.txt",
        ],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr_text()
            .contains("Response is not JSON; refusing to write a no-data report.")
    );
}

#[test]
fn report_redaction_defaults_and_no_redact() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();

    let op = root.join("q.graphql");
    write_text(&op, "query Q { ok }\n");

    let vars = root.join("vars.json");
    write_text(&vars, r#"{"password":"vars-secret"}"#);

    let resp = root.join("resp.json");
    write_text(&resp, r#"{"data":{"token":"resp-secret","ok":true}}"#);

    let out = run_api_gql(
        root,
        &[
            "report",
            "--case",
            "redact",
            "--op",
            "q.graphql",
            "--vars",
            "vars.json",
            "--response",
            "resp.json",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let report_path = out.stdout_text().trim().to_string();
    let contents = std::fs::read_to_string(&report_path).expect("read report");
    assert!(contents.contains("<REDACTED>"));
    assert!(!contents.contains("vars-secret"));
    assert!(!contents.contains("resp-secret"));

    let out = run_api_gql(
        root,
        &[
            "report",
            "--case",
            "no-redact",
            "--op",
            "q.graphql",
            "--vars",
            "vars.json",
            "--response",
            "resp.json",
            "--no-redact",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let report_path = out.stdout_text().trim().to_string();
    let contents = std::fs::read_to_string(&report_path).expect("read report");
    assert!(contents.contains("vars-secret"));
    assert!(contents.contains("resp-secret"));
}

#[test]
fn report_no_command_omits_command_section() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();

    let op = root.join("q.graphql");
    write_text(&op, "query Q { ok }\n");

    let resp = root.join("resp.json");
    write_text(&resp, r#"{"data":{"items":[{"id":1}]}}"#);

    let out = run_api_gql(
        root,
        &[
            "report",
            "--case",
            "no-command",
            "--op",
            "q.graphql",
            "--response",
            "resp.json",
            "--no-command",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let report_path = out.stdout_text().trim().to_string();
    let contents = std::fs::read_to_string(&report_path).expect("read report");
    assert!(!contents.contains("## Command"));
}

#[test]
fn report_no_command_url_omits_url_value() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();

    let op = root.join("q.graphql");
    write_text(&op, "query Q { ok }\n");

    let resp = root.join("resp.json");
    write_text(&resp, r#"{"data":{"items":[{"id":1}]}}"#);

    let out = run_api_gql(
        root,
        &[
            "report",
            "--case",
            "no-command-url",
            "--op",
            "q.graphql",
            "--response",
            "resp.json",
            "--url",
            "http://example.invalid/graphql",
            "--no-command-url",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let report_path = out.stdout_text().trim().to_string();
    let contents = std::fs::read_to_string(&report_path).expect("read report");
    assert!(contents.contains("<omitted>"));
}
