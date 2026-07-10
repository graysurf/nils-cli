use serde::{Deserialize, Serialize};

use crate::backend::applescript::Modifier;
use crate::backend::process::{ProcessFailure, ProcessRequest, ProcessRunner};
use crate::cli::ScrollUnit;
use crate::error::CliError;

const SCROLL_SCRIPT: &str = r#"
local json = hs.json
local args = (_cli and _cli.args) or {}
local raw = args[1]
if raw == "--" then raw = args[2] end
local payload = json.decode(raw or "{}")
if type(payload) ~= "table" then error("invalid input.scroll payload", 0) end
hs.eventtap.scrollWheel(
  { tonumber(payload.delta_x) or 0, tonumber(payload.delta_y) or 0 },
  payload.mods or {},
  tostring(payload.unit or "pixel")
)
return json.encode({ scrolled = true })
"#;

#[derive(Debug, Serialize)]
struct ScrollPayload {
    delta_x: i32,
    delta_y: i32,
    unit: &'static str,
    mods: Vec<&'static str>,
}

#[derive(Debug, Deserialize)]
struct ScrollResponse {
    scrolled: bool,
}

pub fn scroll(
    runner: &dyn ProcessRunner,
    delta_x: i32,
    delta_y: i32,
    unit: ScrollUnit,
    mods: &[Modifier],
    timeout_ms: u64,
) -> Result<(), CliError> {
    let unit = match unit {
        ScrollUnit::Pixel => "pixel",
        ScrollUnit::Line => "line",
    };
    let payload = serde_json::to_string(&ScrollPayload {
        delta_x,
        delta_y,
        unit,
        mods: mods.iter().map(|modifier| modifier.canonical()).collect(),
    })
    .map_err(|error| {
        CliError::runtime(format!("failed to encode input.scroll payload: {error}"))
            .with_operation("input.scroll")
    })?;
    let timeout_seconds = format!("{:.3}", (timeout_ms.max(1) as f64) / 1000.0);
    let request = ProcessRequest::new(
        "hs",
        vec![
            "-q".to_string(),
            "-t".to_string(),
            timeout_seconds,
            "-c".to_string(),
            SCROLL_SCRIPT.to_string(),
            "--".to_string(),
            payload,
        ],
        timeout_ms.max(1),
    );
    let output = runner.run(&request).map_err(map_scroll_failure)?;
    let response: ScrollResponse = serde_json::from_str(output.stdout.trim()).map_err(|error| {
        CliError::runtime(format!("input.scroll returned invalid JSON: {error}"))
            .with_operation("input.scroll")
            .with_hint("Ensure Hammerspoon is running and `hs.ipc` is enabled.")
    })?;
    if !response.scrolled {
        return Err(
            CliError::runtime("input.scroll backend did not confirm the event")
                .with_operation("input.scroll"),
        );
    }
    Ok(())
}

fn map_scroll_failure(failure: ProcessFailure) -> CliError {
    match failure {
        ProcessFailure::NotFound { .. } => {
            CliError::runtime("input.scroll failed: missing dependency `hs` in PATH")
                .with_operation("input.scroll")
                .with_hint("Install Hammerspoon and ensure its `hs` CLI is in PATH.")
        }
        ProcessFailure::Timeout { timeout_ms, .. } => CliError::timeout(
            "input.scroll via `hs`",
            timeout_ms,
        )
        .with_operation("input.scroll")
        .with_hint(
            "Keep Hammerspoon running and enable `require('hs.ipc')` in ~/.hammerspoon/init.lua.",
        ),
        ProcessFailure::NonZero { code, stderr, .. } => {
            let lower = stderr.to_ascii_lowercase();
            let unavailable = lower.contains("message port")
                || lower.contains("ipc module")
                || lower.contains("is it running")
                || lower.contains("connection refused");
            let mut error = CliError::runtime(format!(
                "input.scroll failed via `hs` (exit {code}): {stderr}"
            ))
            .with_operation("input.scroll");
            if unavailable {
                error = error
                    .with_hint("Hammerspoon IPC is unavailable; keep Hammerspoon running.")
                    .with_hint(
                        "Enable `require('hs.ipc')` in ~/.hammerspoon/init.lua and reload the config.",
                    );
            }
            error
        }
        ProcessFailure::Io { message, .. } => {
            CliError::runtime(format!("input.scroll failed to run `hs`: {message}"))
                .with_operation("input.scroll")
                .with_hint("Check the Hammerspoon `hs` executable and local IPC state.")
        }
    }
}
