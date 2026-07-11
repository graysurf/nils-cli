use nils_common::cli_contract::exit;
use nils_common::diag_output;
use nils_common::env as shared_env;
use nils_common::process;
use nils_common::shell::{AnsiStripMode, quote_posix_single, strip_ansi};
use nils_common::usage_time::reset_epoch_seconds_from_str;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{auth, cache, client, render};

const USAGE_SCHEMA_VERSION: &str = "claude-cli.usage.v1";
const USAGE_COMMAND: &str = "usage";
const DEFAULT_CLAUDE_BIN: &str = "claude";
const DEFAULT_CLAUDE_TIMEOUT_SECONDS: u64 = 15;
const DEFAULT_PTY_STARTUP_DELAY_MS: u64 = 4_000;
const DEFAULT_PTY_USAGE_DELAY_MS: u64 = 3_000;
const CLI_USAGE_STDIN: &[u8] = b"/usage\n/exit\n";
const CLI_USAGE_PTY_USAGE_STDIN: &[u8] = b"/usage\r";
const CLI_USAGE_PTY_EXIT_STDIN: &[u8] = b"/exit\r";

#[derive(Clone, Debug)]
pub struct UsageOptions {
    pub source: UsageSource,
    pub output_json: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UsageSource {
    Auto,
    Oauth,
    Cli,
    Cache,
}

#[derive(Debug, Clone, Serialize)]
struct UsageResult {
    provider: String,
    source: String,
    stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<i64>,
    windows: Vec<UsageWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct UsageWindow {
    key: String,
    label: String,
    window_minutes: i64,
    used_percent: f64,
    remaining_percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    resets_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resets_at_epoch: Option<i64>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum WindowKind {
    FiveHour,
    SevenDay,
}

#[derive(Default)]
struct ParsedWindow {
    used_percent: Option<f64>,
    resets_at: Option<String>,
}

pub fn run(options: &UsageOptions) -> i32 {
    let result = resolve_usage(options.source);

    if options.output_json {
        if diag_output::emit_success_result(USAGE_SCHEMA_VERSION, USAGE_COMMAND, &result).is_err() {
            return exit::RUNTIME;
        }
    } else {
        println!("{}", render_text_result(&result));
    }

    exit::SUCCESS
}

fn resolve_usage(source: UsageSource) -> UsageResult {
    let cache_file = cache::cache_file();
    match source {
        UsageSource::Auto => {
            if let Some(result) = try_oauth(cache_file.as_ref()) {
                return result;
            }
            if let Some(result) = try_claude_cli(cache_file.as_ref()) {
                return result;
            }
            read_cache(cache_file.as_ref())
                .unwrap_or_else(|| empty_result(cache_file, "usage unavailable"))
        }
        UsageSource::Oauth => try_oauth(cache_file.as_ref())
            .unwrap_or_else(|| empty_result(cache_file, "oauth usage unavailable")),
        UsageSource::Cli => try_claude_cli(cache_file.as_ref())
            .unwrap_or_else(|| empty_result(cache_file, "claude cli usage unavailable")),
        UsageSource::Cache => read_cache(cache_file.as_ref())
            .unwrap_or_else(|| empty_result(cache_file, "cache missing")),
    }
}

fn try_oauth(cache_file: Option<&PathBuf>) -> Option<UsageResult> {
    let token = auth::resolve_access_token()?;
    let body = client::fetch_usage(&token.value).ok()?;
    let value: Value = serde_json::from_str(&body).ok()?;
    let usage = render::parse_usage_value(&value)?;
    if let Some(cache_file) = cache_file {
        let _ = cache::write_cache_file(cache_file, &body);
    }
    Some(result_from_usage(
        "oauth",
        false,
        cache_file,
        Some(now_epoch_seconds()),
        &usage,
        None,
    ))
}

fn try_claude_cli(cache_file: Option<&PathBuf>) -> Option<UsageResult> {
    let body = probe_claude_cli_usage().ok()?;
    let usage = parse_cli_usage_output(&body)?;
    let cache_body = usage_cache_body(&usage).ok()?;
    if let Some(cache_file) = cache_file {
        let _ = cache::write_cache_file(cache_file, &cache_body);
    }
    Some(result_from_usage(
        "cli",
        false,
        cache_file,
        Some(now_epoch_seconds()),
        &usage,
        None,
    ))
}

fn read_cache(cache_file: Option<&PathBuf>) -> Option<UsageResult> {
    let cache_file = cache_file?;
    let raw = cache::read_cache_file(cache_file)?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let usage = render::parse_usage_value(&value)?;
    Some(result_from_usage(
        "cache",
        true,
        Some(cache_file),
        modified_epoch_seconds(cache_file),
        &usage,
        Some("serving last cached usage".to_string()),
    ))
}

fn empty_result(cache_file: Option<PathBuf>, note: &str) -> UsageResult {
    UsageResult {
        provider: "claude".to_string(),
        source: "none".to_string(),
        stale: true,
        cache_file: cache_file.as_ref().map(|path| display_path(path)),
        updated_at: None,
        windows: Vec::new(),
        plan: None,
        note: Some(note.to_string()),
    }
}

fn result_from_usage(
    source: &str,
    stale: bool,
    cache_file: Option<&PathBuf>,
    updated_at: Option<i64>,
    usage: &render::Usage,
    note: Option<String>,
) -> UsageResult {
    let mut windows = Vec::new();
    if let Some(window) = &usage.five_hour {
        windows.push(usage_window("5h", "5h", 300, window, updated_at));
    }
    if let Some(window) = &usage.seven_day {
        windows.push(usage_window("weekly", "Weekly", 10_080, window, updated_at));
    }

    UsageResult {
        provider: "claude".to_string(),
        source: source.to_string(),
        stale,
        cache_file: cache_file.map(|path| display_path(path)),
        updated_at,
        windows,
        plan: None,
        note,
    }
}

fn usage_window(
    key: &str,
    label: &str,
    window_minutes: i64,
    window: &render::Window,
    reference_epoch: Option<i64>,
) -> UsageWindow {
    let resets_at = window.resets_at.clone();
    let resets_at_epoch = resets_at
        .as_deref()
        .and_then(|raw| reset_epoch_seconds(raw, reference_epoch));
    UsageWindow {
        key: key.to_string(),
        label: label.to_string(),
        window_minutes,
        used_percent: round_percent(window.used_percent),
        remaining_percent: round_percent(f64::from(window.remaining_percent as i32)),
        resets_at,
        resets_at_epoch,
    }
}

fn render_text_result(result: &UsageResult) -> String {
    if result.windows.is_empty() {
        return result
            .note
            .clone()
            .unwrap_or_else(|| "usage unavailable".to_string());
    }

    let mut parts = vec![format!("source={}", result.source)];
    for window in &result.windows {
        parts.push(format!(
            "{}:{}%",
            window.label,
            round_percent(window.remaining_percent)
        ));
    }
    if result.stale {
        parts.push("(stale)".to_string());
    }
    parts.join(" ")
}

fn probe_claude_cli_usage() -> anyhow::Result<String> {
    let claude_bin = shared_env::env_non_empty("CLAUDE_PROMPT_SEGMENT_CLAUDE_BIN")
        .unwrap_or_else(|| DEFAULT_CLAUDE_BIN.to_string());
    let program = process::find_in_path(&claude_bin).unwrap_or_else(|| PathBuf::from(&claude_bin));
    let timeout_seconds = shared_env::env_non_empty("CLAUDE_PROMPT_SEGMENT_CLAUDE_TIMEOUT_SECONDS")
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_CLAUDE_TIMEOUT_SECONDS);

    probe_claude_cli_usage_with_modes(
        &program,
        &select_probe_modes(&program),
        Instant::now() + Duration::from_secs(timeout_seconds),
    )
}

fn probe_claude_cli_usage_with_modes(
    program: &Path,
    modes: &[ProbeMode],
    deadline: Instant,
) -> anyhow::Result<String> {
    let mut last_error: Option<anyhow::Error> = None;
    for mode in modes {
        if Instant::now() >= deadline {
            last_error = Some(anyhow::anyhow!("claude usage probe timed out"));
            break;
        }
        match probe_claude_cli_usage_with_mode(program, deadline, mode) {
            Ok(text)
                if parse_cli_usage_output(&text).is_some() || matches!(mode, ProbeMode::Pipe) =>
            {
                return Ok(text);
            }
            Ok(_) => {
                last_error = Some(anyhow::anyhow!(
                    "claude usage probe output was not parseable"
                ));
            }
            Err(error) => {
                last_error = Some(error);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("claude usage probe unavailable")))
}

fn probe_claude_cli_usage_with_mode(
    program: &Path,
    deadline: Instant,
    mode: &ProbeMode,
) -> anyhow::Result<String> {
    let mut command = mode.command(program);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take()
        && let Err(error) = mode.write_usage_input(&mut stdin, deadline)
    {
        kill_process_tree(&mut child);
        let _ = child.wait();
        return Err(error);
    }

    loop {
        if child.try_wait()?.is_some() {
            let output = child.wait_with_output()?;
            let text = output_text(&output.stdout, &output.stderr);
            if output.status.success() || !text.trim().is_empty() {
                return Ok(text);
            }
            anyhow::bail!("claude exited nonzero");
        }

        if Instant::now() >= deadline {
            kill_process_tree(&mut child);
            let output = child.wait_with_output()?;
            let text = output_text(&output.stdout, &output.stderr);
            if !text.trim().is_empty() {
                return Ok(text);
            }
            anyhow::bail!("claude usage probe timed out");
        }

        thread::sleep(Duration::from_millis(25));
    }
}

fn kill_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Ok(root) = libc::pid_t::try_from(child.id()) {
        for pid in descendant_process_ids(root).into_iter().rev() {
            unsafe {
                let _ = libc::kill(pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
}

#[cfg(target_os = "linux")]
fn descendant_process_ids(root: libc::pid_t) -> Vec<libc::pid_t> {
    let mut discovered = vec![root];
    let mut cursor = 0;
    while cursor < discovered.len() {
        let parent = discovered[cursor];
        cursor += 1;
        let children_path = format!("/proc/{parent}/task/{parent}/children");
        if let Ok(raw) = std::fs::read_to_string(children_path) {
            for child in raw
                .split_whitespace()
                .filter_map(|value| value.parse::<libc::pid_t>().ok())
            {
                if !discovered.contains(&child) {
                    discovered.push(child);
                }
            }
        }
    }
    discovered.remove(0);
    discovered
}

#[cfg(all(unix, not(target_os = "linux")))]
fn descendant_process_ids(root: libc::pid_t) -> Vec<libc::pid_t> {
    let Ok(mut child) = Command::new("/bin/ps")
        .args(["-eo", "pid=,ppid="])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let deadline = Instant::now() + Duration::from_millis(250);
    let output = loop {
        match child.try_wait() {
            Ok(Some(_)) => break child.wait_with_output().ok(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let Some(output) = output.filter(|output| output.status.success()) else {
        return Vec::new();
    };
    descendants_from_process_rows(root, &String::from_utf8_lossy(&output.stdout))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn descendants_from_process_rows(root: libc::pid_t, raw: &str) -> Vec<libc::pid_t> {
    let rows = raw
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<libc::pid_t>().ok()?;
            let parent = fields.next()?.parse::<libc::pid_t>().ok()?;
            Some((pid, parent))
        })
        .collect::<Vec<_>>();
    let mut discovered = vec![root];
    loop {
        let mut changed = false;
        for (pid, parent) in &rows {
            if discovered.contains(parent) && !discovered.contains(pid) {
                discovered.push(*pid);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    discovered.remove(0);
    discovered
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ProbeMode {
    Pty(ScriptLauncher),
    Pipe,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScriptLauncher {
    program: PathBuf,
    flavor: ScriptFlavor,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ScriptFlavor {
    UtilLinux,
    Bsd,
}

impl ProbeMode {
    fn command(&self, program: &Path) -> Command {
        match self {
            Self::Pty(launcher) => script_command(program, launcher),
            Self::Pipe => Command::new(program),
        }
    }

    fn write_usage_input(&self, stdin: &mut dyn Write, deadline: Instant) -> anyhow::Result<()> {
        match self {
            Self::Pty(_) => {
                sleep_before_probe_deadline(
                    Duration::from_millis(env_u64(
                        "CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_STARTUP_DELAY_MS",
                        DEFAULT_PTY_STARTUP_DELAY_MS,
                    )),
                    deadline,
                )?;
                stdin.write_all(CLI_USAGE_PTY_USAGE_STDIN)?;
                stdin.flush()?;
                sleep_before_probe_deadline(
                    Duration::from_millis(env_u64(
                        "CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_USAGE_DELAY_MS",
                        DEFAULT_PTY_USAGE_DELAY_MS,
                    )),
                    deadline,
                )?;
                stdin.write_all(CLI_USAGE_PTY_EXIT_STDIN)?;
                stdin.flush()?;
            }
            Self::Pipe => {
                if Instant::now() >= deadline {
                    anyhow::bail!("claude usage probe timed out");
                }
                stdin.write_all(CLI_USAGE_STDIN)?;
            }
        }
        Ok(())
    }
}

fn sleep_before_probe_deadline(delay: Duration, deadline: Instant) -> anyhow::Result<()> {
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        anyhow::bail!("claude usage probe timed out");
    };
    if delay >= remaining {
        thread::sleep(remaining);
        anyhow::bail!("claude usage probe timed out");
    }
    thread::sleep(delay);
    Ok(())
}

fn select_probe_modes(program: &Path) -> Vec<ProbeMode> {
    let script_program = if cfg!(unix) {
        process::find_in_path("script")
    } else {
        None
    };
    select_probe_modes_for(
        shared_env::env_truthy("CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_DISABLED"),
        program.is_file(),
        script_program,
    )
}

fn select_probe_modes_for(
    pty_disabled: bool,
    program_is_file: bool,
    script_program: Option<PathBuf>,
) -> Vec<ProbeMode> {
    let mut modes = Vec::new();
    if !pty_disabled
        && program_is_file
        && let Some(program) = script_program
    {
        modes.push(ProbeMode::Pty(ScriptLauncher {
            program,
            flavor: platform_script_flavor(),
        }));
    }
    modes.push(ProbeMode::Pipe);
    modes
}

fn script_command(program: &Path, launcher: &ScriptLauncher) -> Command {
    let mut command = Command::new(&launcher.program);
    command.args(script_command_args(program, launcher.flavor));
    command
}

fn script_command_args(program: &Path, flavor: ScriptFlavor) -> Vec<OsString> {
    match flavor {
        ScriptFlavor::UtilLinux => vec![
            OsString::from("-q"),
            OsString::from("-c"),
            OsString::from(quote_posix_single(&program.to_string_lossy())),
            OsString::from("/dev/null"),
        ],
        ScriptFlavor::Bsd => vec![
            OsString::from("-q"),
            OsString::from("/dev/null"),
            program.as_os_str().to_os_string(),
        ],
    }
}

fn platform_script_flavor() -> ScriptFlavor {
    if cfg!(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    )) {
        ScriptFlavor::Bsd
    } else {
        ScriptFlavor::UtilLinux
    }
}

fn output_text(stdout: &[u8], stderr: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(stdout).to_string();
    if !stderr.is_empty() {
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(stderr));
    }
    text
}

fn parse_cli_usage_output(raw: &str) -> Option<render::Usage> {
    let stripped = strip_ansi(raw, AnsiStripMode::CsiAnyTerminator);
    let normalized = stripped.replace('\r', "\n");
    let mut current = None;
    let mut five_hour = ParsedWindow::default();
    let mut seven_day = ParsedWindow::default();

    for line in normalized.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("fable") {
            current = None;
            continue;
        }
        if let Some(kind) = classify_window_line(&lower) {
            current = Some(kind);
        }

        let kind = classify_window_line(&lower).or(current);
        if let Some(kind) = kind {
            if let Some(used_percent) = parse_used_percent(trimmed) {
                target_window(kind, &mut five_hour, &mut seven_day).used_percent =
                    Some(used_percent);
            }
            if lower.contains("reset")
                && let Some(resets_at) = parse_reset_value(trimmed)
            {
                target_window(kind, &mut five_hour, &mut seven_day).resets_at = Some(resets_at);
            }
        }
    }

    let five_hour = build_window(five_hour);
    let seven_day = build_window(seven_day);
    if five_hour.is_none() && seven_day.is_none() {
        return None;
    }
    Some(render::Usage {
        five_hour,
        seven_day,
    })
}

fn classify_window_line(lower: &str) -> Option<WindowKind> {
    if lower.contains("week") || lower.contains("7-day") || lower.contains("7 day") {
        return Some(WindowKind::SevenDay);
    }
    if lower.contains("5-hour")
        || lower.contains("5 hour")
        || lower.contains("5h")
        || lower.contains("session")
    {
        return Some(WindowKind::FiveHour);
    }
    None
}

fn target_window<'a>(
    kind: WindowKind,
    five_hour: &'a mut ParsedWindow,
    seven_day: &'a mut ParsedWindow,
) -> &'a mut ParsedWindow {
    match kind {
        WindowKind::FiveHour => five_hour,
        WindowKind::SevenDay => seven_day,
    }
}

fn build_window(parsed: ParsedWindow) -> Option<render::Window> {
    let used_percent = parsed.used_percent?;
    Some(render::Window {
        used_percent,
        remaining_percent: remaining_percent(used_percent),
        resets_at: parsed.resets_at,
    })
}

fn parse_used_percent(line: &str) -> Option<f64> {
    let lower = line.to_ascii_lowercase();
    let percentages = percent_values(&lower);
    if percentages.is_empty() {
        return None;
    }

    for word in ["used", "usage", "utilized"] {
        if let Some(value) = percent_before_word(&lower, &percentages, word) {
            return Some(value.clamp(0.0, 100.0));
        }
    }
    for word in ["remaining", "left", "available"] {
        if let Some(value) = percent_before_word(&lower, &percentages, word) {
            return Some((100.0 - value).clamp(0.0, 100.0));
        }
    }

    let first = percentages.first()?.1;
    if lower.contains("remaining") || lower.contains("left") {
        Some((100.0 - first).clamp(0.0, 100.0))
    } else {
        Some(first.clamp(0.0, 100.0))
    }
}

fn percent_values(line: &str) -> Vec<(usize, f64)> {
    let mut values = Vec::new();
    for (percent_index, ch) in line.char_indices() {
        if ch != '%' {
            continue;
        }
        let prefix = &line[..percent_index];
        let start = prefix
            .char_indices()
            .rev()
            .find(|(_, ch)| !ch.is_ascii_digit() && *ch != '.')
            .map(|(idx, ch)| idx + ch.len_utf8())
            .unwrap_or(0);
        if start >= percent_index {
            continue;
        }
        if let Ok(value) = line[start..percent_index].trim().parse::<f64>() {
            values.push((percent_index + 1, value));
        }
    }
    values
}

fn percent_before_word(line: &str, percentages: &[(usize, f64)], word: &str) -> Option<f64> {
    let word_index = line.find(word)?;
    percentages
        .iter()
        .take_while(|(end_index, _)| *end_index <= word_index)
        .last()
        .map(|(_, value)| *value)
}

fn parse_reset_value(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    for marker in ["resets at", "reset at", "resets:", "reset:"] {
        if let Some(index) = lower.find(marker) {
            let raw = line[index + marker.len()..]
                .trim()
                .trim_start_matches(':')
                .trim();
            if !raw.is_empty() {
                return Some(raw.to_string());
            }
        }
    }
    for marker in ["resets", "reset"] {
        if lower.starts_with(marker) {
            let raw = line[marker.len()..].trim();
            if !raw.is_empty() {
                return Some(raw.to_string());
            }
        }
    }
    line.split_once(':')
        .map(|(_, raw)| raw.trim())
        .filter(|raw| !raw.is_empty())
        .map(ToOwned::to_owned)
}

fn env_u64(key: &str, default: u64) -> u64 {
    shared_env::env_non_empty(key)
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(default)
}

fn usage_cache_body(usage: &render::Usage) -> serde_json::Result<String> {
    let mut usage_map = Map::new();
    if let Some(window) = &usage.five_hour {
        usage_map.insert("five_hour".to_string(), cache_window(window));
    }
    if let Some(window) = &usage.seven_day {
        usage_map.insert("seven_day".to_string(), cache_window(window));
    }
    serde_json::to_string(&json!({ "usage": Value::Object(usage_map) }))
}

fn cache_window(window: &render::Window) -> Value {
    let mut map = Map::new();
    map.insert(
        "utilization".to_string(),
        json!(round_percent(window.used_percent)),
    );
    if let Some(resets_at) = &window.resets_at {
        map.insert("resets_at".to_string(), json!(resets_at));
    }
    Value::Object(map)
}

fn remaining_percent(used_percent: f64) -> i64 {
    (100.0 - used_percent.round()).clamp(0.0, 100.0) as i64
}

fn round_percent(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn reset_epoch_seconds(raw: &str, reference_epoch: Option<i64>) -> Option<i64> {
    reset_epoch_seconds_from_str(raw, reference_epoch)
}

fn now_epoch_seconds() -> i64 {
    epoch_seconds(SystemTime::now())
}

fn modified_epoch_seconds(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(epoch_seconds(modified))
}

fn epoch_seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_usage_parser_reads_used_and_remaining_lines() {
        let usage = parse_cli_usage_output(
            "\x1b[32mCurrent session\x1b[0m\n\
             5-hour limit: 25% used, 75% remaining\n\
             Resets at 2026-01-01T00:00:00+00:00\n\
             Current week\n\
             Weekly limit: 50% left\n",
        )
        .expect("usage");

        let five = usage.five_hour.expect("five hour");
        assert_eq!(five.used_percent, 25.0);
        assert_eq!(five.remaining_percent, 75);
        assert_eq!(five.resets_at.as_deref(), Some("2026-01-01T00:00:00+00:00"));

        let weekly = usage.seven_day.expect("weekly");
        assert_eq!(weekly.used_percent, 50.0);
        assert_eq!(weekly.remaining_percent, 50);
    }

    #[test]
    fn usage_cache_body_uses_prompt_segment_cache_shape() {
        let body = usage_cache_body(&render::Usage {
            five_hour: Some(render::Window {
                used_percent: 25.0,
                remaining_percent: 75,
                resets_at: None,
            }),
            seven_day: None,
        })
        .expect("cache body");

        let value: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["usage"]["five_hour"]["utilization"], 25.0);
    }

    #[test]
    fn script_command_args_use_util_linux_c_shape() {
        let args = script_command_args(Path::new("/opt/bin/claude"), ScriptFlavor::UtilLinux);

        assert_eq!(
            args,
            vec!["-q", "-c", "'/opt/bin/claude'", "/dev/null"]
                .into_iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn script_command_args_use_bsd_file_command_shape() {
        let args = script_command_args(Path::new("/opt/bin/claude"), ScriptFlavor::Bsd);

        assert_eq!(
            args,
            vec!["-q", "/dev/null", "/opt/bin/claude"]
                .into_iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn pty_command_uses_resolved_script_path_and_flavor() {
        let mode = ProbeMode::Pty(ScriptLauncher {
            program: PathBuf::from("/custom/bin/script"),
            flavor: ScriptFlavor::Bsd,
        });
        let command = mode.command(Path::new("/opt/bin/claude"));

        assert_eq!(command.get_program(), Path::new("/custom/bin/script"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                std::ffi::OsStr::new("-q"),
                std::ffi::OsStr::new("/dev/null"),
                std::ffi::OsStr::new("/opt/bin/claude"),
            ]
        );
    }

    #[test]
    fn selected_probe_modes_keep_pipe_fallback_after_pty() {
        let modes = select_probe_modes_for(false, true, Some(PathBuf::from("/usr/bin/script")));

        assert_eq!(
            modes,
            vec![
                ProbeMode::Pty(ScriptLauncher {
                    program: PathBuf::from("/usr/bin/script"),
                    flavor: platform_script_flavor(),
                }),
                ProbeMode::Pipe,
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn descendant_lookup_does_not_wait_for_path_ps() {
        use nils_test_support::{GlobalStateLock, StubBinDir, prepend_path};

        let lock = GlobalStateLock::new();
        let stubs = StubBinDir::new();
        stubs.write_exe("ps", "#!/usr/bin/env sh\nsleep 1\n");
        let _path = prepend_path(&lock, stubs.path());

        let started = Instant::now();
        let _ = descendant_process_ids(std::process::id() as libc::pid_t);

        assert!(
            started.elapsed() < Duration::from_millis(500),
            "descendant lookup must not wait for a PATH-provided ps"
        );
    }

    #[cfg(unix)]
    #[test]
    fn probe_modes_share_one_absolute_deadline() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let program = tmp.path().join("hanging-claude");
        let calls = tmp.path().join("calls");
        std::fs::write(
            &program,
            format!(
                "#!/usr/bin/env sh\nprintf x >> '{}'\nexec sleep 60\n",
                calls.display()
            ),
        )
        .expect("write program");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod program");

        let deadline = Instant::now() + Duration::from_millis(200);
        let error = probe_claude_cli_usage_with_modes(
            &program,
            &[ProbeMode::Pipe, ProbeMode::Pipe],
            deadline,
        )
        .expect_err("probe deadline");

        assert!(error.to_string().contains("timed out"));
        assert_eq!(std::fs::read_to_string(calls).expect("call count"), "x");
    }

    #[cfg(unix)]
    #[test]
    fn pty_timeout_kills_hup_ignoring_command() {
        use std::os::unix::fs::PermissionsExt;

        let Some(script) = process::find_in_path("script") else {
            return;
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let program = tmp.path().join("hup-ignoring-claude");
        let pid_file = tmp.path().join("command.pid");
        std::fs::write(
            &program,
            format!(
                "#!/usr/bin/env sh\ntrap '' HUP TERM\nprintf '%s' \"$$\" > '{}'\nwhile :; do sleep 1; done\n",
                pid_file.display()
            ),
        )
        .expect("write program");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod program");

        let mode = ProbeMode::Pty(ScriptLauncher {
            program: script,
            flavor: platform_script_flavor(),
        });
        let _ = probe_claude_cli_usage_with_mode(
            &program,
            Instant::now() + Duration::from_secs(1),
            &mode,
        );
        let pid = std::fs::read_to_string(&pid_file)
            .expect("command pid")
            .parse::<libc::pid_t>()
            .expect("numeric pid");
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        let survived = unsafe { libc::kill(pid, 0) } == 0;
        if survived {
            unsafe {
                let _ = libc::kill(pid, libc::SIGKILL);
            }
        }
        assert!(
            !survived,
            "PTY command must not survive its launcher timeout"
        );
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    #[test]
    fn platform_script_flavor_is_bsd_on_bsd_targets() {
        assert_eq!(platform_script_flavor(), ScriptFlavor::Bsd);
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    )))]
    #[test]
    fn platform_script_flavor_is_util_linux_on_other_targets() {
        assert_eq!(platform_script_flavor(), ScriptFlavor::UtilLinux);
    }

    #[test]
    fn cli_usage_parser_ignores_fable_weekly_subwindow() {
        let usage = parse_cli_usage_output(
            "Current session\n\
             98%used\n\
             Resets6:20am(Asia/Taipei)\n\
             Current week (all models)\n\
             67% used\n\
             Resets Jul 12, 9pm (Asia/Taipei)\n\
             Current week (Fable)\n\
             12% used\n\
             Resets Jul 12, 8:59pm (Asia/Taipei)\n",
        )
        .expect("usage");

        let five = usage.five_hour.expect("five hour");
        assert_eq!(five.used_percent, 98.0);
        assert_eq!(five.resets_at.as_deref(), Some("6:20am(Asia/Taipei)"));

        let weekly = usage.seven_day.expect("weekly");
        assert_eq!(weekly.used_percent, 67.0);
        assert_eq!(
            weekly.resets_at.as_deref(),
            Some("Jul 12, 9pm (Asia/Taipei)")
        );
    }
}
