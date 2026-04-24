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

fn run_api_gql(cwd: &Path, args: &[&str]) -> CmdOutput {
    run_api_gql_with(cwd, args, &[])
}

fn run_api_gql_with(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default().with_cwd(cwd).with_env_remove_many(&[
        "GQL_HISTORY_ENABLED",
        "GQL_HISTORY_FILE",
        "GQL_HISTORY_LOG_URL_ENABLED",
        "GQL_URL",
        "GQL_ENV_DEFAULT",
        "GQL_JWT_NAME",
        "GQL_JWT_VALIDATE_ENABLED",
        "ACCESS_TOKEN",
        "SERVICE_TOKEN",
        "GQL_SCHEMA_FILE",
    ]);
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
fn history_empty_file_exits_3() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let history_file = setup_dir.join(".gql_history");
    std::fs::write(&history_file, "").expect("write history");

    let out = run_api_gql(
        root,
        &["history", "--config-dir", "setup/graphql", "--last"],
    );

    assert_eq!(out.code, 3, "stderr={}", out.stderr_text());
    assert_eq!(out.stdout_text(), "");
    assert_eq!(out.stderr_text(), "");
}

#[test]
fn history_command_only_strips_metadata_and_preserves_blank_lines() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let history_file = setup_dir.join(".gql_history");
    let history_body = concat!(
        "# stamp exit=0 setup_dir=.\n",
        "api-gql call \\\n",
        "  --config-dir 'setup/graphql' \\\n",
        "  ops/one.graphql \\\n",
        "| jq .\n",
        "\n",
        "# stamp exit=1 setup_dir=.\n",
        "api-gql call \\\n",
        "  --config-dir 'setup/graphql' \\\n",
        "  ops/two.graphql \\\n",
        "| jq .\n",
        "\n",
    );
    std::fs::write(&history_file, history_body).expect("write history");

    let out = run_api_gql(
        root,
        &[
            "history",
            "--config-dir",
            "setup/graphql",
            "--tail",
            "2",
            "--command-only",
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let stdout = out.stdout_text();
    let expected = concat!(
        "api-gql call \\\n",
        "  --config-dir 'setup/graphql' \\\n",
        "  ops/one.graphql \\\n",
        "| jq .\n",
        "\n",
        "api-gql call \\\n",
        "  --config-dir 'setup/graphql' \\\n",
        "  ops/two.graphql \\\n",
        "| jq .\n",
        "\n",
    );
    assert_eq!(stdout, expected);
    assert!(!stdout.contains("# stamp"));
}

#[test]
fn history_records_service_token_env_source() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/graphql");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let server = start_server();
    write_text(&root.join("q.graphql"), "query Q { ok }\n");

    let out = run_api_gql_with(
        root,
        &[
            "call",
            "--config-dir",
            "setup/graphql",
            "--url",
            &server.url(),
            "q.graphql",
        ],
        &[
            ("GQL_HISTORY_ENABLED", "true"),
            ("GQL_JWT_VALIDATE_ENABLED", "false"),
            ("SERVICE_TOKEN", "svc-token"),
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let history_file = setup_dir.join(".gql_history");
    let content = std::fs::read_to_string(&history_file).expect("read history");
    assert!(content.contains("token=SERVICE_TOKEN"));
    assert!(!content.contains("token=ACCESS_TOKEN"));
}
