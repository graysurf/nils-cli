use std::path::Path;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_with};
use nils_test_support::fs::write_text;
use nils_test_support::http::{HttpResponse, RecordedRequest, TestServer};

fn api_gql_bin() -> std::path::PathBuf {
    resolve("api-gql")
}

fn run_api_gql(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default().with_cwd(cwd);
    for key in [
        "GQL_URL",
        "GQL_ENV_DEFAULT",
        "GQL_JWT_NAME",
        "ACCESS_TOKEN",
        "SERVICE_TOKEN",
        "GQL_SCHEMA_FILE",
    ] {
        options = options.with_env_remove(key);
    }
    options = options.with_env("GQL_JWT_VALIDATE_ENABLED", "false");

    for (k, v) in envs {
        options = options.with_env(k, v);
    }

    run_with(&api_gql_bin(), args, &options)
}

fn start_server() -> TestServer {
    TestServer::new(|_req: &RecordedRequest| {
        HttpResponse::new(200, r#"{"data":{"ok":true}}"#)
            .with_header("Content-Type", "application/json")
    })
    .expect("start test server")
}

#[test]
fn list_envs_outputs_sorted_deduped_suffixes_from_endpoints_env_and_local() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    write_text(
        &setup_dir.join("endpoints.env"),
        r#"
# comment
export GQL_URL_PROD=http://example.invalid/graphql
GQL_URL_STAGING=http://example.invalid/graphql
"#,
    );
    write_text(
        &setup_dir.join("endpoints.local.env"),
        r#"
GQL_URL_LOCAL=http://example.invalid/graphql
GQL_URL_PROD=http://example.invalid/graphql
"#,
    );

    let out = run_api_gql(
        root,
        &["call", "--config-dir", "setup/graphql", "--list-envs"],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let lines: Vec<String> = out
        .stdout_text()
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();
    assert_eq!(lines, vec!["local", "prod", "staging"]);
}

#[test]
fn list_jwts_outputs_sorted_deduped_suffixes_and_skips_name() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    write_text(
        &setup_dir.join("jwts.env"),
        r#"
# comment
GQL_JWT_ADMIN=token-a
GQL_JWT_NAME=admin
GQL_JWT_TEAM=token-team
"#,
    );
    write_text(
        &setup_dir.join("jwts.local.env"),
        r#"
GQL_JWT_ADMIN=token-local
GQL_JWT_SERVICE=token-service
"#,
    );

    let out = run_api_gql(
        root,
        &["call", "--config-dir", "setup/graphql", "--list-jwts"],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let lines: Vec<String> = out
        .stdout_text()
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();
    assert_eq!(lines, vec!["admin", "service", "team"]);
}

#[test]
fn list_jwts_errors_when_missing_jwts_files() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let out = run_api_gql(
        root,
        &["call", "--config-dir", "setup/graphql", "--list-jwts"],
        &[],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr_text()
            .contains("jwts(.local).env not found (expected under setup/graphql/)")
    );
}

#[test]
fn env_endpoint_prefers_endpoints_local_over_endpoints_env() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let server_a = start_server();
    let server_b = start_server();
    write_text(
        &setup_dir.join("endpoints.env"),
        &format!("GQL_URL_STAGING={}/graphql\n", server_a.url()),
    );
    write_text(
        &setup_dir.join("endpoints.local.env"),
        &format!("GQL_URL_STAGING={}/graphql\n", server_b.url()),
    );

    write_text(&root.join("q.graphql"), "query Q { ok }\n");
    let out = run_api_gql(
        root,
        &[
            "call",
            "--no-history",
            "--config-dir",
            "setup/graphql",
            "--env",
            "staging",
            "q.graphql",
        ],
        &[],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(
        out.stdout_text().contains("\"ok\":true"),
        "stdout={}",
        out.stdout_text()
    );

    assert_eq!(server_a.take_requests().len(), 0);
    assert_eq!(server_b.take_requests().len(), 1);
}

#[test]
fn gql_env_default_is_used_when_no_env_or_url_is_provided() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let server = start_server();
    write_text(
        &setup_dir.join("endpoints.env"),
        &format!(
            "GQL_ENV_DEFAULT=staging\nGQL_URL_STAGING={}/graphql\n",
            server.url()
        ),
    );

    write_text(&root.join("q.graphql"), "query Q { ok }\n");
    let out = run_api_gql(
        root,
        &[
            "call",
            "--no-history",
            "--config-dir",
            "setup/graphql",
            "q.graphql",
        ],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].path, "/graphql");
}

#[test]
fn gql_url_env_overrides_gql_env_default() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let server_default = start_server();
    let server_env = start_server();
    write_text(
        &setup_dir.join("endpoints.env"),
        &format!(
            "GQL_ENV_DEFAULT=staging\nGQL_URL_STAGING={}/graphql\n",
            server_default.url()
        ),
    );

    write_text(&root.join("q.graphql"), "query Q { ok }\n");
    let out = run_api_gql(
        root,
        &[
            "call",
            "--no-history",
            "--config-dir",
            "setup/graphql",
            "q.graphql",
        ],
        &[("GQL_URL", &format!("{}/graphql", server_env.url()))],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    assert_eq!(server_default.take_requests().len(), 0);
    assert_eq!(server_env.take_requests().len(), 1);
}

#[test]
fn unknown_env_error_lists_available_suffixes() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    write_text(
        &setup_dir.join("endpoints.env"),
        "GQL_URL_LOCAL=http://example.invalid/graphql\n",
    );
    write_text(
        &setup_dir.join("endpoints.local.env"),
        "GQL_URL_STAGING=http://example.invalid/graphql\n",
    );
    write_text(&root.join("q.graphql"), "query Q { ok }\n");

    let out = run_api_gql(
        root,
        &[
            "call",
            "--no-history",
            "--config-dir",
            "setup/graphql",
            "--env",
            "does-not-exist",
            "q.graphql",
        ],
        &[],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().contains("Unknown --env 'does-not-exist'"));
    assert!(out.stderr_text().contains("available:"));
    assert!(out.stderr_text().contains("local"));
    assert!(out.stderr_text().contains("staging"));
}

#[test]
fn jwt_flag_wins_over_env_and_file_profile_selection() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let server = start_server();
    write_text(
        &setup_dir.join("endpoints.env"),
        &format!("GQL_URL_LOCAL={}/graphql\n", server.url()),
    );
    write_text(
        &setup_dir.join("jwts.env"),
        r#"
GQL_JWT_NAME=file
GQL_JWT_FILE=file_token
GQL_JWT_ENV=env_token
GQL_JWT_ARG=arg_token
"#,
    );

    write_text(&root.join("q.graphql"), "query Q { ok }\n");
    let out = run_api_gql(
        root,
        &[
            "call",
            "--no-history",
            "--config-dir",
            "setup/graphql",
            "--env",
            "local",
            "--jwt",
            "arg",
            "q.graphql",
        ],
        &[("GQL_JWT_NAME", "env")],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].path, "/graphql");
    assert_eq!(
        reqs[0].header_value("authorization").as_deref(),
        Some("Bearer arg_token")
    );
}

#[test]
fn jwt_env_profile_selection_wins_over_file_default() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let server = start_server();
    write_text(
        &setup_dir.join("endpoints.env"),
        &format!("GQL_URL_LOCAL={}/graphql\n", server.url()),
    );
    write_text(
        &setup_dir.join("jwts.env"),
        r#"
GQL_JWT_NAME=file
GQL_JWT_FILE=file_token
GQL_JWT_ENV=env_token
"#,
    );

    write_text(&root.join("q.graphql"), "query Q { ok }\n");
    let out = run_api_gql(
        root,
        &[
            "call",
            "--no-history",
            "--config-dir",
            "setup/graphql",
            "--env",
            "local",
            "q.graphql",
        ],
        &[("GQL_JWT_NAME", "env")],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].header_value("authorization").as_deref(),
        Some("Bearer env_token")
    );
}

#[test]
fn jwt_file_profile_selection_is_used_when_no_flag_or_env() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let server = start_server();
    write_text(
        &setup_dir.join("endpoints.env"),
        &format!("GQL_URL_LOCAL={}/graphql\n", server.url()),
    );
    write_text(
        &setup_dir.join("jwts.env"),
        r#"
GQL_JWT_NAME=file
GQL_JWT_FILE=file_token
"#,
    );

    write_text(&root.join("q.graphql"), "query Q { ok }\n");
    let out = run_api_gql(
        root,
        &[
            "call",
            "--no-history",
            "--config-dir",
            "setup/graphql",
            "--env",
            "local",
            "q.graphql",
        ],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].header_value("authorization").as_deref(),
        Some("Bearer file_token")
    );
}

#[test]
fn access_token_is_used_when_no_jwt_profile_is_selected() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let server = start_server();
    write_text(
        &setup_dir.join("endpoints.env"),
        &format!("GQL_URL_LOCAL={}/graphql\n", server.url()),
    );

    write_text(&root.join("q.graphql"), "query Q { ok }\n");
    let out = run_api_gql(
        root,
        &[
            "call",
            "--no-history",
            "--config-dir",
            "setup/graphql",
            "--env",
            "local",
            "q.graphql",
        ],
        &[("ACCESS_TOKEN", "access-token")],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].header_value("authorization").as_deref(),
        Some("Bearer access-token")
    );
}
