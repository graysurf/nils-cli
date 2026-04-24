use std::path::Path;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_with};
use nils_test_support::fs::write_text;
use nils_test_support::http::{HttpResponse, RecordedRequest, TestServer};

const SECRET_TOKEN: &str = "VERY_SECRET_TOKEN";

fn api_test_bin() -> std::path::PathBuf {
    resolve("api-test")
}

fn run_api_test(cwd: &Path, args: &[&str]) -> CmdOutput {
    run_api_test_with_env(cwd, args, &[])
}

fn run_api_test_with_env(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default().with_cwd(cwd).with_env_remove_many(&[
        "ACCESS_TOKEN",
        "SERVICE_TOKEN",
        "REST_TOKEN_NAME",
        "GQL_JWT_NAME",
    ]);
    for (k, v) in env {
        options = options.with_env(k, v);
    }
    run_with(&api_test_bin(), args, &options)
}

fn start_server() -> TestServer {
    TestServer::new(
        |req: &RecordedRequest| match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/health") => HttpResponse::new(200, r#"{"ok":true}"#)
                .with_header("Content-Type", "application/json"),
            ("GET", "/login") => {
                HttpResponse::new(200, format!(r#"{{"accessToken":"{SECRET_TOKEN}"}}"#))
                    .with_header("Content-Type", "application/json")
            }
            ("GET", "/me") => {
                let auth = req.header_value("authorization").unwrap_or_default();
                if auth == format!("Bearer {SECRET_TOKEN}") {
                    HttpResponse::new(200, r#"{"me":{"ok":true}}"#)
                        .with_header("Content-Type", "application/json")
                } else {
                    HttpResponse::new(401, r#"{"error":"unauthorized"}"#)
                        .with_header("Content-Type", "application/json")
                }
            }
            ("POST", "/graphql") => HttpResponse::new(200, r#"{"data":{"ok":true}}"#)
                .with_header("Content-Type", "application/json"),
            _ => HttpResponse::new(404, r#"{"error":"not_found"}"#)
                .with_header("Content-Type", "application/json"),
        },
    )
    .expect("start test server")
}

#[test]
fn run_e2e_suite_smoke_passes_and_does_not_leak_secrets() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).expect("mkdir .git");

    let server = start_server();

    // REST requests
    write_text(
        &root.join("setup/rest/requests/health.request.json"),
        r#"{"method":"GET","path":"/health"}"#,
    );
    write_text(
        &root.join("setup/rest/requests/login.request.json"),
        r#"{"method":"GET","path":"/login"}"#,
    );
    write_text(
        &root.join("setup/rest/requests/me.request.json"),
        r#"{"method":"GET","path":"/me"}"#,
    );
    write_text(
        &root.join("setup/rest/requests/write.request.json"),
        r#"{"method":"POST","path":"/write"}"#,
    );

    // GraphQL ops
    write_text(
        &root.join("setup/graphql/operations/health.graphql"),
        "query Q { ok }\n",
    );
    write_text(
        &root.join("setup/graphql/operations/mutation.graphql"),
        "mutation M { write { ok } }\n",
    );

    // Suite file
    let smoke_suite_json = serde_json::json!({
      "version": 1,
      "name": "smoke",
      "defaults": {
        "env": "staging",
        "noHistory": true,
        "rest": { "url": server.url() },
        "graphql": { "url": format!("{}/graphql", server.url()) }
      },
      "cases": [
        { "id": "rest.health", "type": "rest", "tags": ["smoke"], "request": "setup/rest/requests/health.request.json" },
        { "id": "graphql.health", "type": "graphql", "tags": ["smoke"], "op": "setup/graphql/operations/health.graphql" },
        {
          "id": "rest_flow.me",
          "type": "rest-flow",
          "tags": ["smoke"],
          "loginRequest": "setup/rest/requests/login.request.json",
          "request": "setup/rest/requests/me.request.json",
          "tokenJq": ".accessToken"
        }
      ]
    });
    write_text(
        &root.join("tests/api/suites/smoke.suite.json"),
        &serde_json::to_string_pretty(&smoke_suite_json).expect("suite json"),
    );

    let out = run_api_test_with_env(
        root,
        &[
            "run",
            "--suite",
            "smoke",
            "--out",
            "out/smoke/results.json",
            "--junit",
            "out/smoke/junit.xml",
        ],
        &[("API_TEST_OUTPUT_DIR", "out/api-test-runner-smoke")],
    );

    assert_eq!(
        out.code,
        0,
        "stdout={}\nstderr={}",
        out.stdout_text(),
        out.stderr_text()
    );

    let stdout_text = out.stdout_text();
    assert!(!stdout_text.contains(SECRET_TOKEN));

    let results_file = root.join("out/smoke/results.json");
    assert!(results_file.is_file());
    let file_bytes = std::fs::read(&results_file).expect("read results");
    assert_eq!(file_bytes, out.stdout);

    let junit_file = root.join("out/smoke/junit.xml");
    assert!(junit_file.is_file());
    let junit_text = std::fs::read_to_string(&junit_file).expect("read junit");
    assert!(junit_text.contains("<testsuite"));

    let results_json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("results json");
    assert_eq!(results_json["summary"]["total"], 3);
    assert_eq!(results_json["summary"]["passed"], 3);
    assert_eq!(results_json["summary"]["failed"], 0);
    assert_eq!(results_json["summary"]["skipped"], 0);

    let output_dir_rel = results_json["outputDir"].as_str().unwrap_or("");
    assert!(!output_dir_rel.is_empty());
    let output_dir_abs = root.join(output_dir_rel);
    assert!(output_dir_abs.is_dir());

    // Ensure referenced artifacts exist and do not contain secrets.
    if let Some(cases) = results_json["cases"].as_array() {
        for c in cases {
            if let Some(stdout_rel) = c.get("stdoutFile").and_then(|v| v.as_str()) {
                let p = root.join(stdout_rel);
                assert!(p.is_file(), "missing stdout file: {}", p.display());
                let bytes = std::fs::read(&p).unwrap();
                assert!(!String::from_utf8_lossy(&bytes).contains(SECRET_TOKEN));
            }
            if let Some(stderr_rel) = c.get("stderrFile").and_then(|v| v.as_str()) {
                let p = root.join(stderr_rel);
                assert!(p.is_file(), "missing stderr file: {}", p.display());
                let bytes = std::fs::read(&p).unwrap();
                assert!(!String::from_utf8_lossy(&bytes).contains(SECRET_TOKEN));
            }
        }
    }

    let out2 = run_api_test(
        root,
        &[
            "summary",
            "--in",
            "out/smoke/results.json",
            "--out",
            "out/smoke/summary.md",
            "--slow",
            "5",
        ],
    );
    assert_eq!(out2.code, 0);
    let summary_file = root.join("out/smoke/summary.md");
    assert!(summary_file.is_file());
    let summary_text = std::fs::read_to_string(&summary_file).expect("read summary");
    assert!(summary_text.contains("API test summary"));

    // Sanity: rest-flow actually hit /me with Authorization.
    let reqs = server.take_requests();
    assert!(reqs.iter().any(|r| r.path == "/me"));
}

#[test]
fn run_e2e_suite_guardrails_fails_with_expected_messages() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).expect("mkdir .git");

    let server = start_server();

    // REST requests
    write_text(
        &root.join("setup/rest/requests/write.request.json"),
        r#"{"method":"POST","path":"/write"}"#,
    );

    // GraphQL ops
    write_text(
        &root.join("setup/graphql/operations/mutation.graphql"),
        "mutation M { write { ok } }\n",
    );

    let guardrails_suite_json = serde_json::json!({
      "version": 1,
      "name": "guardrails",
      "defaults": {
        "env": "staging",
        "noHistory": true,
        "rest": { "url": server.url() },
        "graphql": { "url": format!("{}/graphql", server.url()) }
      },
      "cases": [
        { "id": "rest.write_no_allow", "type": "rest", "tags": ["guardrails"], "allowWrite": false, "request": "setup/rest/requests/write.request.json" },
        { "id": "rest.write_skip", "type": "rest", "tags": ["guardrails"], "allowWrite": true, "request": "setup/rest/requests/write.request.json" },
        { "id": "graphql.mutation_no_allow", "type": "graphql", "tags": ["guardrails"], "allowWrite": false, "op": "setup/graphql/operations/mutation.graphql" }
      ]
    });
    write_text(
        &root.join("tests/api/suites/guardrails.suite.json"),
        &serde_json::to_string_pretty(&guardrails_suite_json).expect("suite json"),
    );

    let out = run_api_test_with_env(
        root,
        &[
            "run",
            "--suite",
            "guardrails",
            "--out",
            "out/guardrails/results.json",
            "--junit",
            "out/guardrails/junit.xml",
        ],
        &[("API_TEST_OUTPUT_DIR", "out/api-test-runner-guardrails")],
    );

    assert_eq!(
        out.code,
        2,
        "stdout={}\nstderr={}",
        out.stdout_text(),
        out.stderr_text()
    );

    let stdout_text = out.stdout_text();
    assert!(!stdout_text.contains(SECRET_TOKEN));

    let results_file = root.join("out/guardrails/results.json");
    assert!(results_file.is_file());
    let file_bytes = std::fs::read(&results_file).expect("read results");
    assert_eq!(file_bytes, out.stdout);

    let junit_file = root.join("out/guardrails/junit.xml");
    assert!(junit_file.is_file());

    let results_json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("results json");
    assert_eq!(results_json["summary"]["total"], 3);
    assert_eq!(results_json["summary"]["passed"], 0);
    assert_eq!(results_json["summary"]["failed"], 2);
    assert_eq!(results_json["summary"]["skipped"], 1);

    let cases = results_json["cases"].as_array().expect("cases array");
    let mut by_id: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    for c in cases {
        if let Some(id) = c.get("id").and_then(|v| v.as_str()) {
            by_id.insert(id.to_string(), c.clone());
        }
    }

    assert_eq!(by_id["rest.write_no_allow"]["status"], "failed");
    assert_eq!(
        by_id["rest.write_no_allow"]["message"],
        "write_capable_case_requires_allowWrite_true"
    );

    assert_eq!(by_id["graphql.mutation_no_allow"]["status"], "failed");
    assert_eq!(
        by_id["graphql.mutation_no_allow"]["message"],
        "mutation_case_requires_allowWrite_true"
    );

    assert_eq!(by_id["rest.write_skip"]["status"], "skipped");
    assert_eq!(by_id["rest.write_skip"]["message"], "write_cases_disabled");

    let output_dir_rel = results_json["outputDir"].as_str().unwrap_or("");
    assert!(!output_dir_rel.is_empty());
    let output_dir_abs = root.join(output_dir_rel);
    assert!(output_dir_abs.is_dir());
}
