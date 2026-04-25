// This file is included as the `preflight` submodule of the integration test
// target, and internally pulls in `src/preflight.rs` under the same name to
// exercise it as a white-box SUT. The double-nesting trips clippy's
// module_inception lint, which is benign here.
#[allow(dead_code, clippy::module_inception)]
#[path = "../../src/preflight.rs"]
mod preflight;

use preflight::{
    ACCESSIBILITY_HINT, AUTOMATION_HINT, CLICLICK_INSTALL_HINT, PermissionSignal, PermissionState,
    ProbeSnapshot, SCREEN_RECORDING_HINT, build_report, render_json, render_text,
};
use pretty_assertions::assert_eq;
use serde_json::json;

fn ready_signal(detail: &str) -> PermissionSignal {
    PermissionSignal {
        state: PermissionState::Ready,
        detail: detail.to_string(),
    }
}

#[test]
fn preflight_missing_cliclick_has_explicit_install_hint() {
    let snapshot = ProbeSnapshot {
        osascript_path: Some("/usr/bin/osascript".to_string()),
        cliclick_path: None,
        accessibility_signal: ready_signal("System Events reports UI scripting is enabled."),
        automation_signal: ready_signal("Apple Events access to System Events is allowed."),
        screen_recording_signal: ready_signal("Screen Recording preflight passed."),
    };

    let report = build_report(snapshot, false);
    let check = report
        .check("cliclick")
        .expect("cliclick check should exist");
    assert_eq!(check.status, preflight::CheckStatus::Fail);
    assert_eq!(check.hint.as_deref(), Some(CLICLICK_INSTALL_HINT));

    let text = render_text(&report);
    assert!(
        text.contains("brew install cliclick"),
        "text output: {text}"
    );
}

#[test]
fn preflight_json_structure_is_deterministic() {
    let snapshot = ProbeSnapshot {
        osascript_path: Some("/usr/bin/osascript".to_string()),
        cliclick_path: Some("/opt/homebrew/bin/cliclick".to_string()),
        accessibility_signal: ready_signal("System Events reports UI scripting is enabled."),
        automation_signal: PermissionSignal::blocked(
            "Apple Events access to System Events is blocked.",
        ),
        screen_recording_signal: PermissionSignal::unknown(
            "Advisory only. Screen Recording is validated when observe screenshot runs.",
        ),
    };

    let report = build_report(snapshot, false);
    let payload = render_json(&report);

    let expected = json!({
        "schema_version": 1,
        "ok": false,
        "command": "preflight",
        "result": {
            "strict": false,
            "status": "not_ready",
            "summary": {
                "blocking_failures": 1,
                "warnings": 1
            },
            "checks": [
                {
                    "id": "osascript",
                    "label": "osascript",
                    "status": "ok",
                    "blocking": true,
                    "message": "found at /usr/bin/osascript",
                    "hint": null
                },
                {
                    "id": "cliclick",
                    "label": "cliclick",
                    "status": "ok",
                    "blocking": true,
                    "message": "found at /opt/homebrew/bin/cliclick",
                    "hint": null
                },
                {
                    "id": "accessibility",
                    "label": "Accessibility",
                    "status": "ok",
                    "blocking": true,
                    "message": "System Events reports UI scripting is enabled.",
                    "hint": null
                },
                {
                    "id": "automation",
                    "label": "Automation",
                    "status": "fail",
                    "blocking": true,
                    "message": "Apple Events access to System Events is blocked.",
                    "hint": AUTOMATION_HINT
                },
                {
                    "id": "screen_recording",
                    "label": "Screen Recording",
                    "status": "warn",
                    "blocking": false,
                    "message": "Advisory only. Screen Recording is validated when observe screenshot runs.",
                    "hint": SCREEN_RECORDING_HINT
                }
            ],
            "permissions": {
                "screen_recording": "unknown",
                "accessibility": "ready",
                "automation": "blocked",
                "ready": false,
                "hints": [
                    AUTOMATION_HINT,
                    SCREEN_RECORDING_HINT
                ]
            }
        }
    });

    assert_eq!(payload, expected);
}

#[test]
fn preflight_permissions_schema_ready_is_consistent_with_component_states() {
    let snapshot = ProbeSnapshot {
        osascript_path: Some("/usr/bin/osascript".to_string()),
        cliclick_path: Some("/opt/homebrew/bin/cliclick".to_string()),
        accessibility_signal: ready_signal("System Events reports UI scripting is enabled."),
        automation_signal: ready_signal("Apple Events access to System Events is allowed."),
        screen_recording_signal: PermissionSignal::unknown(
            "Advisory only. Screen Recording is validated when observe screenshot runs.",
        ),
    };

    let report = build_report(snapshot, false);
    let payload = render_json(&report);

    assert_eq!(
        payload["result"]["permissions"]["ready"],
        serde_json::json!(true)
    );
    assert_eq!(
        payload["result"]["permissions"]["hints"],
        serde_json::json!([SCREEN_RECORDING_HINT])
    );
}

#[test]
fn preflight_text_output_is_deterministic() {
    let snapshot = ProbeSnapshot {
        osascript_path: Some("/usr/bin/osascript".to_string()),
        cliclick_path: None,
        accessibility_signal: PermissionSignal::blocked(
            "Accessibility access is blocked for this terminal host.",
        ),
        automation_signal: PermissionSignal::blocked(
            "Apple Events access to System Events is blocked.",
        ),
        screen_recording_signal: PermissionSignal::unknown(
            "Advisory only. Screen Recording is validated when observe screenshot runs.",
        ),
    };

    let report = build_report(snapshot, false);
    let text = render_text(&report);

    let expected = [
        "preflight: not ready (strict=false)".to_string(),
        "blocking_failures: 3, warnings: 1".to_string(),
        "- [ok] osascript: found at /usr/bin/osascript".to_string(),
        "- [fail] cliclick: not found in PATH".to_string(),
        format!("  hint: {CLICLICK_INSTALL_HINT}"),
        "- [fail] Accessibility: Accessibility access is blocked for this terminal host."
            .to_string(),
        format!("  hint: {ACCESSIBILITY_HINT}"),
        "- [fail] Automation: Apple Events access to System Events is blocked.".to_string(),
        format!("  hint: {AUTOMATION_HINT}"),
        "- [warn] Screen Recording: Advisory only. Screen Recording is validated when observe screenshot runs."
            .to_string(),
        format!("  hint: {SCREEN_RECORDING_HINT}"),
    ]
    .join("\n");

    assert_eq!(text, expected);
}
