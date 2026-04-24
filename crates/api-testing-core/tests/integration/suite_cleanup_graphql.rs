use api_testing_core::suite::cleanup::{CleanupContext, run_case_cleanup};
use api_testing_core::suite::schema::{SuiteCleanup, SuiteCleanupStep, SuiteDefaults};
use nils_test_support::fixtures::write_text;
use nils_test_support::http::{HttpResponse, LoopbackServer};
use tempfile::TempDir;

fn build_cleanup(
    op: &str,
    url: &str,
    vars_template: &str,
    vars: serde_json::Value,
) -> SuiteCleanup {
    SuiteCleanup::One(Box::new(SuiteCleanupStep {
        step_type: "graphql".to_string(),
        config_dir: String::new(),
        url: url.to_string(),
        env: String::new(),
        no_history: None,
        method: String::new(),
        path_template: String::new(),
        vars: Some(vars),
        token: String::new(),
        expect: None,
        expect_status: None,
        expect_jq: ".data.ok == true".to_string(),
        jwt: String::new(),
        op: op.to_string(),
        vars_jq: String::new(),
        vars_template: vars_template.to_string(),
        allow_errors: false,
    }))
}

#[test]
fn suite_cleanup_graphql_success_sends_vars() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).expect("repo marker");

    let run_dir = root.join("out");
    std::fs::create_dir_all(&run_dir).expect("run dir");

    let main_response_file = run_dir.join("main.response.json");
    write_text(&main_response_file, r#"{"data":{"id":"123"}}"#);
    let main_stderr_file = run_dir.join("main.stderr.log");
    write_text(&main_stderr_file, "");

    let op_path = root.join("ops/cleanup.graphql");
    std::fs::create_dir_all(op_path.parent().expect("op dir")).expect("op dir create");
    write_text(&op_path, "query Cleanup($id: String!) { ok }\n");

    let vars_template_path = root.join("templates/vars.json");
    std::fs::create_dir_all(vars_template_path.parent().expect("template dir"))
        .expect("template dir create");
    write_text(&vars_template_path, r#"{"id":"{{id}}"}"#);

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "POST",
        "/graphql",
        HttpResponse::new(200, r#"{"data":{"ok":true}}"#),
    );

    let cleanup = build_cleanup(
        "ops/cleanup.graphql",
        &format!("{}/graphql", server.url()),
        "templates/vars.json",
        serde_json::json!({"id": ".data.id"}),
    );
    let defaults = SuiteDefaults::default();
    let mut ctx = CleanupContext {
        repo_root: root,
        run_dir: &run_dir,
        case_id: "case",
        safe_id: "case",
        main_response_file: Some(&main_response_file),
        main_stderr_file: &main_stderr_file,
        allow_writes_flag: true,
        effective_env: "staging",
        effective_no_history: true,
        suite_defaults: &defaults,
        env_rest_url: "",
        env_gql_url: "",
        rest_config_dir: "setup/rest",
        rest_url: "",
        rest_token: "",
        gql_config_dir: "setup/graphql",
        gql_url: "",
        gql_jwt: "",
        access_token_for_case: "",
        auth_manager: None,
        cleanup: Some(&cleanup),
    };

    assert!(run_case_cleanup(&mut ctx).unwrap());

    let stderr_text = std::fs::read_to_string(&main_stderr_file).expect("stderr read");
    assert_eq!(stderr_text, "");

    let requests = server.take_requests();
    let req = requests
        .iter()
        .find(|r| r.method == "POST" && r.path == "/graphql")
        .expect("graphql request");
    let body: serde_json::Value = serde_json::from_str(&req.body_text()).expect("json body");
    assert!(
        body.get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains("query Cleanup")
    );
    assert_eq!(
        body.get("variables"),
        Some(&serde_json::json!({"id":"123"}))
    );

    let cleanup_stdout = run_dir.join("case.cleanup.0.response.json");
    let cleanup_stdout_text = std::fs::read_to_string(&cleanup_stdout).expect("cleanup stdout");
    assert_eq!(cleanup_stdout_text, r#"{"data":{"ok":true}}"#);
}

#[test]
fn suite_cleanup_graphql_failure_errors_present_is_logged() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).expect("repo marker");

    let run_dir = root.join("out");
    std::fs::create_dir_all(&run_dir).expect("run dir");

    let main_response_file = run_dir.join("main.response.json");
    write_text(&main_response_file, r#"{"data":{"id":"123"}}"#);
    let main_stderr_file = run_dir.join("main.stderr.log");
    write_text(&main_stderr_file, "");

    let op_path = root.join("ops/cleanup.graphql");
    std::fs::create_dir_all(op_path.parent().expect("op dir")).expect("op dir create");
    write_text(&op_path, "query Cleanup($id: String!) { ok }\n");

    let vars_template_path = root.join("templates/vars.json");
    std::fs::create_dir_all(vars_template_path.parent().expect("template dir"))
        .expect("template dir create");
    write_text(&vars_template_path, r#"{"id":"{{id}}"}"#);

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "POST",
        "/graphql",
        HttpResponse::new(200, r#"{"errors":[{"message":"boom"}]}"#),
    );

    let cleanup = build_cleanup(
        "ops/cleanup.graphql",
        &format!("{}/graphql", server.url()),
        "templates/vars.json",
        serde_json::json!({"id": ".data.id"}),
    );
    let defaults = SuiteDefaults::default();
    let mut ctx = CleanupContext {
        repo_root: root,
        run_dir: &run_dir,
        case_id: "case",
        safe_id: "case",
        main_response_file: Some(&main_response_file),
        main_stderr_file: &main_stderr_file,
        allow_writes_flag: true,
        effective_env: "staging",
        effective_no_history: true,
        suite_defaults: &defaults,
        env_rest_url: "",
        env_gql_url: "",
        rest_config_dir: "setup/rest",
        rest_url: "",
        rest_token: "",
        gql_config_dir: "setup/graphql",
        gql_url: "",
        gql_jwt: "",
        access_token_for_case: "",
        auth_manager: None,
        cleanup: Some(&cleanup),
    };

    assert!(!run_case_cleanup(&mut ctx).unwrap());

    let stderr_text = std::fs::read_to_string(&main_stderr_file).expect("stderr read");
    assert!(
        stderr_text.contains("cleanup(graphql) errors present: step[0] op=ops/cleanup.graphql")
    );
}
