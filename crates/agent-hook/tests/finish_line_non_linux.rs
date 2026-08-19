#![cfg(not(target_os = "linux"))]

mod support;

use pretty_assertions::assert_eq;
use serde_json::json;

use support::Fixture;

const POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "finish-line-non-linux-test"
version = "2026.08.18.1"
"#;

#[test]
fn open_fails_closed_with_the_documented_non_linux_containment_error() {
    let fixture = Fixture::new(POLICY);
    let request = json!({
        "schema_version": "agent-hook.finish-line.open.v1",
        "product": "dsh",
        "session_id": "session-a",
        "turn_id": "turn-1",
        "cwd": fixture.root,
    });
    let output = fixture.run(
        &["finish-line", "open", "--format", "json"],
        Some(&request.to_string()),
    );
    assert_eq!(output.code, 69);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "finish-line-containment-unavailable"
    );
}
