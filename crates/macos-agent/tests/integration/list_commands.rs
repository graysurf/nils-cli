use tempfile::TempDir;

use crate::common;
#[test]
fn windows_list_tsv_is_deterministic() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let first = harness.run(cwd.path(), &["--format", "tsv", "windows", "list"]);
    let second = harness.run(cwd.path(), &["--format", "tsv", "windows", "list"]);

    assert_eq!(first.code, 0, "stderr: {}", first.stderr_text());
    assert_eq!(second.code, 0, "stderr: {}", second.stderr_text());
    assert_eq!(first.stdout_text(), second.stdout_text());
    assert_eq!(first.stderr_text(), "");

    let stdout = first.stdout_text();
    let lines = stdout
        .trim()
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();

    assert_eq!(
        lines,
        vec![
            "200\tFinder\tFinder\t80\t80\t900\t600\ttrue",
            "101\tTerminal\tDocs\t40\t40\t1100\t760\ttrue",
            "100\tTerminal\tInbox\t0\t0\t1200\t800\ttrue",
        ]
    );
}

#[test]
fn apps_list_json_has_schema_version() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(cwd.path(), &["--format", "json", "apps", "list"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_text(), "");

    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be valid json");
    assert_eq!(payload["schema_version"], serde_json::json!(1));
    assert_eq!(payload["ok"], serde_json::json!(true));
    assert_eq!(payload["command"], serde_json::json!("apps.list"));

    let names = payload["result"]["apps"]
        .as_array()
        .expect("apps array")
        .iter()
        .map(|app| app["app_name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["Finder".to_string(), "Terminal".to_string()]);
}

#[test]
fn windows_list_json_and_apps_list_tsv_are_both_supported() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let windows_json = harness.run(cwd.path(), &["--format", "json", "windows", "list"]);
    assert_eq!(
        windows_json.code,
        0,
        "stderr: {}",
        windows_json.stderr_text()
    );
    let payload: serde_json::Value =
        serde_json::from_str(&windows_json.stdout_text()).expect("json payload");
    assert_eq!(payload["command"], serde_json::json!("windows.list"));
    assert!(payload["result"]["windows"].as_array().unwrap().len() >= 2);

    let apps_tsv = harness.run(cwd.path(), &["--format", "tsv", "apps", "list"]);
    assert_eq!(apps_tsv.code, 0, "stderr: {}", apps_tsv.stderr_text());
    let apps_stdout = apps_tsv.stdout_text();
    let lines = apps_stdout
        .trim()
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines[0], "Finder\t222\tcom.apple.Finder");
}

#[test]
fn ax_list_json_emits_expected_node_fields() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness.cmd_options(cwd.path()).with_env(
        "AGENTS_MACOS_AGENT_AX_LIST_JSON",
        r#"{
          "nodes":[
            {
              "node_id":"1.1",
              "role":"AXButton",
              "subrole":"AXStandardWindowButton",
              "title":"Open",
              "identifier":"open-button",
              "value_preview":null,
              "enabled":true,
              "focused":false,
              "frame":{"x":100.0,"y":120.0,"width":88.0,"height":22.0},
              "actions":["AXPress"],
              "path":["1","1"]
            }
          ],
          "warnings":[]
        }"#,
    );

    let out = harness.run_with_options(cwd.path(), &["--format", "json", "ax", "list"], options);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_text(), "");

    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be valid json");
    assert_eq!(payload["command"], serde_json::json!("ax.list"));
    let node = &payload["result"]["nodes"][0];
    assert_eq!(node["node_id"], serde_json::json!("1.1"));
    assert_eq!(node["role"], serde_json::json!("AXButton"));
    assert_eq!(node["title"], serde_json::json!("Open"));
    assert_eq!(node["identifier"], serde_json::json!("open-button"));
    assert_eq!(node["actions"][0], serde_json::json!("AXPress"));
}
