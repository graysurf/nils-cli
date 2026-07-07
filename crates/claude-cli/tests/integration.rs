use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::http::{HttpResponse, LoopbackServer};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

fn claude_cli_bin() -> PathBuf {
    bin::resolve("claude-cli")
}

fn run(args: &[&str], options: &CmdOptions) -> CmdOutput {
    let bin = claude_cli_bin();
    cmd::run_with(&bin, args, options)
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(output.code, code, "stderr: {}", output.stderr_text());
}

fn stdout(output: &CmdOutput) -> String {
    output.stdout_text()
}

fn stderr(output: &CmdOutput) -> String {
    output.stderr_text()
}

fn base_options(cache_dir: &Path) -> CmdOptions {
    CmdOptions::default()
        .with_env_remove_prefix("CLAUDE_PROMPT")
        .with_env_remove("NO_COLOR")
        .with_env_remove("TZ")
        .with_env("CLAUDE_PROMPT_SEGMENT_CACHE_DIR", &path_str(cache_dir))
        .with_env("CLAUDE_PROMPT_SEGMENT_KEYCHAIN_DISABLED", "1")
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn write_cache(cache_dir: &Path, body: &str) -> PathBuf {
    std::fs::create_dir_all(cache_dir).expect("cache dir");
    let path = cache_dir.join("usage.json");
    std::fs::write(&path, body).expect("write cache");
    path
}

fn make_old(path: &Path) {
    let file = OpenOptions::new().write(true).open(path).expect("open");
    file.set_modified(SystemTime::now() - Duration::from_secs(120))
        .expect("set modified");
}

fn usage_json(five_utilization: f64, weekly_utilization: f64) -> String {
    format!(
        r#"{{
          "usage": {{
            "five_hour": {{"utilization": {five_utilization}, "resets_at": "2026-01-01T00:00:00+00:00"}},
            "seven_day": {{"utilization": {weekly_utilization}, "resets_at": "2026-01-03T12:30:00+00:00"}}
          }}
        }}"#
    )
}

#[cfg(unix)]
fn write_fake_claude(dir: &Path, body: &str) -> PathBuf {
    let bin_dir = dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("fake bin dir");
    let path = bin_dir.join("claude");
    std::fs::write(&path, body).expect("write fake claude");
    let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("chmod fake claude");
    bin_dir
}

#[test]
fn main_no_args_prints_help_and_exits_zero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = run(&[], &base_options(tmp.path()));
    assert_exit(&output, 0);
    let help = stdout(&output);
    assert!(help.contains("claude-cli"));
    assert!(help.contains("Prompt-segment command group"));
    assert!(help.contains("completion"));
}

#[test]
fn main_unknown_command_exits_64() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = run(&["not-a-real-command"], &base_options(tmp.path()));
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("unrecognized subcommand"));
}

#[test]
fn main_completion_exports_bash_and_zsh_scripts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let options = base_options(tmp.path());

    let zsh = run(&["completion", "zsh"], &options);
    assert_exit(&zsh, 0);
    let zsh_text = stdout(&zsh);
    assert!(zsh_text.contains("#compdef claude-cli"));
    assert!(zsh_text.contains("prompt-segment:Prompt-segment command group"));
    assert!(zsh_text.contains(":shell -- Shell to generate completion script for:(bash zsh)"));

    let bash = run(&["completion", "bash"], &options);
    assert_exit(&bash, 0);
    let bash_text = stdout(&bash);
    assert!(bash_text.contains("_claude-cli()"));
    assert!(bash_text.contains("complete -F _claude-cli"));
    assert!(bash_text.contains("opts=\"-h --help bash zsh\""));
}

#[cfg(unix)]
#[test]
fn usage_auto_falls_back_to_claude_cli_and_writes_cache() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/usr/bin/env sh
cat >/dev/null
cat <<'OUT'
Claude Code Usage

Current session
5-hour limit: 25% used, 75% remaining
Resets at 2026-01-01T00:00:00+00:00

Current week
Weekly limit: 50% used, 50% remaining
Resets at 2026-01-03T12:30:00+00:00
OUT
"#,
    );
    let server = LoopbackServer::new().expect("server");
    server.add_route("GET", "/usage", HttpResponse::new(500, "upstream exploded"));

    let output = run(
        &["usage", "--format", "json", "--source", "auto"],
        &base_options(tmp.path())
            .with_path_prepend(&bin_dir)
            .with_env("CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN", "secret-token-usage")
            .with_env(
                "CLAUDE_PROMPT_SEGMENT_ENDPOINT",
                &format!("{}/usage", server.url()),
            )
            .with_env("CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_DISABLED", "1"),
    );

    assert_exit(&output, 0);
    assert!(!stdout(&output).contains("secret-token-usage"));
    assert!(!stderr(&output).contains("secret-token-usage"));

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "claude-cli.usage.v1");
    assert_eq!(payload["command"], "usage");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["source"], "cli");
    assert_eq!(payload["result"]["stale"], false);
    assert_eq!(payload["result"]["windows"][0]["key"], "5h");
    assert_eq!(payload["result"]["windows"][0]["used_percent"], 25.0);
    assert_eq!(payload["result"]["windows"][0]["remaining_percent"], 75.0);
    assert_eq!(payload["result"]["windows"][1]["key"], "weekly");
    assert_eq!(payload["result"]["windows"][1]["used_percent"], 50.0);
    assert_eq!(payload["result"]["windows"][1]["remaining_percent"], 50.0);

    let cached = std::fs::read_to_string(tmp.path().join("usage.json")).expect("cache");
    assert!(!cached.contains("secret-token-usage"));
    assert!(cached.contains("\"five_hour\""));
    assert!(cached.contains("\"seven_day\""));
}

#[test]
fn prompt_segment_check_reads_credentials_json_without_printing_token() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let token = "secret-token-for-check";
    let options = base_options(tmp.path()).with_env(
        "CLAUDE_PROMPT_SEGMENT_CREDENTIALS_JSON",
        &format!(r#"{{"claudeAiOauth":{{"accessToken":"{token}"}}}}"#),
    );

    let output = run(&["prompt-segment", "check"], &options);
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "");
    assert_eq!(stderr(&output), "");
}

#[test]
fn prompt_segment_is_enabled_returns_one_when_credentials_are_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = run(
        &["prompt-segment", "--is-enabled"],
        &base_options(tmp.path()),
    );
    assert_exit(&output, 1);
    assert_eq!(stdout(&output), "");
}

#[test]
fn prompt_segment_renders_fresh_cached_usage_without_credentials() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_cache(tmp.path(), &usage_json(23.2, 44.1));

    let options = base_options(tmp.path())
        .with_env("NO_COLOR", "1")
        .with_env("TZ", "UTC");
    let output = run(
        &["prompt-segment", "--ttl", "1h", "--time-format", "%Y-%m-%d"],
        &options,
    );

    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "5h:77% W:56% 2026-01-03\n");
    assert_eq!(stderr(&output), "");
}

#[test]
fn prompt_segment_refresh_fetches_and_writes_cache_without_secret_leakage() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = LoopbackServer::new().expect("server");
    let body = usage_json(25.0, 50.0);
    server.add_route(
        "GET",
        "/usage",
        HttpResponse::new(200, body.clone()).with_header("Content-Type", "application/json"),
    );

    let token = "secret-token-refresh";
    let endpoint = format!("{}/usage", server.url());
    let options = base_options(tmp.path())
        .with_env("CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN", token)
        .with_env("CLAUDE_PROMPT_SEGMENT_ENDPOINT", &endpoint)
        .with_env("NO_COLOR", "1")
        .with_env("TZ", "UTC");

    let output = run(
        &["prompt-segment", "--refresh", "--time-format", "%Y-%m-%d"],
        &options,
    );

    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "5h:75% W:50% 2026-01-03\n");
    assert!(!stdout(&output).contains(token));
    assert!(!stderr(&output).contains(token));
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("usage.json")).expect("cache"),
        body
    );

    let requests = server.take_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].header_value("authorization"),
        Some(format!("Bearer {token}"))
    );
    assert_eq!(
        requests[0].header_value("anthropic-beta"),
        Some("oauth-2025-04-20".to_string())
    );
    assert_eq!(
        requests[0].header_value("user-agent"),
        Some("claude-code/2.1.0".to_string())
    );
}

#[test]
fn prompt_segment_stale_cache_fallback_suppresses_fetch_errors() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_file = write_cache(tmp.path(), &usage_json(25.0, 50.0));
    make_old(&cache_file);

    let server = LoopbackServer::new().expect("server");
    server.add_route("GET", "/usage", HttpResponse::new(500, "upstream exploded"));

    let options = base_options(tmp.path())
        .with_env("CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN", "secret-token-stale")
        .with_env(
            "CLAUDE_PROMPT_SEGMENT_ENDPOINT",
            &format!("{}/usage", server.url()),
        )
        .with_env("NO_COLOR", "1")
        .with_env("TZ", "UTC");

    let output = run(
        &["prompt-segment", "--ttl", "1s", "--time-format", "%Y-%m-%d"],
        &options,
    );

    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "5h:75% W:50% 2026-01-03 (stale)\n");
    assert_eq!(stderr(&output), "");
}

#[test]
fn prompt_segment_missing_credentials_and_cache_is_quiet_success() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = run(&["prompt-segment"], &base_options(tmp.path()));
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "");
    assert_eq!(stderr(&output), "");
}

#[test]
fn prompt_segment_status_json_has_stable_envelope_and_no_secret_leakage() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let token = "secret-token-status";
    let options = base_options(tmp.path()).with_env("CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN", token);

    let output = run(&["prompt-segment", "status", "--format", "json"], &options);
    assert_exit(&output, 0);
    assert!(!stdout(&output).contains(token));
    assert!(!stderr(&output).contains(token));

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "claude-cli.prompt-segment.v1");
    assert_eq!(payload["command"], "prompt-segment status");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["authenticated"], true);
    assert_eq!(payload["result"]["auth_source"], "access-token-env");
    assert_eq!(payload["result"]["cache_exists"], false);
    assert_eq!(payload["result"]["reason"], "cache-missing");
}
