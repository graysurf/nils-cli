use std::process::Command;

use nils_common::{env as shared_env, process as shared_process};
use screen_record::types::PermissionStatusSchema;
use serde_json::{Value, json};

pub use screen_record::types::PermissionState;

pub const CLICLICK_INSTALL_HINT: &str = "Install cliclick with Homebrew: brew install cliclick";
pub const ACCESSIBILITY_HINT: &str = "Open System Settings > Privacy & Security > Accessibility, then enable your terminal app (Terminal, iTerm, or other shell host).";
pub const AUTOMATION_HINT: &str = "Open System Settings > Privacy & Security > Automation, then allow your terminal app to control System Events.";
pub const SCREEN_RECORDING_HINT: &str = "Advisory: if screenshot commands fail, open System Settings > Privacy & Security > Screen Recording and enable your terminal app.";

const ACCESSIBILITY_SCRIPT: &str = r#"tell application "System Events" to get UI elements enabled"#;
const AUTOMATION_SCRIPT: &str = r#"tell application "System Events" to get name of first application process whose frontmost is true"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSignal {
    pub state: PermissionState,
    pub detail: String,
}

impl PermissionSignal {
    pub fn ready(detail: impl Into<String>) -> Self {
        Self {
            state: PermissionState::Ready,
            detail: detail.into(),
        }
    }

    pub fn blocked(detail: impl Into<String>) -> Self {
        Self {
            state: PermissionState::Blocked,
            detail: detail.into(),
        }
    }

    pub fn unknown(detail: impl Into<String>) -> Self {
        Self {
            state: PermissionState::Unknown,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeSnapshot {
    pub osascript_path: Option<String>,
    pub cliclick_path: Option<String>,
    pub accessibility_signal: PermissionSignal,
    pub automation_signal: PermissionSignal,
    pub screen_recording_signal: PermissionSignal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    Fail,
    Warn,
}

impl CheckStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Fail => "fail",
            Self::Warn => "warn",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    pub id: &'static str,
    pub label: &'static str,
    pub status: CheckStatus,
    pub blocking: bool,
    pub message: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreflightSummary {
    pub ok: bool,
    pub blocking_failures: usize,
    pub warnings: usize,
}

impl PreflightSummary {
    pub fn status(self) -> &'static str {
        if self.ok { "ready" } else { "not_ready" }
    }

    fn title(self) -> &'static str {
        if self.ok { "ready" } else { "not ready" }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReport {
    pub strict: bool,
    pub checks: Vec<CheckReport>,
    pub permissions: PermissionStatusSchema,
}

impl PreflightReport {
    pub fn summary(&self) -> PreflightSummary {
        let blocking_failures = self
            .checks
            .iter()
            .filter(|check| check.blocking && check.status == CheckStatus::Fail)
            .count();
        let warnings = self
            .checks
            .iter()
            .filter(|check| check.status == CheckStatus::Warn)
            .count();
        let ok = if self.strict {
            blocking_failures == 0 && warnings == 0
        } else {
            blocking_failures == 0
        };

        PreflightSummary {
            ok,
            blocking_failures,
            warnings,
        }
    }

    #[cfg(test)]
    pub fn check(&self, id: &str) -> Option<&CheckReport> {
        self.checks.iter().find(|check| check.id == id)
    }
}

pub fn collect_system_snapshot() -> ProbeSnapshot {
    let osascript_path = find_in_path("osascript");
    let cliclick_path = find_in_path("cliclick");
    let osascript_available = osascript_path.is_some();

    let accessibility_signal = if osascript_available {
        probe_accessibility()
    } else {
        PermissionSignal::unknown("Skipped because osascript is missing.")
    };
    let automation_signal = if osascript_available {
        probe_automation()
    } else {
        PermissionSignal::unknown("Skipped because osascript is missing.")
    };
    let screen_recording_signal = PermissionSignal::unknown(
        "Advisory only. Screen Recording is validated when observe screenshot runs.",
    );

    ProbeSnapshot {
        osascript_path,
        cliclick_path,
        accessibility_signal,
        automation_signal,
        screen_recording_signal,
    }
}

pub fn build_report(snapshot: ProbeSnapshot, strict: bool) -> PreflightReport {
    build_report_with_probes(snapshot, strict, Vec::new())
}

pub fn build_report_with_probes(
    mut snapshot: ProbeSnapshot,
    strict: bool,
    mut probe_checks: Vec<CheckReport>,
) -> PreflightReport {
    let screenshot_probe_succeeded = probe_checks
        .iter()
        .any(|check| check.id == "probe_screenshot" && check.status == CheckStatus::Ok);
    if snapshot.screen_recording_signal.state == PermissionState::Unknown
        && screenshot_probe_succeeded
    {
        snapshot.screen_recording_signal =
            PermissionSignal::ready("Screenshot probe confirmed Screen Recording access.");
    }

    let permissions = permission_status_from_snapshot(&snapshot);

    let checks = vec![
        tool_check(
            "osascript",
            "osascript",
            snapshot.osascript_path,
            true,
            None,
        ),
        tool_check(
            "cliclick",
            "cliclick",
            snapshot.cliclick_path,
            true,
            Some(CLICLICK_INSTALL_HINT),
        ),
        permission_check(
            "accessibility",
            "Accessibility",
            snapshot.accessibility_signal,
            true,
            ACCESSIBILITY_HINT,
        ),
        permission_check(
            "automation",
            "Automation",
            snapshot.automation_signal,
            true,
            AUTOMATION_HINT,
        ),
        permission_check(
            "screen_recording",
            "Screen Recording",
            snapshot.screen_recording_signal,
            false,
            SCREEN_RECORDING_HINT,
        ),
    ];

    let mut checks = checks;
    checks.append(&mut probe_checks);

    PreflightReport {
        strict,
        checks,
        permissions,
    }
}

fn permission_status_from_snapshot(snapshot: &ProbeSnapshot) -> PermissionStatusSchema {
    let mut hints = Vec::new();
    push_permission_hint_if_not_ready(
        &mut hints,
        snapshot.accessibility_signal.state,
        ACCESSIBILITY_HINT,
    );
    push_permission_hint_if_not_ready(
        &mut hints,
        snapshot.automation_signal.state,
        AUTOMATION_HINT,
    );
    push_permission_hint_if_not_ready(
        &mut hints,
        snapshot.screen_recording_signal.state,
        SCREEN_RECORDING_HINT,
    );

    PermissionStatusSchema::from_components(
        snapshot.screen_recording_signal.state,
        snapshot.accessibility_signal.state,
        snapshot.automation_signal.state,
        hints,
    )
}

fn push_permission_hint_if_not_ready(hints: &mut Vec<String>, state: PermissionState, hint: &str) {
    if state != PermissionState::Ready {
        hints.push(hint.to_string());
    }
}

pub fn run_live_probes() -> Vec<CheckReport> {
    vec![probe_activate(), probe_input_hotkey(), probe_screenshot()]
}

pub fn render_text(report: &PreflightReport) -> String {
    let summary = report.summary();
    let mut lines = Vec::with_capacity(2 + report.checks.len() * 2);
    lines.push(format!(
        "preflight: {} (strict={})",
        summary.title(),
        report.strict
    ));
    lines.push(format!(
        "blocking_failures: {}, warnings: {}",
        summary.blocking_failures, summary.warnings
    ));

    for check in &report.checks {
        lines.push(format!(
            "- [{}] {}: {}",
            check.status.as_str(),
            check.label,
            check.message
        ));
        if let Some(hint) = &check.hint {
            lines.push(format!("  hint: {hint}"));
        }
    }

    lines.join("\n")
}

pub fn render_json(report: &PreflightReport) -> Value {
    let summary = report.summary();
    let checks = report
        .checks
        .iter()
        .map(|check| {
            json!({
                "id": check.id,
                "label": check.label,
                "status": check.status.as_str(),
                "blocking": check.blocking,
                "message": check.message,
                "hint": check.hint,
            })
        })
        .collect::<Vec<_>>();
    let permissions = json!({
        "screen_recording": report.permissions.screen_recording.as_str(),
        "accessibility": report.permissions.accessibility.as_str(),
        "automation": report.permissions.automation.as_str(),
        "ready": report.permissions.ready,
        "hints": report.permissions.hints.clone(),
    });

    json!({
        "schema_version": 1,
        "ok": summary.ok,
        "command": "preflight",
        "result": {
            "strict": report.strict,
            "status": summary.status(),
            "summary": {
                "blocking_failures": summary.blocking_failures,
                "warnings": summary.warnings,
            },
            "checks": checks,
            "permissions": permissions,
        }
    })
}

fn tool_check(
    id: &'static str,
    label: &'static str,
    path: Option<String>,
    blocking: bool,
    missing_hint: Option<&str>,
) -> CheckReport {
    match path {
        Some(path) => CheckReport {
            id,
            label,
            status: CheckStatus::Ok,
            blocking,
            message: format!("found at {path}"),
            hint: None,
        },
        None => CheckReport {
            id,
            label,
            status: CheckStatus::Fail,
            blocking,
            message: "not found in PATH".to_string(),
            hint: missing_hint.map(str::to_string),
        },
    }
}

fn permission_check(
    id: &'static str,
    label: &'static str,
    signal: PermissionSignal,
    blocking: bool,
    guidance: &'static str,
) -> CheckReport {
    match signal.state {
        PermissionState::Ready => CheckReport {
            id,
            label,
            status: CheckStatus::Ok,
            blocking,
            message: signal.detail,
            hint: None,
        },
        PermissionState::Blocked => CheckReport {
            id,
            label,
            status: CheckStatus::Fail,
            blocking,
            message: signal.detail,
            hint: Some(guidance.to_string()),
        },
        PermissionState::Unknown => CheckReport {
            id,
            label,
            status: CheckStatus::Warn,
            blocking,
            message: signal.detail,
            hint: Some(guidance.to_string()),
        },
    }
}

fn probe_accessibility() -> PermissionSignal {
    let output = run_osascript(ACCESSIBILITY_SCRIPT);
    if output.success {
        let value = output.stdout.trim().to_ascii_lowercase();
        return match value.as_str() {
            "true" => PermissionSignal::ready("System Events reports UI scripting is enabled."),
            "false" => PermissionSignal::blocked("System Events reports UI scripting is disabled."),
            _ => PermissionSignal::unknown(format!(
                "Accessibility probe returned unexpected value: {}",
                sanitize_probe_detail(&output.stdout)
            )),
        };
    }

    let normalized = output.normalized_detail();
    if looks_like_accessibility_blocked(&normalized) {
        PermissionSignal::blocked("Accessibility access is blocked for this terminal host.")
    } else if looks_like_automation_blocked(&normalized) {
        PermissionSignal::unknown(
            "Could not confirm Accessibility because Automation access to System Events is blocked.",
        )
    } else {
        PermissionSignal::unknown(format!(
            "Accessibility probe failed: {}",
            sanitize_probe_detail(&normalized)
        ))
    }
}

fn probe_automation() -> PermissionSignal {
    let output = run_osascript(AUTOMATION_SCRIPT);
    if output.success {
        return PermissionSignal::ready("Apple Events access to System Events is allowed.");
    }

    let normalized = output.normalized_detail();
    if looks_like_automation_blocked(&normalized) {
        PermissionSignal::blocked("Apple Events access to System Events is blocked.")
    } else {
        PermissionSignal::unknown(format!(
            "Automation probe failed: {}",
            sanitize_probe_detail(&normalized)
        ))
    }
}

fn probe_activate() -> CheckReport {
    let output = run_osascript(AUTOMATION_SCRIPT);
    if output.success {
        return CheckReport {
            id: "probe_activate",
            label: "Probe: window activate",
            status: CheckStatus::Ok,
            blocking: false,
            message: "Activation probe succeeded.".to_string(),
            hint: None,
        };
    }

    let detail = sanitize_probe_detail(&output.normalized_detail());
    CheckReport {
        id: "probe_activate",
        label: "Probe: window activate",
        status: CheckStatus::Warn,
        blocking: false,
        message: format!("Activation probe failed: {detail}"),
        hint: Some(AUTOMATION_HINT.to_string()),
    }
}

fn probe_input_hotkey() -> CheckReport {
    // Use a non-printing modifier key so probe execution does not leak visible
    // escape/control glyphs into an interactive terminal session.
    let script = r#"tell application "System Events" to key code 56"#;
    let output = run_osascript(script);
    if output.success {
        return CheckReport {
            id: "probe_input_hotkey",
            label: "Probe: input hotkey",
            status: CheckStatus::Ok,
            blocking: false,
            message: "Input hotkey probe succeeded.".to_string(),
            hint: None,
        };
    }

    let detail = sanitize_probe_detail(&output.normalized_detail());
    CheckReport {
        id: "probe_input_hotkey",
        label: "Probe: input hotkey",
        status: CheckStatus::Warn,
        blocking: false,
        message: format!("Input probe failed: {detail}"),
        hint: Some(ACCESSIBILITY_HINT.to_string()),
    }
}

fn probe_screenshot() -> CheckReport {
    if shared_env::env_truthy("AGENTS_MACOS_AGENT_TEST_MODE") {
        return CheckReport {
            id: "probe_screenshot",
            label: "Probe: observe screenshot",
            status: CheckStatus::Ok,
            blocking: false,
            message: "Screenshot probe succeeded in deterministic test mode.".to_string(),
            hint: None,
        };
    }

    #[cfg(target_os = "macos")]
    let shareable =
        screen_record::macos::shareable::fetch_shareable().map_err(|err| err.to_string());

    #[cfg(not(target_os = "macos"))]
    let shareable: Result<screen_record::types::ShareableContent, String> =
        Err("macOS shareable probe is unavailable on this platform".to_string());

    match shareable {
        Ok(content) => {
            if content.windows.is_empty() {
                CheckReport {
                    id: "probe_screenshot",
                    label: "Probe: observe screenshot",
                    status: CheckStatus::Warn,
                    blocking: false,
                    message: "Screenshot probe found no shareable windows.".to_string(),
                    hint: Some(SCREEN_RECORDING_HINT.to_string()),
                }
            } else {
                CheckReport {
                    id: "probe_screenshot",
                    label: "Probe: observe screenshot",
                    status: CheckStatus::Ok,
                    blocking: false,
                    message: "Screenshot probe validated shareable content access.".to_string(),
                    hint: None,
                }
            }
        }
        Err(err) => CheckReport {
            id: "probe_screenshot",
            label: "Probe: observe screenshot",
            status: CheckStatus::Warn,
            blocking: false,
            message: format!("Screenshot probe failed: {err}"),
            hint: Some(SCREEN_RECORDING_HINT.to_string()),
        },
    }
}

fn find_in_path(bin: &str) -> Option<String> {
    shared_process::find_in_path(bin).map(|path| path.display().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OsaScriptOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

impl OsaScriptOutput {
    fn normalized_detail(&self) -> String {
        let merged = format!("{} {}", self.stdout, self.stderr);
        sanitize_probe_detail(&merged).to_ascii_lowercase()
    }
}

fn run_osascript(script: &str) -> OsaScriptOutput {
    match Command::new("osascript").args(["-e", script]).output() {
        Ok(output) => OsaScriptOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        Err(err) => OsaScriptOutput {
            success: false,
            stdout: String::new(),
            stderr: err.to_string(),
        },
    }
}

fn sanitize_probe_detail(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "no detail available".to_string()
    } else {
        collapsed
    }
}

fn looks_like_automation_blocked(message: &str) -> bool {
    message.contains("-1743") || message.contains("not authorized to send apple events")
}

fn looks_like_accessibility_blocked(message: &str) -> bool {
    message.contains("-25211")
        || message.contains("assistive access")
        || message.contains("ui scripting")
            && (message.contains("not allowed") || message.contains("permission"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use nils_test_support::{EnvGuard, GlobalStateLock, StubBinDir, prepend_path};

    use super::{
        ACCESSIBILITY_HINT, CheckStatus, PermissionState, collect_system_snapshot, find_in_path,
        looks_like_accessibility_blocked, looks_like_automation_blocked, probe_accessibility,
        probe_automation, probe_input_hotkey, run_osascript, sanitize_probe_detail,
    };

    fn install_stub_tools(
        lock: &GlobalStateLock,
        include_cliclick: bool,
    ) -> (StubBinDir, EnvGuard) {
        let stub_dir = StubBinDir::new();
        stub_dir.write_exe(
            "osascript",
            r#"#!/usr/bin/env bash
set -euo pipefail
script="${*: -1}"
if [[ "$script" == *"UI elements enabled"* ]]; then
  mode="${MACOS_AGENT_TEST_ACCESSIBILITY_MODE:-true}"
  case "$mode" in
    true|false)
      echo "$mode"
      exit 0
      ;;
    block)
      echo "Assistive access not allowed (-25211)" >&2
      exit 1
      ;;
    automation_block)
      echo "Not authorized to send apple events to System Events. (-1743)" >&2
      exit 1
      ;;
    other_error)
      echo "unexpected accessibility failure" >&2
      exit 1
      ;;
    *)
      echo "$mode"
      exit 0
      ;;
  esac
fi
if [[ "$script" == *"frontmost is true"* ]]; then
  mode="${MACOS_AGENT_TEST_AUTOMATION_MODE:-ok}"
  case "$mode" in
    ok)
      echo "Terminal"
      exit 0
      ;;
    block)
      echo "Not authorized to send apple events to System Events. (-1743)" >&2
      exit 1
      ;;
    other_error)
      echo "automation probe exploded" >&2
      exit 1
      ;;
  esac
fi
if [[ "$script" == *"key code 56"* ]]; then
  mode="${MACOS_AGENT_TEST_HOTKEY_MODE:-ok}"
  case "$mode" in
    ok)
      exit 0
      ;;
    block)
      echo "Assistive access not allowed (-25211)" >&2
      exit 1
      ;;
    other_error)
      echo "hotkey probe exploded" >&2
      exit 1
      ;;
  esac
fi
echo "unsupported script" >&2
exit 1
"#,
        );

        if include_cliclick {
            stub_dir.write_exe("cliclick", "#!/usr/bin/env bash\nexit 0\n");
        }

        let path_guard = prepend_path(lock, stub_dir.path());
        (stub_dir, path_guard)
    }

    #[test]
    fn collect_snapshot_uses_stubbed_tools() {
        let lock = GlobalStateLock::new();
        let (_stubs, _path) = install_stub_tools(&lock, true);
        let _a11y = EnvGuard::set(&lock, "MACOS_AGENT_TEST_ACCESSIBILITY_MODE", "true");
        let _automation = EnvGuard::set(&lock, "MACOS_AGENT_TEST_AUTOMATION_MODE", "ok");

        let snapshot = collect_system_snapshot();
        assert!(snapshot.osascript_path.is_some());
        assert!(snapshot.cliclick_path.is_some());
        assert_eq!(snapshot.accessibility_signal.state, PermissionState::Ready);
        assert_eq!(snapshot.automation_signal.state, PermissionState::Ready);
    }

    #[test]
    fn collect_snapshot_without_osascript_marks_permission_unknown() {
        let lock = GlobalStateLock::new();
        let empty = StubBinDir::new();
        let _path = EnvGuard::set(&lock, "PATH", &empty.path_str());

        let snapshot = collect_system_snapshot();
        assert!(snapshot.osascript_path.is_none());
        assert_eq!(
            snapshot.accessibility_signal.state,
            PermissionState::Unknown
        );
        assert_eq!(snapshot.automation_signal.state, PermissionState::Unknown);
    }

    #[test]
    fn probe_accessibility_covers_success_and_error_modes() {
        let lock = GlobalStateLock::new();
        let (_stubs, _path) = install_stub_tools(&lock, false);

        let _mode_false = EnvGuard::set(&lock, "MACOS_AGENT_TEST_ACCESSIBILITY_MODE", "false");
        let blocked = probe_accessibility();
        assert_eq!(blocked.state, PermissionState::Blocked);

        let _mode_weird = EnvGuard::set(
            &lock,
            "MACOS_AGENT_TEST_ACCESSIBILITY_MODE",
            "unexpected-value",
        );
        let unknown = probe_accessibility();
        assert_eq!(unknown.state, PermissionState::Unknown);

        let _mode_block = EnvGuard::set(&lock, "MACOS_AGENT_TEST_ACCESSIBILITY_MODE", "block");
        let blocked = probe_accessibility();
        assert_eq!(blocked.state, PermissionState::Blocked);

        let _mode_auto_block = EnvGuard::set(
            &lock,
            "MACOS_AGENT_TEST_ACCESSIBILITY_MODE",
            "automation_block",
        );
        let unknown = probe_accessibility();
        assert_eq!(unknown.state, PermissionState::Unknown);

        let _mode_other =
            EnvGuard::set(&lock, "MACOS_AGENT_TEST_ACCESSIBILITY_MODE", "other_error");
        let unknown = probe_accessibility();
        assert_eq!(unknown.state, PermissionState::Unknown);
    }

    #[test]
    fn probe_automation_covers_blocked_and_unknown() {
        let lock = GlobalStateLock::new();
        let (_stubs, _path) = install_stub_tools(&lock, false);

        let _mode_ok = EnvGuard::set(&lock, "MACOS_AGENT_TEST_AUTOMATION_MODE", "ok");
        let ready = probe_automation();
        assert_eq!(ready.state, PermissionState::Ready);

        let _mode_block = EnvGuard::set(&lock, "MACOS_AGENT_TEST_AUTOMATION_MODE", "block");
        let blocked = probe_automation();
        assert_eq!(blocked.state, PermissionState::Blocked);

        let _mode_other = EnvGuard::set(&lock, "MACOS_AGENT_TEST_AUTOMATION_MODE", "other_error");
        let unknown = probe_automation();
        assert_eq!(unknown.state, PermissionState::Unknown);
    }

    #[test]
    fn probe_input_hotkey_uses_non_esc_key_and_maps_failures() {
        let lock = GlobalStateLock::new();
        let (_stubs, _path) = install_stub_tools(&lock, false);

        let _mode_ok = EnvGuard::set(&lock, "MACOS_AGENT_TEST_HOTKEY_MODE", "ok");
        let ready = probe_input_hotkey();
        assert_eq!(ready.status, CheckStatus::Ok);

        let _mode_block = EnvGuard::set(&lock, "MACOS_AGENT_TEST_HOTKEY_MODE", "block");
        let blocked = probe_input_hotkey();
        assert_eq!(blocked.status, CheckStatus::Warn);
        assert_eq!(blocked.hint.as_deref(), Some(ACCESSIBILITY_HINT));
    }

    #[test]
    fn helpers_cover_path_detection_and_sanitization() {
        let lock = GlobalStateLock::new();
        let (stubs, _path) = install_stub_tools(&lock, false);

        let osascript_path = stubs.path().join("osascript");
        let detected = find_in_path(osascript_path.to_str().unwrap()).expect("explicit path");
        assert_eq!(PathBuf::from(detected), osascript_path);
        assert!(find_in_path(stubs.path().join("missing").to_str().unwrap()).is_none());

        assert_eq!(sanitize_probe_detail(" a \n b \t c "), "a b c");
        assert_eq!(sanitize_probe_detail(""), "no detail available");

        assert!(looks_like_automation_blocked("-1743"));
        assert!(looks_like_accessibility_blocked(
            "assistive access not allowed"
        ));
    }

    #[test]
    fn run_osascript_reports_spawn_failures() {
        let lock = GlobalStateLock::new();
        let empty = StubBinDir::new();
        let _path = EnvGuard::set(&lock, "PATH", &empty.path_str());

        let output = run_osascript("return 1");
        assert!(!output.success);
        assert!(!output.stderr.is_empty());
    }
}
