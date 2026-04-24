use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct UiPoint {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArcProfile {
    pub youtube_home_url: String,
    pub video_tiles: Vec<UiPoint>,
    pub player_focus: UiPoint,
    pub comment_checkpoint: UiPoint,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpotifyProfile {
    pub search_box: UiPoint,
    pub track_rows: Vec<UiPoint>,
    pub play_toggle: UiPoint,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FinderProfile {
    pub window_focus: UiPoint,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RealE2eProfile {
    pub profile_name: String,
    pub arc: ArcProfile,
    pub spotify: SpotifyProfile,
    pub finder: FinderProfile,
}

#[derive(Debug, Clone)]
pub struct SpotifyPlaybackState {
    pub player_state: String,
    pub track_name: String,
    pub artist: String,
}

pub struct ArcCleanupGuard;

impl ArcCleanupGuard {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ArcCleanupGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ArcCleanupGuard {
    fn drop(&mut self) {
        best_effort_arc_reset_to_blank();
    }
}

pub struct SpotifyPlaybackCleanupGuard {
    initial_was_playing: Option<bool>,
}

impl SpotifyPlaybackCleanupGuard {
    pub fn capture() -> Self {
        Self {
            initial_was_playing: try_spotify_is_playing(),
        }
    }
}

impl Drop for SpotifyPlaybackCleanupGuard {
    fn drop(&mut self) {
        let Some(initial_was_playing) = self.initial_was_playing else {
            return;
        };
        let Some(currently_playing) = try_spotify_is_playing() else {
            return;
        };

        if initial_was_playing != currently_playing {
            let _ = try_spotify_playpause();
        }
    }
}

pub struct FinderWindowCleanupGuard {
    armed: bool,
}

impl FinderWindowCleanupGuard {
    pub fn new() -> Self {
        Self { armed: false }
    }

    pub fn arm(&mut self) {
        self.armed = true;
    }
}

impl Default for FinderWindowCleanupGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for FinderWindowCleanupGuard {
    fn drop(&mut self) {
        if self.armed {
            best_effort_close_active_finder_window();
        }
    }
}

static ARTIFACT_COUNTER: AtomicU64 = AtomicU64::new(1);
static INPUT_SOURCE_SETUP: OnceLock<Result<(), String>> = OnceLock::new();
static STEP_LEDGER_ACTIVE: AtomicBool = AtomicBool::new(false);
const DEFAULT_STEP_TIMEOUT_MS: u64 = 15_000;
const STEP_TIMEOUT_MIN_MS: u64 = 1_000;
const STEP_TIMEOUT_KILL_GRACE_MS: u64 = 2_000;
const STEP_LOG_EXCERPT_MAX: usize = 500;
const SUPPORTED_REAL_APPS: [&str; 3] = ["arc", "spotify", "finder"];
const ABC_INPUT_SOURCE_IDS: [&str; 4] = [
    "com.apple.keylayout.abc",
    "com.apple.keylayout.us",
    "abc",
    "us",
];

#[derive(Debug, Clone, Serialize)]
struct StepLogEntry {
    step_id: String,
    scenario_id: String,
    step: String,
    args: Vec<String>,
    attempt: usize,
    elapsed_ms: u64,
    success: bool,
    exit_code: i32,
    stdout_excerpt: String,
    stderr_excerpt: String,
}

#[derive(Debug, Clone, Serialize, Default)]
struct StepLedgerSummary {
    scenario_id: String,
    steps_path: String,
    total_steps: usize,
    last_successful_step_id: Option<String>,
    failing_step_id: Option<String>,
}

#[derive(Debug, Clone)]
struct StepLedgerState {
    scenario_id: String,
    steps_path: PathBuf,
    summary_path: PathBuf,
    counter: u64,
    total_steps: usize,
    last_successful_step_id: Option<String>,
    failing_step_id: Option<String>,
}

thread_local! {
    static STEP_LEDGER_STATE: std::cell::RefCell<Option<StepLedgerState>> = const { std::cell::RefCell::new(None) };
}

pub struct StepLedgerGuard;

impl Drop for StepLedgerGuard {
    fn drop(&mut self) {
        STEP_LEDGER_STATE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if let Some(state) = slot.take() {
                let summary = StepLedgerSummary {
                    scenario_id: state.scenario_id,
                    steps_path: state.steps_path.display().to_string(),
                    total_steps: state.total_steps,
                    last_successful_step_id: state.last_successful_step_id,
                    failing_step_id: state.failing_step_id,
                };
                let _ = write_json_to_path(&state.summary_path, &serde_json::json!(summary));
            }
        });
        STEP_LEDGER_ACTIVE.store(false, Ordering::SeqCst);
    }
}

pub fn begin_step_ledger(artifact_dir: &Path, scenario_id: &str) -> StepLedgerGuard {
    let steps_path = artifact_dir.join("steps.jsonl");
    let summary_path = artifact_dir.join("step-summary.json");
    std::fs::create_dir_all(artifact_dir).expect("create artifact dir for step ledger");
    if !steps_path.exists() {
        std::fs::write(&steps_path, b"").expect("initialize step ledger");
    }
    let existing_steps = std::fs::read_to_string(&steps_path)
        .ok()
        .map(|raw| raw.lines().filter(|line| !line.trim().is_empty()).count() as u64)
        .unwrap_or(0);

    STEP_LEDGER_STATE.with(|slot| {
        *slot.borrow_mut() = Some(StepLedgerState {
            scenario_id: scenario_id.to_string(),
            steps_path,
            summary_path,
            counter: existing_steps,
            total_steps: existing_steps as usize,
            last_successful_step_id: None,
            failing_step_id: None,
        });
    });
    STEP_LEDGER_ACTIVE.store(true, Ordering::SeqCst);
    StepLedgerGuard
}

pub fn step_ledger_path_for(artifact_dir: &Path) -> String {
    artifact_dir.join("steps.jsonl").display().to_string()
}

pub fn app_gate_reason(app: &str, requires_mutation: bool) -> Option<String> {
    validate_selected_apps_env().unwrap_or_else(|message| panic!("{message}"));
    if !real_e2e_enabled() {
        return Some("MACOS_AGENT_REAL_E2E is not enabled".to_string());
    }
    if requires_mutation && !real_mutation_enabled() {
        return Some("MACOS_AGENT_REAL_E2E_MUTATING is not enabled".to_string());
    }
    if !app_selected(app) {
        return Some(format!(
            "app `{app}` is not selected by MACOS_AGENT_REAL_E2E_APPS"
        ));
    }
    None
}

pub fn validate_selected_apps_env() -> Result<(), String> {
    let Ok(raw) = env::var("MACOS_AGENT_REAL_E2E_APPS") else {
        return Ok(());
    };
    validate_selected_apps_raw(&raw)
}

pub fn validate_selected_apps_raw(raw: &str) -> Result<(), String> {
    if raw.trim().is_empty() {
        return Ok(());
    }
    let tokens = raw
        .split(',')
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let unknown = tokens
        .iter()
        .filter(|token| !SUPPORTED_REAL_APPS.contains(&token.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(format!(
        "unsupported MACOS_AGENT_REAL_E2E_APPS entries: {}; supported values: {}",
        unknown.join(","),
        SUPPORTED_REAL_APPS.join(",")
    ))
}

pub fn real_e2e_enabled() -> bool {
    cfg!(target_os = "macos")
        && env::var("MACOS_AGENT_REAL_E2E")
            .ok()
            .map(|value| value == "1")
            .unwrap_or(false)
}

pub fn real_mutation_enabled() -> bool {
    env::var("MACOS_AGENT_REAL_E2E_MUTATING")
        .ok()
        .map(|value| value == "1")
        .unwrap_or(false)
}

pub fn app_selected(app: &str) -> bool {
    validate_selected_apps_env().unwrap_or_else(|message| panic!("{message}"));
    let Ok(raw) = env::var("MACOS_AGENT_REAL_E2E_APPS") else {
        return true;
    };
    if raw.trim().is_empty() {
        return true;
    }

    let app = app.to_ascii_lowercase();
    raw.split(',')
        .map(|item| item.trim().to_ascii_lowercase())
        .any(|item| !item.is_empty() && item == app)
}

pub fn base_options(cwd: &Path) -> CmdOptions {
    CmdOptions::new()
        .with_cwd(cwd)
        .with_env_remove("AGENTS_MACOS_AGENT_TEST_MODE")
        .with_env_remove("AGENTS_MACOS_AGENT_TEST_TIMESTAMP")
        .with_env_remove("AGENTS_MACOS_AGENT_STUB_CLICLICK_MODE")
        .with_env_remove("AGENTS_MACOS_AGENT_STUB_OSASCRIPT_MODE")
}

pub fn create_artifact_dir(prefix: &str) -> PathBuf {
    let base = agents_out_dir().join("macos-agent-e2e");
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let counter = ARTIFACT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = base.join(format!(
        "{prefix}-{timestamp_ms}-{}-{counter}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create real e2e artifact directory");
    dir
}

pub fn selected_profile_name() -> String {
    env::var("MACOS_AGENT_REAL_E2E_PROFILE").unwrap_or_else(|_| "default-1440p".to_string())
}

pub fn load_profile() -> RealE2eProfile {
    let profile = selected_profile_name();
    let fixture = format!(
        "real_e2e_profile_{}.json",
        profile.replace('-', "_").to_ascii_lowercase()
    );
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(fixture);

    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read profile at {}: {err}", path.display()));
    serde_json::from_str::<RealE2eProfile>(&raw).unwrap_or_else(|err| {
        panic!(
            "failed to parse profile `{profile}` from {}: {err}",
            path.display()
        )
    })
}

pub fn run_json_step(
    bin: &Path,
    options: &CmdOptions,
    args: &[&str],
    step: &str,
) -> serde_json::Value {
    let started = Instant::now();
    let out = run_with_timeout(bin, args, options, step_timeout());
    record_step_entry(step, args, 1, started.elapsed(), &out);
    if out.code != 0 {
        best_effort_failure_checkpoint(bin, options, step);
    }
    assert_eq!(
        out.code,
        0,
        "step `{step}` failed\nstdout:\n{}\nstderr:\n{}",
        out.stdout_text(),
        out.stderr_text()
    );
    serde_json::from_str(&out.stdout_text())
        .unwrap_or_else(|err| panic!("step `{step}` did not emit valid json: {err}"))
}

pub fn try_run_json_step(
    bin: &Path,
    options: &CmdOptions,
    args: &[&str],
    step: &str,
) -> Result<serde_json::Value, String> {
    let started = Instant::now();
    let out = run_with_timeout(bin, args, options, step_timeout());
    record_step_entry(step, args, 1, started.elapsed(), &out);
    if out.code != 0 {
        best_effort_failure_checkpoint(bin, options, step);
        return Err(format!(
            "step `{step}` failed\nstdout:\n{}\nstderr:\n{}",
            out.stdout_text(),
            out.stderr_text()
        ));
    }
    serde_json::from_str(&out.stdout_text())
        .map_err(|err| format!("step `{step}` did not emit valid json: {err}"))
}

pub fn run_json_step_with_retry(
    bin: &Path,
    options: &CmdOptions,
    args: &[&str],
    step: &str,
    retries: usize,
    retry_delay: Duration,
) -> serde_json::Value {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let started = Instant::now();
        let out = run_with_timeout(bin, args, options, step_timeout());
        record_step_entry(step, args, attempt, started.elapsed(), &out);
        if out.code == 0 {
            return serde_json::from_str(&out.stdout_text()).unwrap_or_else(|err| {
                panic!("step `{step}` attempt {attempt} emitted invalid json: {err}")
            });
        }

        if attempt > retries {
            best_effort_failure_checkpoint(bin, options, step);
            panic!(
                "step `{step}` failed after {attempt} attempts\nstdout:\n{}\nstderr:\n{}",
                out.stdout_text(),
                out.stderr_text()
            );
        }
        std::thread::sleep(retry_delay);
    }
}

pub fn require_preflight_ready(payload: &serde_json::Value, ids: &[&str]) {
    let checks = payload["result"]["checks"]
        .as_array()
        .expect("preflight result.checks should be an array");
    for id in ids {
        let check = checks
            .iter()
            .find(|check| check["id"] == serde_json::json!(*id))
            .unwrap_or_else(|| panic!("missing preflight check `{id}`"));
        let status = check["status"]
            .as_str()
            .unwrap_or("<non-string-status>")
            .to_string();
        if status != "ok" {
            let message = check["message"].as_str().unwrap_or("");
            let hint = check["hint"].as_str().unwrap_or("");
            panic!("preflight `{id}` not ready (status={status}): {message}; hint: {hint}");
        }
    }
}

pub fn require_app_installed(app: &str) {
    let script = format!("id of application \"{}\"", escape_applescript(app));
    let out = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .unwrap_or_else(|err| panic!("failed to execute osascript for app `{app}`: {err}"));
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        panic!(
            "required app `{app}` is not installed/available: {}",
            stderr.trim()
        );
    }
}

pub fn launch_app(app: &str) {
    let script = format!(
        r#"tell application "{}" to launch"#,
        escape_applescript(app)
    );
    let out = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .unwrap_or_else(|err| panic!("failed to launch app `{app}` via osascript: {err}"));
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        panic!("failed to launch app `{app}`: {}", stderr.trim());
    }
}

pub fn activate_app_with_retry(
    bin: &Path,
    options: &CmdOptions,
    app: &str,
    wait_ms: u64,
    action_timeout_ms: u64,
    retries: usize,
    retry_delay: Duration,
) -> serde_json::Value {
    launch_app(app);
    ensure_app_process_running(app, 10_000, 120);
    let wait_ms_text = wait_ms.to_string();
    let timeout_ms_text = action_timeout_ms.to_string();
    let step = format!("window.activate {app}");
    run_json_step_with_retry(
        bin,
        options,
        &[
            "--format",
            "json",
            "--timeout-ms",
            &timeout_ms_text,
            "window",
            "activate",
            "--app",
            app,
            "--wait-ms",
            &wait_ms_text,
            "--reopen-on-fail",
        ],
        &step,
        retries,
        retry_delay,
    )
}

pub fn wait_app_active(
    bin: &Path,
    options: &CmdOptions,
    app: &str,
    timeout_ms: u64,
    poll_ms: u64,
    step: &str,
) -> serde_json::Value {
    let timeout_text = timeout_ms.to_string();
    let poll_text = poll_ms.to_string();
    run_json_step(
        bin,
        options,
        &[
            "--format",
            "json",
            "wait",
            "app-active",
            "--app",
            app,
            "--timeout-ms",
            &timeout_text,
            "--poll-ms",
            &poll_text,
        ],
        step,
    )
}

pub fn ensure_input_source_is_abc(step: &str) {
    let current = run_macos_agent_input_source_current().unwrap_or_else(|err| {
        panic!("step `{step}` failed to query current input source: {err}");
    });
    if !is_abc_input_source(&current) {
        panic!(
            "step `{step}` expected ABC input source (e.g. `com.apple.keylayout.ABC`) but got `{current}`"
        );
    }
}

pub fn ensure_app_active_or_frontmost(
    bin: &Path,
    options: &CmdOptions,
    app: &str,
    timeout_ms: u64,
    poll_ms: u64,
    step: &str,
) {
    let timeout_text = timeout_ms.to_string();
    let poll_text = poll_ms.to_string();
    let wait_error = match try_run_json_step(
        bin,
        options,
        &[
            "--format",
            "json",
            "wait",
            "app-active",
            "--app",
            app,
            "--timeout-ms",
            &timeout_text,
            "--poll-ms",
            &poll_text,
        ],
        &format!("{step} (wait.app-active)"),
    ) {
        Ok(payload) => {
            assert_eq!(payload["command"], serde_json::json!("wait.app-active"));
            None
        }
        Err(err) => Some(err),
    };

    if wait_for_frontmost_app(app, timeout_ms, poll_ms) {
        if let Some(err) = wait_error.as_ref() {
            eprintln!(
                "WARN[{step}]: wait.app-active failed but frontmost app is `{app}`; continuing.\n{err}"
            );
        }
        return;
    }

    let observed_before_recovery = frontmost_app_name().unwrap_or_else(|| "<unknown>".to_string());
    eprintln!(
        "WARN[{step}]: `{app}` is not frontmost (observed `{observed_before_recovery}`); attempting recovery via window.activate"
    );
    let recover_error = try_activate_frontmost_for_recovery(bin, options, app, step).err();
    if wait_for_frontmost_app(app, 3_000, poll_ms) {
        if let Some(err) = wait_error.as_ref() {
            eprintln!(
                "WARN[{step}]: recovered frontmost `{app}` after window.activate; initial wait.app-active error:\n{err}"
            );
        }
        return;
    }

    let observed_after_recovery = frontmost_app_name().unwrap_or_else(|| "<unknown>".to_string());
    if wait_error.is_none() && recover_error.is_none() {
        panic!(
            "step `{step}` expected frontmost app `{app}` within {timeout_ms}ms; observed `{observed_after_recovery}`"
        );
    }

    let mut details = Vec::new();
    if let Some(err) = wait_error {
        details.push(format!("wait.app-active error:\n{err}"));
    }
    if let Some(err) = recover_error {
        details.push(format!("window.activate recovery error:\n{err}"));
    }
    panic!(
        "step `{step}` expected frontmost app `{app}` within {timeout_ms}ms; observed `{observed_after_recovery}`;\n{}",
        details.join("\n")
    );
}

pub fn ensure_app_ready_for_keyboard_input(
    bin: &Path,
    options: &CmdOptions,
    app: &str,
    timeout_ms: u64,
    poll_ms: u64,
    step: &str,
) {
    ensure_input_source_for_text_entry();
    let input_step = format!("{step} (input-source=ABC)");
    ensure_input_source_is_abc(&input_step);
    let app_step = format!("{step} (frontmost={app})");
    ensure_app_active_or_frontmost(bin, options, app, timeout_ms, poll_ms, &app_step);
}

pub fn fail_step_with_checkpoint(bin: &Path, options: &CmdOptions, step: &str, message: &str) -> ! {
    let out = CmdOutput {
        code: 1,
        stdout: Vec::new(),
        stderr: message.as_bytes().to_vec(),
    };
    record_step_entry(step, &[], 1, Duration::from_millis(0), &out);
    best_effort_failure_checkpoint(bin, options, step);
    panic!("{message}");
}

pub fn current_step_ledger_snapshot() -> (Option<String>, Option<String>, Option<String>) {
    STEP_LEDGER_STATE.with(|slot| {
        let slot = slot.borrow();
        let Some(state) = slot.as_ref() else {
            return (None, None, None);
        };
        (
            Some(state.steps_path.display().to_string()),
            state.failing_step_id.clone(),
            state.last_successful_step_id.clone(),
        )
    })
}

fn ensure_app_process_running(app: &str, timeout_ms: u64, poll_ms: u64) {
    let started = Instant::now();
    while started.elapsed().as_millis() < timeout_ms as u128 {
        match app_process_running(app) {
            Ok(true) => return,
            Ok(false) => {}
            Err(err) => panic!("failed probing running process for `{app}`: {err}"),
        }
        std::thread::sleep(Duration::from_millis(poll_ms));
    }
    panic!("app `{app}` did not appear as a running process within {timeout_ms}ms after launch");
}

fn app_process_running(app: &str) -> Result<bool, String> {
    let script = format!(
        r#"tell application "System Events" to return exists process "{}""#,
        escape_applescript(app)
    );
    let out = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map_err(|err| format!("run osascript for process check failed: {err}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(stderr.trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().eq("true"))
}

fn wait_for_frontmost_app(app: &str, timeout_ms: u64, poll_ms: u64) -> bool {
    let started = Instant::now();
    let delay = Duration::from_millis(poll_ms.max(40));
    while started.elapsed().as_millis() < timeout_ms as u128 {
        if frontmost_app_is(app) {
            return true;
        }
        std::thread::sleep(delay);
    }
    frontmost_app_is(app)
}

fn try_activate_frontmost_for_recovery(
    bin: &Path,
    options: &CmdOptions,
    app: &str,
    step: &str,
) -> Result<(), String> {
    let payload = try_run_json_step(
        bin,
        options,
        &[
            "--format",
            "json",
            "--timeout-ms",
            "6000",
            "window",
            "activate",
            "--app",
            app,
            "--wait-ms",
            "1200",
            "--reopen-on-fail",
        ],
        &format!("{step} (window.activate recovery)"),
    )?;
    if payload["command"] != serde_json::json!("window.activate") {
        return Err(format!(
            "unexpected command field for window.activate recovery: {}",
            payload["command"]
        ));
    }
    Ok(())
}

pub fn spotify_playback_state() -> SpotifyPlaybackState {
    let script = r#"tell application "Spotify"
set stateText to (player state as text)
set trackName to ""
set artistName to ""
if player state is not stopped then
  set trackName to name of current track
  set artistName to artist of current track
end if
return stateText & "|" & trackName & "|" & artistName
end tell"#;
    let out = Command::new("osascript")
        .args(["-e", script])
        .output()
        .unwrap_or_else(|err| panic!("failed to execute spotify state probe: {err}"));
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        panic!(
            "spotify state probe failed (check Automation permissions): {}",
            stderr.trim()
        );
    }

    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mut parts = stdout.splitn(3, '|');
    let state = parts.next().unwrap_or("").trim().to_string();
    let track = parts.next().unwrap_or("").trim().to_string();
    let artist = parts.next().unwrap_or("").trim().to_string();

    SpotifyPlaybackState {
        player_state: state,
        track_name: track,
        artist,
    }
}

pub fn spotify_toggle_play_pause() {
    let script = r#"tell application "Spotify" to playpause"#;
    let out = Command::new("osascript")
        .args(["-e", script])
        .output()
        .unwrap_or_else(|err| panic!("failed to execute spotify playpause probe: {err}"));
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        panic!(
            "spotify playpause probe failed (check Automation permissions): {}",
            stderr.trim()
        );
    }
}

pub fn ensure_input_source_for_text_entry() {
    let result = INPUT_SOURCE_SETUP.get_or_init(configure_input_source_once);
    if let Err(message) = result {
        panic!("{message}");
    }
}

pub fn send_hotkey(bin: &Path, options: &CmdOptions, mods: Option<&str>, key: &str, step: &str) {
    ensure_input_source_for_text_entry();
    if let Some(mods) = mods {
        let mut args = vec!["--format", "json", "input", "hotkey"];
        args.extend(["--mods", mods, "--key", key]);
        let payload = run_json_step(bin, options, &args, step);
        assert_eq!(payload["command"], serde_json::json!("input.hotkey"));
        return;
    }

    send_unmodified_key(key, step);
}

pub fn replace_focused_text_with_clipboard(
    bin: &Path,
    options: &CmdOptions,
    text: &str,
    step_prefix: &str,
) {
    ensure_input_source_for_text_entry();
    send_hotkey(
        bin,
        options,
        Some("cmd"),
        "a",
        &format!("{step_prefix} select-all"),
    );
    set_clipboard_text(text);
    send_hotkey(
        bin,
        options,
        Some("cmd"),
        "v",
        &format!("{step_prefix} paste"),
    );
}

pub fn read_clipboard_text() -> String {
    let script = "the clipboard as text";
    let out = Command::new("osascript")
        .args(["-e", script])
        .output()
        .unwrap_or_else(|err| panic!("failed to read clipboard text: {err}"));
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        panic!("failed to read clipboard text: {}", stderr.trim());
    }
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn record_step_entry(
    step: &str,
    args: &[&str],
    attempt: usize,
    elapsed: Duration,
    out: &CmdOutput,
) {
    if !STEP_LEDGER_ACTIVE.load(Ordering::SeqCst) {
        return;
    }

    STEP_LEDGER_STATE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return;
        };
        state.counter = state.counter.saturating_add(1);
        state.total_steps = state.total_steps.saturating_add(1);
        let step_id = format!("{}-{}", state.scenario_id, state.counter);
        let success = out.code == 0;
        if success {
            state.last_successful_step_id = Some(step_id.clone());
        } else if state.failing_step_id.is_none() {
            state.failing_step_id = Some(step_id.clone());
        }

        let entry = StepLogEntry {
            step_id,
            scenario_id: state.scenario_id.clone(),
            step: step.to_string(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            attempt,
            elapsed_ms: elapsed.as_millis() as u64,
            success,
            exit_code: out.code,
            stdout_excerpt: compact_log_excerpt(&out.stdout_text()),
            stderr_excerpt: compact_log_excerpt(&out.stderr_text()),
        };

        if let Ok(line) = serde_json::to_string(&entry)
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&state.steps_path)
        {
            let _ = writeln!(file, "{line}");
        }
    });
}

fn compact_log_excerpt(raw: &str) -> String {
    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= STEP_LOG_EXCERPT_MAX {
        return compact;
    }
    let mut out = String::new();
    for (idx, ch) in compact.chars().enumerate() {
        if idx >= STEP_LOG_EXCERPT_MAX {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn best_effort_failure_checkpoint(bin: &Path, options: &CmdOptions, step: &str) {
    if !STEP_LEDGER_ACTIVE.load(Ordering::SeqCst) {
        return;
    }

    let capture = STEP_LEDGER_STATE.with(|slot| {
        let slot = slot.borrow();
        let state = slot.as_ref()?;
        let parent = state.steps_path.parent()?;
        Some(parent.join(format!(
            "failure-{}-{}.png",
            state.counter.saturating_add(1),
            sanitize_filename(step)
        )))
    });
    let Some(path) = capture else {
        return;
    };
    let path_text = path.to_string_lossy().to_string();
    let out = run_with_timeout(
        bin,
        &[
            "--format",
            "json",
            "observe",
            "screenshot",
            "--active-window",
            "--path",
            &path_text,
        ],
        options,
        Duration::from_millis(2_000),
    );
    if out.code == 0 {
        record_step_entry(
            "failure-checkpoint screenshot",
            &[
                "--format",
                "json",
                "observe",
                "screenshot",
                "--active-window",
                "--path",
                &path_text,
            ],
            1,
            Duration::from_millis(0),
            &out,
        );
    }
}

fn sanitize_filename(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

pub fn write_json(path: &Path, value: &serde_json::Value) {
    write_json_to_path(path, value)
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", path.display()));
}

fn write_json_to_path(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create {}: {err}", parent.display()))?;
    }
    let raw = serde_json::to_vec_pretty(value)
        .map_err(|err| format!("serialize {}: {err}", path.display()))?;
    std::fs::write(path, raw).map_err(|err| format!("write {}: {err}", path.display()))
}

fn agents_out_dir() -> PathBuf {
    if let Ok(agent_home) = env::var("AGENT_HOME") {
        return PathBuf::from(agent_home).join("out");
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(".agents").join("out");
    }
    PathBuf::from(".agents").join("out")
}

fn escape_applescript(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

fn configure_input_source_once() -> Result<(), String> {
    let configured = env::var("MACOS_AGENT_REAL_E2E_INPUT_SOURCE")
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|value| !value.is_empty());
    let token = configured.unwrap_or_else(|| "com.apple.keylayout.ABC".to_string());
    let target = normalize_input_source_token(&token);
    match run_macos_agent_input_source_switch(&target) {
        Ok(()) => Ok(()),
        Err(err) => {
            if err.contains("missing dependency `im-select`") {
                return Err(
                    "real e2e keyboard setup requires `im-select`; install with `brew install im-select`"
                        .to_string(),
                );
            }
            Err(err)
        }
    }
}

fn normalize_input_source_token(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "abc" | "english" | "us" => "com.apple.keylayout.ABC".to_string(),
        _ => raw.to_string(),
    }
}

fn run_macos_agent_input_source_switch(target: &str) -> Result<(), String> {
    let bin = resolve("macos-agent");
    let out = Command::new(bin)
        .args(["--format", "json", "input-source", "switch", "--id", target])
        .output()
        .map_err(|err| format!("failed to execute macos-agent input-source switch: {err}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if stderr.is_empty() {
        return Err("input-source switch failed with empty stderr".to_string());
    }
    Err(stderr)
}

fn run_macos_agent_input_source_current() -> Result<String, String> {
    run_macos_agent_input_source_current_via_cli()
}

fn run_macos_agent_input_source_current_via_cli() -> Result<String, String> {
    let bin = resolve("macos-agent");
    let out = Command::new(bin)
        .args(["--format", "json", "input-source", "current"])
        .output()
        .map_err(|err| format!("failed to execute macos-agent input-source current: {err}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if stderr.is_empty() {
            return Err("input-source current failed with empty stderr".to_string());
        }
        return Err(stderr);
    }

    let payload: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|err| format!("input-source current emitted invalid JSON: {err}"))?;
    let current = payload
        .get("result")
        .and_then(|result| result.get("current"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            format!(
                "input-source current response missing result.current: {}",
                String::from_utf8_lossy(&out.stdout).trim()
            )
        })?;
    Ok(current.to_string())
}

fn is_abc_input_source(current: &str) -> bool {
    let normalized = current.trim().to_ascii_lowercase();
    ABC_INPUT_SOURCE_IDS.contains(&normalized.as_str())
}

fn set_clipboard_text(text: &str) {
    let script = format!("set the clipboard to \"{}\"", escape_applescript(text));
    let out = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .unwrap_or_else(|err| panic!("failed to set clipboard text: {err}"));
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        panic!("failed to set clipboard text: {}", stderr.trim());
    }
}

fn best_effort_arc_reset_to_blank() {
    if !try_activate_app("Arc") {
        return;
    }

    std::thread::sleep(Duration::from_millis(250));

    for _ in 0..3 {
        if !try_set_clipboard_text("about:blank") {
            return;
        }
        if !try_arc_replace_address_with_clipboard() {
            continue;
        }
        std::thread::sleep(Duration::from_millis(200));

        if let Some(current_url) = try_arc_copy_active_url() {
            let normalized = current_url.to_ascii_lowercase();
            if normalized.contains("about:blank")
                || (!normalized.contains("youtube.com")
                    && !normalized.contains("google.com/search"))
            {
                break;
            }
        }
    }

    // Guard escape dispatch so we do not leak control glyphs to terminal when Arc
    // activation loses focus unexpectedly in real E2E runs.
    if frontmost_app_is("Arc") {
        let _ = run_osascript_script(r#"tell application "System Events" to key code 53"#);
    }
}

fn best_effort_close_active_finder_window() {
    let _ = try_activate_app("Finder");
    let script = r#"tell application "System Events" to keystroke "w" using command down"#;
    let _ = run_osascript_script(script);
}

fn try_activate_app(app_name: &str) -> bool {
    let script = format!(
        r#"tell application "{}" to activate"#,
        escape_applescript(app_name)
    );
    run_osascript_script(&script).is_some()
}

fn frontmost_app_is(app_name: &str) -> bool {
    frontmost_app_name()
        .map(|name| name.eq_ignore_ascii_case(app_name))
        .unwrap_or(false)
}

fn frontmost_app_name() -> Option<String> {
    let script = r#"tell application "System Events" to get name of first application process whose frontmost is true"#;
    let raw = run_osascript_script(script)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

fn try_set_clipboard_text(text: &str) -> bool {
    let script = format!("set the clipboard to \"{}\"", escape_applescript(text));
    run_osascript_script(&script).is_some()
}

fn try_arc_replace_address_with_clipboard() -> bool {
    let script = r#"tell application "System Events"
keystroke "l" using command down
delay 0.08
keystroke "v" using command down
delay 0.08
key code 36
end tell"#;
    run_osascript_script(script).is_some()
}

fn try_arc_copy_active_url() -> Option<String> {
    let script = r#"tell application "System Events"
keystroke "l" using command down
delay 0.08
keystroke "c" using command down
end tell"#;
    run_osascript_script(script)?;
    std::thread::sleep(Duration::from_millis(80));
    try_read_clipboard_text()
}

fn try_read_clipboard_text() -> Option<String> {
    let out = Command::new("osascript")
        .args(["-e", "the clipboard as text"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn try_spotify_is_playing() -> Option<bool> {
    let script = r#"tell application "Spotify" to return (player state as text)"#;
    let raw = run_osascript_script(script)?;
    let state = raw.trim().to_ascii_lowercase();
    Some(state.contains("playing"))
}

fn try_spotify_playpause() -> bool {
    let script = r#"tell application "Spotify" to playpause"#;
    run_osascript_script(script).is_some()
}

fn run_osascript_script(script: &str) -> Option<String> {
    let out = Command::new("osascript")
        .args(["-e", script])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

fn send_unmodified_key(key: &str, step: &str) {
    let keycode = match key.trim().to_ascii_lowercase().as_str() {
        "space" => 49_u16,
        "return" | "enter" => 36_u16,
        "escape" => 53_u16,
        _ => panic!("step `{step}` unsupported unmodified key `{key}`"),
    };

    let script = format!("tell application \"System Events\" to key code {keycode}");
    let out = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .unwrap_or_else(|err| panic!("step `{step}` failed to execute osascript: {err}"));
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        panic!(
            "step `{step}` failed sending key `{key}`: {}",
            stderr.trim()
        );
    }
}

fn step_timeout() -> Duration {
    env::var("MACOS_AGENT_REAL_E2E_STEP_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= STEP_TIMEOUT_MIN_MS)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_STEP_TIMEOUT_MS))
}

fn run_with_timeout(
    bin: &Path,
    args: &[&str],
    options: &CmdOptions,
    timeout: Duration,
) -> CmdOutput {
    let mut cmd = Command::new(bin);
    if let Some(cwd) = options.cwd.as_deref() {
        cmd.current_dir(cwd);
    }
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

    for key in &options.env_remove {
        cmd.env_remove(key);
    }
    for (key, value) in &options.envs {
        cmd.env(key, value);
    }

    if options.stdin.is_some() {
        cmd.stdin(Stdio::piped());
    } else if options.stdin_null {
        cmd.stdin(Stdio::null());
    }

    let mut child = cmd.spawn().expect("spawn command");
    if let Some(input) = options.stdin.as_ref()
        && let Some(mut writer) = child.stdin.take()
    {
        writer.write_all(input).expect("write stdin");
    }

    let pid = child.id();
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => CmdOutput {
            code: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        },
        Ok(Err(err)) => CmdOutput {
            code: -1,
            stdout: Vec::new(),
            stderr: format!("wait command failed: {err}").into_bytes(),
        },
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            let mut stderr = format!(
                "command timed out after {} ms and was killed (pid={pid})",
                timeout.as_millis()
            )
            .into_bytes();

            match rx.recv_timeout(Duration::from_millis(STEP_TIMEOUT_KILL_GRACE_MS)) {
                Ok(Ok(output)) => {
                    if !output.stderr.is_empty() {
                        stderr.extend_from_slice(b"\n");
                        stderr.extend_from_slice(&output.stderr);
                    }
                    CmdOutput {
                        code: -1,
                        stdout: output.stdout,
                        stderr,
                    }
                }
                Ok(Err(err)) => {
                    stderr.extend_from_slice(b"\n");
                    stderr.extend_from_slice(format!("failed collecting output: {err}").as_bytes());
                    CmdOutput {
                        code: -1,
                        stdout: Vec::new(),
                        stderr,
                    }
                }
                Err(_) => CmdOutput {
                    code: -1,
                    stdout: Vec::new(),
                    stderr,
                },
            }
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => CmdOutput {
            code: -1,
            stdout: Vec::new(),
            stderr: b"command output channel disconnected".to_vec(),
        },
    }
}
