use api_testing_core::graphql::report::{GraphqlReport, render_graphql_report_markdown};
use api_testing_core::history::{HistoryWriter, RotationPolicy, read_records};
use api_testing_core::markdown::format_json_pretty_sorted;
use api_testing_core::redact::{REDACTED, redact_json};
use api_testing_core::rest::report::{
    RestReport, RestReportAssertion, render_rest_report_markdown,
};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn command_only(record: &str) -> String {
    if record.starts_with('#') {
        record
            .split_once('\n')
            .map(|(_first, rest)| rest.to_string())
            .unwrap_or_default()
    } else {
        record.to_string()
    }
}

#[test]
fn rest_report_includes_command_snippet_section() {
    let report = RestReport {
        report_date: "2026-02-01".to_string(),
        case_name: "Health".to_string(),
        generated_at: "2026-02-01T00:00:00Z".to_string(),
        endpoint_note: "Endpoint: http://localhost:8080/health".to_string(),
        result_note: "Result: OK".to_string(),
        command_snippet: Some("api-rest call --env staging health.request.json".to_string()),
        assertions: vec![RestReportAssertion {
            label: "status".to_string(),
            state: "pass".to_string(),
        }],
        request_json: r#"{"method":"GET","path":"/health"}"#.to_string(),
        response_lang: "json".to_string(),
        response_body: r#"{"ok":true}"#.to_string(),
        stderr_note: None,
    };

    let markdown = render_rest_report_markdown(&report);
    assert!(markdown.contains("## Command\n"));
    assert!(markdown.contains("```bash\napi-rest call --env staging"));
    assert!(markdown.contains("### Request\n"));
    assert!(markdown.contains("### Response\n"));
}

#[test]
fn graphql_report_includes_variables_note_and_section() {
    let report = GraphqlReport {
        report_date: "2026-02-01".to_string(),
        case_name: "GraphQL Health".to_string(),
        generated_at: "2026-02-01T00:00:00Z".to_string(),
        endpoint_note: "Endpoint: http://localhost:8080/graphql".to_string(),
        result_note: "Result: OK".to_string(),
        command_snippet: Some("api-gql call --env staging ops/health.graphql".to_string()),
        operation: "query Health { ok }".to_string(),
        variables_note: Some("Variables (resolved):".to_string()),
        variables_json: r#"{"id":"123"}"#.to_string(),
        response_note: Some("Response:".to_string()),
        response_lang: "json".to_string(),
        response_body: r#"{"data":{"ok":true}}"#.to_string(),
    };

    let markdown = render_graphql_report_markdown(&report);
    assert!(markdown.contains("### GraphQL Operation\n"));
    assert!(markdown.contains("### GraphQL Operation (Variables)\n"));
    assert!(markdown.contains("Variables (resolved):"));
    assert!(markdown.contains("```graphql\nquery Health { ok }"));
}

#[test]
fn redaction_masks_secrets_in_report_json() {
    let mut value = serde_json::json!({
        "access_token": "secret",
        "nested": { "Authorization": "Bearer abc" },
        "ok": true
    });

    redact_json(&mut value).unwrap();
    let formatted = format_json_pretty_sorted(&value).unwrap();

    assert!(formatted.contains(REDACTED));
    assert!(!formatted.contains("secret"));
    assert!(!formatted.contains("Bearer abc"));
}

#[test]
fn history_rotation_and_command_only_extraction() {
    let tmp = TempDir::new().expect("tmp");
    let history_file = tmp.path().join(".rest_history");

    std::fs::write(&history_file, vec![b'a'; 1024 * 1024]).expect("write big file");

    let record = "# 2026-02-01T00:00:00Z exit=0 setup_dir=.\napi-rest call \\\n  --config-dir setup/rest \\\n  requests/health.request.json \\\n| jq .\n\n";
    let writer = HistoryWriter::new(history_file.clone(), RotationPolicy { max_mb: 1, keep: 1 });
    let appended = writer.append(record).unwrap();
    assert!(appended);

    let rotated = tmp.path().join(".rest_history.1");
    assert!(rotated.is_file());

    let records = read_records(&history_file).unwrap();
    assert_eq!(records.len(), 1);
    let extracted = command_only(&records[0]);
    assert!(extracted.starts_with("api-rest call"));
    assert!(extracted.contains("--config-dir"));
}

#[test]
fn command_only_keeps_records_without_metadata() {
    let record = "api-rest call --env staging request.json\n\n";
    let extracted = command_only(record);
    assert_eq!(extracted, record);
}
