use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::http::{HttpResponse, LoopbackServer, TestServer};
use nils_test_support::{bin, git as test_git};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// An endpoint nothing listens on, so a request that escapes a test's control
/// fails immediately instead of reaching a real host.
///
/// `base_options` clears the `CLAUDE_PROMPT` prefix to isolate ambient config,
/// which also clears `CLAUDE_PROMPT_SEGMENT_ENDPOINT` — and the production
/// default is the real usage endpoint. This is **defence in depth, not a fix for
/// an observed reach**: the same prefix removal also clears the token variables
/// and `base_options` disables the keychain, so `refresh_blocking` returns at its
/// token guard before it would fetch anything. What it buys is that the first
/// token-bearing test added under this default cannot reach a real host by
/// omission. A test that wants a server sets its own endpoint afterwards and
/// wins, because removals are applied before values.
const UNROUTABLE_ENDPOINT: &str = "http://127.0.0.1:9/usage";

/// Bound the fast-fail so it stays fast on a host that drops rather than refuses
/// loopback traffic. `gemini-cli` pins its timeouts alongside its unroutable
/// endpoint for the same reason.
const FAST_FAIL_MAX_TIME_SECONDS: &str = "1";

/// Keeps a plain `prompt-segment` run from launching a detached refresh child.
///
/// A run whose cache is expired calls `enqueue_background_refresh`, which spawns
/// `prompt-segment --refresh` and returns without waiting. That child outlives the
/// test, and its first act is `create_dir_all` on the cache directory, so it can
/// recreate the fixture `TempDir` teardown has already removed — class 3 in
/// `docs/specs/test-temp-directory-policy.md`.
///
/// A test whose subject is rendering rather than refreshing opts out through the
/// production cooldown instead of racing the child or neutering it: a recent
/// `usage.refresh.at` plus a long `CLAUDE_PROMPT_SEGMENT_REFRESH_MIN_SECONDS`
/// makes `enqueue_background_refresh` return before it spawns anything. That is
/// the approach the policy prefers, and unlike a no-op executable it is
/// falsifiable — see [`RefreshCooldown::assert_held`].
struct RefreshCooldown {
    marker: PathBuf,
    stamp: String,
}

impl RefreshCooldown {
    /// Hold the cooldown for the `usage.json` cache in `cache_dir`.
    fn hold(cache_dir: &Path) -> Self {
        std::fs::create_dir_all(cache_dir).expect("cache dir");
        let marker = cache_dir.join("usage.refresh.at");
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs().saturating_sub(5).max(1))
            .expect("epoch")
            .to_string();
        std::fs::write(&marker, &stamp).expect("write refresh marker");

        Self { marker, stamp }
    }

    /// The environment that makes the held marker actually gate the spawn.
    fn env(&self) -> (&'static str, &'static str) {
        ("CLAUDE_PROMPT_SEGMENT_REFRESH_MIN_SECONDS", "3600")
    }

    /// Fails when the run spawned a refresh child after all.
    ///
    /// `enqueue_background_refresh` rewrites the marker immediately before
    /// spawning, so an unchanged marker is proof that it returned on the cooldown
    /// and that no background writer can outlive this test.
    fn assert_held(&self) {
        assert_eq!(
            std::fs::read_to_string(&self.marker).expect("read refresh marker"),
            self.stamp,
            "the run spawned a detached refresh child instead of honouring the cooldown"
        );
    }
}

fn base_options(cache_dir: &Path) -> CmdOptions {
    CmdOptions::default()
        .with_env_remove_prefix("CLAUDE_CLI_")
        .with_env_remove_prefix("CLAUDE_PROMPT")
        .with_env_remove("NO_COLOR")
        .with_env_remove("TZ")
        .with_env(
            "CLAUDE_CONFIG_DIR",
            &path_str(&cache_dir.join("claude-config")),
        )
        .with_env("CLAUDE_PROMPT_SEGMENT_CACHE_DIR", &path_str(cache_dir))
        .with_env("CLAUDE_PROMPT_SEGMENT_KEYCHAIN_DISABLED", "1")
        .with_env("CLAUDE_PROMPT_SEGMENT_ENDPOINT", UNROUTABLE_ENDPOINT)
        .with_env(
            "CLAUDE_PROMPT_SEGMENT_MAX_TIME_SECONDS",
            FAST_FAIL_MAX_TIME_SECONDS,
        )
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

/// `<stem>.refresh.lock`, matching `prompt_segment::refresh::sibling_path`.
fn refresh_lock_path(cache_file: &Path) -> PathBuf {
    let stem = cache_file
        .file_stem()
        .expect("cache file stem")
        .to_string_lossy()
        .to_string();
    cache_file.with_file_name(format!("{stem}.refresh.lock"))
}

/// Whether no process currently holds the refresh lock.
///
/// `flock` locks belong to the open file description, so a fresh `open` here
/// contends with the detached child's descriptor even though both live on this
/// host.
fn refresh_lock_is_free(lock_file: &Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(lock_file) else {
        // No lock file means no refresh ever took it; nothing to wait for.
        return true;
    };
    // SAFETY: `flock` observes the valid descriptor owned by `file`.
    let acquired = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if acquired {
        // SAFETY: same descriptor, still owned by `file` here.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }
    acquired
}

/// Waits for the detached refresh child to drop `<stem>.refresh.lock`.
///
/// `refresh_blocking` holds that lock across *every* write it makes, so a free
/// lock means the child has no filesystem work left in the fixture. Callers must
/// first observe something the child only does while holding the lock (its HTTP
/// request, or the refreshed cache body); otherwise a free lock just means the
/// child has not started yet.
fn wait_for_refresh_lock_release(cache_file: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let lock_file = refresh_lock_path(cache_file);
    while !refresh_lock_is_free(&lock_file) {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(25));
    }
    true
}

/// Waits until the background refresh wrote `expected` *and* fully settled.
///
/// Returning as soon as the cache body lands is not enough: `refresh_blocking`
/// still has to write the `<stem>.refresh.at` marker, and `write_atomic`
/// re-creates the parent directory before writing. That marker write therefore
/// resurrects the fixture directory *after* `TempDir` removed it, leaking one
/// directory under `$TMPDIR` per run while the test still passes.
fn wait_for_background_refresh_settled(
    cache_file: &Path,
    expected: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if std::fs::read_to_string(cache_file).ok().as_deref() == Some(expected) {
            break;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(25));
    }
    wait_for_refresh_lock_release(
        cache_file,
        deadline.saturating_duration_since(Instant::now()),
    )
}

fn make_old(path: &Path) {
    set_modified(path, SystemTime::now() - Duration::from_secs(120));
}

fn set_modified(path: &Path, time: SystemTime) {
    let file = OpenOptions::new().write(true).open(path).expect("open");
    file.set_modified(time).expect("set modified");
}

fn wait_for_requests(
    server: &LoopbackServer,
    expected: usize,
) -> Vec<nils_test_support::http::RecordedRequest> {
    let deadline = Instant::now() + Duration::from_secs(4);
    let quiet_period = Duration::from_millis(300);
    let mut quiet_since = None;
    let mut requests = Vec::new();
    while Instant::now() < deadline {
        let new_requests = server.take_requests();
        if !new_requests.is_empty() {
            requests.extend(new_requests);
            quiet_since = None;
        }
        if requests.len() >= expected {
            let since = quiet_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= quiet_period {
                break;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    requests
}

fn wait_for_test_requests(
    server: &TestServer,
    expected: usize,
) -> Vec<nils_test_support::http::RecordedRequest> {
    let deadline = Instant::now() + Duration::from_secs(4);
    let quiet_period = Duration::from_millis(300);
    let mut quiet_since = None;
    let mut requests = Vec::new();
    while Instant::now() < deadline {
        let new_requests = server.take_requests();
        if !new_requests.is_empty() {
            requests.extend(new_requests);
            quiet_since = None;
        }
        if requests.len() >= expected {
            let since = quiet_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= quiet_period {
                break;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    requests
}

fn usage_json(five_utilization: f64, weekly_utilization: f64) -> String {
    usage_json_with_resets(
        five_utilization,
        weekly_utilization,
        "2026-01-01T00:00:00+00:00",
        "2026-01-03T12:30:00+00:00",
    )
}

fn usage_json_with_resets(
    five_utilization: f64,
    weekly_utilization: f64,
    five_resets_at: &str,
    weekly_resets_at: &str,
) -> String {
    format!(
        r#"{{
          "usage": {{
            "five_hour": {{"utilization": {five_utilization}, "resets_at": {five_resets_at:?}}},
            "seven_day": {{"utilization": {weekly_utilization}, "resets_at": {weekly_resets_at:?}}}
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

fn init_git_repo(repo: &Path) {
    std::fs::create_dir_all(repo).expect("repo");
    test_git::git(repo, &["init"]);
    test_git::git(repo, &["config", "user.name", "Test User"]);
    test_git::git(repo, &["config", "user.email", "test@example.com"]);
    test_git::git(repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("base.txt"), "base\n").expect("base");
    test_git::git(repo, &["add", "base.txt"]);
    test_git::git(repo, &["commit", "-m", "chore: base"]);
}

#[cfg(unix)]
fn write_agent_commit_success_tools(dir: &Path) -> PathBuf {
    let bin_dir = write_fake_claude(
        dir,
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --json-schema --safe-mode --strict-mcp-config --no-session-persistence --permission-mode --disable-slash-commands --no-chrome --tools --append-system-prompt --model --effort'
  exit 0
fi
cat >/dev/null
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"structured_output":{"type":"test","scope":"agent","subject":"commit staged changes","body_bullets":[]}}'
"#,
    );
    nils_test_support::write_exe(
        &bin_dir,
        "semantic-commit",
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "staged-context" ]; then
  printf '%s\n' 'STAGED BUNDLE'
  exit 0
fi
repo=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '--repo' ]; then repo="$arg"; fi
  previous="$arg"
done
"$REAL_GIT" -C "$repo" commit -m 'test(agent): commit staged changes' >/dev/null
if [ -n "${SEMANTIC_TEST_RETARGET_URL:-}" ]; then
  "$REAL_GIT" -C "$repo" remote set-url origin "$SEMANTIC_TEST_RETARGET_URL"
fi
"#,
    );
    bin_dir
}

#[test]
fn main_no_args_prints_help_and_exits_zero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = run(&[], &base_options(tmp.path()));
    assert_exit(&output, 0);
    let help = stdout(&output);
    assert!(help.contains("claude-cli"));
    assert!(help.contains("Authentication command group"));
    assert!(help.contains("Configuration command group"));
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
    assert!(zsh_text.contains("auth:Authentication command group"));
    assert!(zsh_text.contains("config:Configuration command group"));
    assert!(zsh_text.contains("prompt-segment:Prompt-segment command group"));
    assert!(zsh_text.contains(":shell -- Shell to generate completion script for:(bash zsh)"));

    let bash = run(&["completion", "bash"], &options);
    assert_exit(&bash, 0);
    let bash_text = stdout(&bash);
    assert!(bash_text.contains("_claude__cli()"));
    assert!(bash_text.contains("complete -F _claude__cli"));
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
    let server = TestServer::new(|_| {
        thread::sleep(Duration::from_millis(400));
        HttpResponse::new(500, "upstream exploded")
    })
    .expect("server");

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
    assert_eq!(payload["result"]["provider"], "claude");
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
fn usage_oauth_classifies_past_due_billing_without_forwarding_provider_body() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = LoopbackServer::new().expect("server");
    let provider_body = r#"{"error":{"message":"Your subscription payment is past due. Please pay your overdue invoice to restore access."}}"#;
    server.add_route("GET", "/usage", HttpResponse::new(402, provider_body));

    let output = run(
        &["usage", "--format", "json", "--source", "oauth"],
        &base_options(tmp.path())
            .with_env("CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN", "secret-token-billing")
            .with_env(
                "CLAUDE_PROMPT_SEGMENT_ENDPOINT",
                &format!("{}/usage", server.url()),
            ),
    );

    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["result"]["reason_code"], "billing_past_due");
    assert!(!stdout(&output).contains("overdue invoice"));
    assert!(!stdout(&output).contains("secret-token-billing"));
}

#[test]
fn usage_oauth_classifies_missing_auth() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = run(
        &["usage", "--format", "json", "--source", "oauth"],
        &base_options(tmp.path()),
    );

    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["result"]["reason_code"], "auth_required");
}

#[cfg(unix)]
#[test]
fn usage_cli_classifies_organization_disabled_without_forwarding_terminal_text() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/usr/bin/env sh
cat >/dev/null
printf '%s\n' 'Your organization has disabled Claude subscription access for Claude Code. Contact your admin.'
"#,
    );

    let output = run(
        &["usage", "--format", "json", "--source", "cli"],
        &base_options(tmp.path())
            .with_path_prepend(&bin_dir)
            .with_env("CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_DISABLED", "1"),
    );

    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["result"]["reason_code"], "organization_disabled");
    assert!(!stdout(&output).contains("Contact your admin"));
}

#[cfg(unix)]
#[test]
fn usage_auto_prefers_recent_structured_api_error_over_generic_usage_failure() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("claude-config");
    let transcript = config_dir.join("projects/repo/session.jsonl");
    std::fs::create_dir_all(transcript.parent().expect("parent")).expect("projects");
    std::fs::write(
        &transcript,
        r#"{"type":"assistant","isApiErrorMessage":true,"message":{"type":"message","content":[{"type":"text","text":"Your organization has disabled Claude subscription access for Claude Code. Contact your admin."}]}}
"#,
    )
    .expect("transcript");
    let bin_dir = write_fake_claude(
        tmp.path(),
        "#!/usr/bin/env sh\ncat >/dev/null\nprintf '%s\\n' 'usage unavailable'\n",
    );
    let server = LoopbackServer::new().expect("server");
    server.add_route("GET", "/usage", HttpResponse::new(429, "rate limited"));

    let output = run(
        &["usage", "--format", "json", "--source", "auto"],
        &base_options(tmp.path())
            .with_path_prepend(&bin_dir)
            .with_env("CLAUDE_CONFIG_DIR", &path_str(&config_dir))
            .with_env(
                "CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN",
                "secret-token-api-error",
            )
            .with_env(
                "CLAUDE_PROMPT_SEGMENT_ENDPOINT",
                &format!("{}/usage", server.url()),
            )
            .with_env("CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_DISABLED", "1"),
    );

    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["result"]["reason_code"], "organization_disabled");
    assert!(!stdout(&output).contains("Contact your admin"));
    assert!(!stdout(&output).contains("session.jsonl"));
}

#[cfg(unix)]
#[test]
fn usage_auto_ignores_structured_api_error_after_newer_assistant_success() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("claude-config");
    let transcript = config_dir.join("projects/repo/session.jsonl");
    std::fs::create_dir_all(transcript.parent().expect("parent")).expect("projects");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"assistant","isApiErrorMessage":true,"message":{"type":"message","content":[{"type":"text","text":"Your organization has disabled Claude subscription access for Claude Code. Contact your admin."}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"type":"message","content":[{"type":"text","text":"Access restored."}]}}"#,
            "\n"
        ),
    )
    .expect("transcript");
    let bin_dir = write_fake_claude(
        tmp.path(),
        "#!/usr/bin/env sh\ncat >/dev/null\nprintf '%s\\n' 'usage unavailable'\n",
    );
    let server = LoopbackServer::new().expect("server");
    server.add_route("GET", "/usage", HttpResponse::new(429, "rate limited"));

    let output = run(
        &["usage", "--format", "json", "--source", "auto"],
        &base_options(tmp.path())
            .with_path_prepend(&bin_dir)
            .with_env("CLAUDE_CONFIG_DIR", &path_str(&config_dir))
            .with_env(
                "CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN",
                "secret-token-api-error",
            )
            .with_env(
                "CLAUDE_PROMPT_SEGMENT_ENDPOINT",
                &format!("{}/usage", server.url()),
            )
            .with_env("CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_DISABLED", "1"),
    );

    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["result"]["reason_code"], "rate_limited");
    assert!(!stdout(&output).contains("Contact your admin"));
}

#[cfg(unix)]
#[test]
fn usage_auto_ignores_older_error_after_newer_success_in_another_transcript() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("claude-config");
    let projects = config_dir.join("projects/repo");
    std::fs::create_dir_all(&projects).expect("projects");
    let error_transcript = projects.join("older-error.jsonl");
    std::fs::write(
        &error_transcript,
        concat!(
            r#"{"type":"assistant","isApiErrorMessage":true,"message":{"content":[{"text":"Your organization has disabled Claude access."}]}}"#,
            "\n"
        ),
    )
    .expect("error transcript");
    let success_transcript = projects.join("newer-success.jsonl");
    std::fs::write(
        &success_transcript,
        concat!(
            r#"{"type":"assistant","message":{"content":[{"text":"Access restored."}]}}"#,
            "\n"
        ),
    )
    .expect("success transcript");
    let now = SystemTime::now();
    set_modified(&error_transcript, now - Duration::from_secs(60));
    set_modified(&success_transcript, now);
    let bin_dir = write_fake_claude(
        tmp.path(),
        "#!/usr/bin/env sh\ncat >/dev/null\nprintf '%s\\n' 'usage unavailable'\n",
    );
    let server = LoopbackServer::new().expect("server");
    server.add_route("GET", "/usage", HttpResponse::new(429, "rate limited"));

    let output = run(
        &["usage", "--format", "json", "--source", "auto"],
        &base_options(tmp.path())
            .with_path_prepend(&bin_dir)
            .with_env("CLAUDE_CONFIG_DIR", &path_str(&config_dir))
            .with_env(
                "CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN",
                "secret-token-api-error",
            )
            .with_env(
                "CLAUDE_PROMPT_SEGMENT_ENDPOINT",
                &format!("{}/usage", server.url()),
            )
            .with_env("CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_DISABLED", "1"),
    );

    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["result"]["reason_code"], "rate_limited");
    assert!(!stdout(&output).contains("organization_disabled"));
}

#[test]
fn usage_cache_source_outputs_epoch_for_rfc3339_reset_times() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_file = write_cache(
        tmp.path(),
        &usage_json_with_resets(21.0, 10.0, "2026-07-14T07:20:00Z", "2026-07-18T13:00:00Z"),
    );
    let updated_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_secs();
    set_modified(&cache_file, UNIX_EPOCH + Duration::from_secs(updated_at));

    let output = run(
        &["usage", "--format", "json", "--source", "cache"],
        &base_options(tmp.path()),
    );

    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "claude-cli.usage.v1");
    assert_eq!(payload["command"], "usage");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["updated_at"], updated_at);
    assert_eq!(
        payload["result"]["windows"][0]["resets_at"],
        "2026-07-14T07:20:00Z"
    );
    assert_eq!(
        payload["result"]["windows"][0]["resets_at_epoch"],
        1_784_013_600
    );
    assert_eq!(
        payload["result"]["windows"][1]["resets_at"],
        "2026-07-18T13:00:00Z"
    );
    assert_eq!(
        payload["result"]["windows"][1]["resets_at_epoch"],
        1_784_379_600
    );
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
    // The stale render enqueues a detached refresh whose fetch fails. Its
    // `.refresh.at` write lands after this test body returns unless we wait, and
    // `write_atomic` re-creates `tmp` on the way.
    assert_eq!(wait_for_requests(&server, 1).len(), 1);
    assert!(
        wait_for_refresh_lock_release(&cache_file, Duration::from_secs(4)),
        "detached refresh still held the lock at teardown"
    );
}

#[test]
fn prompt_segment_does_not_render_cache_older_than_max_stale_age() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_file = write_cache(tmp.path(), &usage_json(25.0, 50.0));
    set_modified(&cache_file, SystemTime::now() - Duration::from_secs(601));
    // The second uncontained spawner, which the first pass classified wrongly: a
    // 601s-old cache is display-expired, so this run spawns a detached child that
    // can recreate `tmp` after teardown. Its subject is max-stale rendering, so
    // hold the cooldown.
    //
    // This one is the falsifiable case: disabling the gate makes the run spawn and
    // `assert_held` fails with its own message, so the containment is demonstrated
    // rather than assumed.
    let cooldown = RefreshCooldown::hold(tmp.path());

    let output = run(
        &["prompt-segment", "--ttl", "1h", "--time-format", "%Y-%m-%d"],
        &base_options(tmp.path())
            .with_env("NO_COLOR", "1")
            .with_env(cooldown.env().0, cooldown.env().1),
    );

    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "");
    assert!(
        cache_file.is_file(),
        "max-stale handling must not delete cache"
    );
    cooldown.assert_held();
}

#[test]
fn prompt_segment_expired_cache_failed_refresh_observes_cooldown() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_file = write_cache(tmp.path(), &usage_json(25.0, 50.0));
    set_modified(&cache_file, SystemTime::now() - Duration::from_secs(601));

    let server = LoopbackServer::new().expect("server");
    server.add_route("GET", "/usage", HttpResponse::new(500, "upstream exploded"));
    let options = base_options(tmp.path())
        .with_env(
            "CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN",
            "secret-token-cooldown",
        )
        .with_env(
            "CLAUDE_PROMPT_SEGMENT_ENDPOINT",
            &format!("{}/usage", server.url()),
        );

    for _ in 0..2 {
        let output = run(&["prompt-segment", "--ttl", "1h"], &options);
        assert_exit(&output, 0);
        assert_eq!(stdout(&output), "");
        assert_eq!(stderr(&output), "");
    }

    assert_eq!(wait_for_requests(&server, 1).len(), 1);
    // The observed request proves the detached child holds the refresh lock; it
    // still writes the `.refresh.at` marker before releasing, which would
    // re-create `tmp` after teardown.
    assert!(
        wait_for_refresh_lock_release(&cache_file, Duration::from_secs(4)),
        "detached refresh still held the lock at teardown"
    );
}

#[test]
fn prompt_segment_expired_cache_concurrent_refreshes_are_coalesced() {
    const WORKERS: usize = 6;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_file = write_cache(tmp.path(), &usage_json(25.0, 50.0));
    set_modified(&cache_file, SystemTime::now() - Duration::from_secs(601));

    let server = TestServer::new(|_| {
        thread::sleep(Duration::from_millis(400));
        HttpResponse::new(500, "upstream exploded")
    })
    .expect("server");
    let shim_dir = tmp.path().join("refresh-shim");
    std::fs::create_dir_all(&shim_dir).expect("shim dir");
    let shim = shim_dir.join("claude-cli-refresh");
    let launch_log = tmp.path().join("refresh-launches.log");
    nils_test_support::write_exe(
        &shim_dir,
        "claude-cli-refresh",
        "#!/bin/sh\nprintf '%s\\n' launch >> \"$CLAUDE_TEST_REFRESH_LAUNCH_LOG\"\nexec \"$CLAUDE_TEST_REAL_EXE\" \"$@\"\n",
    );
    let options = base_options(tmp.path())
        .with_env(
            "CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN",
            "secret-token-coalesced",
        )
        .with_env(
            "CLAUDE_PROMPT_SEGMENT_ENDPOINT",
            &format!("{}/usage", server.url()),
        )
        .with_env("CLAUDE_PROMPT_SEGMENT_EXE", &path_str(&shim))
        .with_env("CLAUDE_TEST_REAL_EXE", &path_str(&claude_cli_bin()))
        .with_env("CLAUDE_TEST_REFRESH_LAUNCH_LOG", &path_str(&launch_log));
    let barrier = Arc::new(Barrier::new(WORKERS));

    let handles = (0..WORKERS)
        .map(|_| {
            let options = options.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                run(&["prompt-segment", "--ttl", "1h"], &options)
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        let output = handle.join().expect("prompt worker");
        assert_exit(&output, 0);
        assert_eq!(stdout(&output), "");
        assert_eq!(stderr(&output), "");
    }

    assert_eq!(wait_for_test_requests(&server, 1).len(), 1);
    assert_eq!(
        std::fs::read_to_string(launch_log)
            .expect("refresh launch log")
            .lines()
            .count(),
        1
    );
    assert!(
        wait_for_refresh_lock_release(&cache_file, Duration::from_secs(4)),
        "coalesced refresh still held the lock at teardown"
    );
}

#[test]
fn prompt_segment_explicit_refresh_bypasses_expired_cache_cooldown() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_file = write_cache(tmp.path(), &usage_json(25.0, 50.0));
    set_modified(&cache_file, SystemTime::now() - Duration::from_secs(601));

    let server = LoopbackServer::new().expect("server");
    server.add_route("GET", "/usage", HttpResponse::new(500, "upstream exploded"));
    let options = base_options(tmp.path())
        .with_env(
            "CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN",
            "secret-token-explicit",
        )
        .with_env(
            "CLAUDE_PROMPT_SEGMENT_ENDPOINT",
            &format!("{}/usage", server.url()),
        );

    for _ in 0..2 {
        let output = run(&["prompt-segment", "--refresh"], &options);
        assert_exit(&output, 0);
        assert_eq!(stdout(&output), "");
        assert_eq!(stderr(&output), "");
    }

    assert_eq!(server.take_requests().len(), 2);
}

#[test]
fn prompt_segment_status_reports_expired_cache_without_rendering() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_file = write_cache(tmp.path(), &usage_json(25.0, 50.0));
    set_modified(&cache_file, SystemTime::now() - Duration::from_secs(601));

    let output = run(
        &["prompt-segment", "status", "--format", "json"],
        &base_options(tmp.path())
            .with_env("CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN", "secret-token-status"),
    );

    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["result"]["cache_exists"], true);
    assert_eq!(payload["result"]["cache_stale"], true);
    assert_eq!(payload["result"]["would_render"], false);
    assert_eq!(payload["result"]["reason"], "cache-expired");
    assert!(cache_file.is_file(), "status must not delete expired cache");
}

#[test]
fn usage_cache_source_omits_windows_older_than_max_stale_age() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_file = write_cache(tmp.path(), &usage_json(25.0, 50.0));
    set_modified(&cache_file, SystemTime::now() - Duration::from_secs(601));

    let output = run(
        &["usage", "--format", "json", "--source", "cache"],
        &base_options(tmp.path()),
    );

    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["result"]["windows"], serde_json::json!([]));
    assert!(
        cache_file.is_file(),
        "max-stale handling must not delete cache"
    );
}

#[cfg(unix)]
#[test]
fn usage_auto_keeps_live_rate_limit_reason_when_expired_cache_is_omitted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_file = write_cache(tmp.path(), &usage_json(25.0, 50.0));
    set_modified(&cache_file, SystemTime::now() - Duration::from_secs(601));
    let bin_dir = write_fake_claude(
        tmp.path(),
        "#!/usr/bin/env sh\ncat >/dev/null\nprintf '%s\\n' 'usage unavailable'\n",
    );
    let server = LoopbackServer::new().expect("server");
    server.add_route("GET", "/usage", HttpResponse::new(429, "rate limited"));

    let output = run(
        &["usage", "--format", "json", "--source", "auto"],
        &base_options(tmp.path())
            .with_path_prepend(&bin_dir)
            .with_env("CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN", "secret-token")
            .with_env(
                "CLAUDE_PROMPT_SEGMENT_ENDPOINT",
                &format!("{}/usage", server.url()),
            )
            .with_env("CLAUDE_PROMPT_SEGMENT_CLAUDE_PTY_DISABLED", "1"),
    );

    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["result"]["windows"], serde_json::json!([]));
    assert_eq!(payload["result"]["reason_code"], "rate_limited");
    assert!(
        cache_file.is_file(),
        "max-stale handling must not delete cache"
    );
}

/// Pins the containment defaults, asserting the **effective** value rather than
/// mere presence.
///
/// `run_impl_os` replays `envs` in order through `Command::env`, whose map is
/// last-write-wins per key, and applies `envs_os` after `envs`. So a later entry
/// for the same key decides what the child sees. An `any()` assertion would stay
/// green if a future edit appended a live endpoint after the pin — exactly the
/// regression this guards. `with_path_prepend` in `nils-test-support` reads the
/// effective value with `rev().find(..)` for the same reason.
#[test]
fn base_options_pins_containment_defaults_as_the_effective_values() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let options = base_options(tmp.path());

    let effective = |key: &str| {
        assert!(
            !options.envs_os.iter().any(|(name, _)| name == key),
            "{key} must not also be set through envs_os, which is applied last"
        );
        options
            .envs
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    };

    assert_eq!(
        effective("CLAUDE_PROMPT_SEGMENT_ENDPOINT"),
        Some(UNROUTABLE_ENDPOINT),
        "base_options must pin the endpoint: it clears the CLAUDE_PROMPT prefix, and the \
         production default is a real host, so omission means reaching it; envs={:?}",
        options.envs
    );
    assert_eq!(
        effective("CLAUDE_PROMPT_SEGMENT_MAX_TIME_SECONDS"),
        Some(FAST_FAIL_MAX_TIME_SECONDS),
        "the fast-fail must be bounded so the pinned endpoint fails fast on a host that \
         drops rather than refuses loopback traffic; envs={:?}",
        options.envs
    );
}

#[test]
fn prompt_segment_missing_credentials_and_cache_is_quiet_success() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Per-test enumeration (#1412) showed this test reaches
    // `enqueue_background_refresh` despite having neither credentials nor a
    // cache, with no containment at all. Its subject is the quiet-success
    // rendering, not the refresh, so hold the cooldown.
    //
    // Measured: with the cooldown held this run enters `enqueue_background_refresh`
    // and spawns nothing. Unlike the max-stale case below, disabling the gate does
    // not make it spawn either — holding the cooldown also creates the cache
    // directory, which changes the rest of the path — so `assert_held` here is a
    // tripwire that cannot currently fire rather than a proof. Kept because it
    // costs nothing and would catch the marker being rewritten if that changes.
    let cooldown = RefreshCooldown::hold(tmp.path());
    let options = base_options(tmp.path()).with_env(cooldown.env().0, cooldown.env().1);

    let output = run(&["prompt-segment"], &options);

    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "");
    assert_eq!(stderr(&output), "");
    cooldown.assert_held();
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

fn write_claude_session(config_dir: &Path, project: &str, id: &str, cwd: &Path) {
    let dir = config_dir.join("projects").join(project);
    std::fs::create_dir_all(&dir).expect("project dir");
    let line = format!(
        "{{\"sessionId\":\"{id}\",\"cwd\":\"{}\"}}\n",
        cwd.to_string_lossy()
    );
    std::fs::write(dir.join(format!("{id}.jsonl")), line).expect("transcript");
}

#[test]
fn agent_resume_control_char_id_is_usage_error() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let options = CmdOptions::default().with_cwd(tmp.path());
    let output = run(&["agent", "resume", "bad\tid"], &options);
    assert_exit(&output, 64);
}

#[test]
fn agent_resume_unknown_id_returns_data_error() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let config = tmp.path().join("claude-config");
    std::fs::create_dir_all(config.join("projects")).expect("projects");
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");

    let options = CmdOptions::default()
        .with_cwd(&elsewhere)
        .with_env("CLAUDE_CONFIG_DIR", &path_str(&config));
    let output = run(&["agent", "resume", "absent-id"], &options);

    assert_exit(&output, 65);
    assert!(stderr(&output).contains("no Claude session history"));
}

#[test]
fn agent_resume_launches_claude_in_recorded_cwd_from_unrelated_dir() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    // A recorded cwd with a space exercises exact working-directory handling.
    let repo = tmp.path().join("recorded repo");
    std::fs::create_dir_all(&repo).expect("repo");
    let config = tmp.path().join("claude-config");
    write_claude_session(&config, "-recorded-repo", "cl-x", &repo);
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");

    let out_log = tmp.path().join("claude-out.txt");
    let stub_dir = tmp.path().join("stub");
    std::fs::create_dir_all(&stub_dir).expect("stub dir");
    // The stub drops a marker in its own working directory (proving it launched
    // in the recorded cwd) and records its argv to an absolute log path.
    nils_test_support::write_exe(
        &stub_dir,
        "claude",
        &format!(
            "#!/bin/sh\n: > launched-here\nprintf 'ARG:%s\\n' \"$@\" > '{}'\nexit 5\n",
            out_log.display()
        ),
    );

    let options = CmdOptions::default()
        .with_cwd(&elsewhere)
        .with_env("CLAUDE_CONFIG_DIR", &path_str(&config))
        .with_path_prepend(&stub_dir);
    let output = run(&["agent", "resume", "cl-x"], &options);

    assert_exit(&output, 5);
    assert!(
        repo.join("launched-here").exists(),
        "expected claude to be launched in the recorded cwd"
    );
    let logged = std::fs::read_to_string(&out_log).expect("stub log");
    assert_eq!(
        logged.lines().collect::<Vec<_>>(),
        vec!["ARG:--resume", "ARG:cl-x"],
        "claude must be launched with exactly `--resume <id>`"
    );
}

#[test]
fn agent_resume_cd_override_to_missing_directory_is_runtime_error() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let missing = tmp.path().join("does-not-exist");
    let options = CmdOptions::default().with_cwd(tmp.path());
    let output = run(
        &["agent", "resume", "any-id", "--cd", &path_str(&missing)],
        &options,
    );

    assert_exit(&output, 1);
    assert!(stderr(&output).contains("not an existing directory"));
}

#[test]
fn agent_resume_truncated_scan_returns_runtime_error() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    let config = tmp.path().join("claude-config");
    // Two matching projects plus a one-entry budget forces the bounded scan to
    // truncate before it can decide, without ever launching claude.
    write_claude_session(&config, "-proj-a", "trunc-id", &repo);
    write_claude_session(&config, "-proj-b", "trunc-id", &repo);

    let options = CmdOptions::default()
        .with_cwd(tmp.path())
        .with_env("CLAUDE_CONFIG_DIR", &path_str(&config))
        .with_env("AGENT_SESSION_CLAUDE_RESUME_SCAN_MAX_ENTRIES", "1");
    let output = run(&["agent", "resume", "trunc-id"], &options);

    assert_exit(&output, 1);
    assert!(stderr(&output).contains("truncated"));
}

#[test]
fn agent_resume_cd_override_bypasses_resolution() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let override_dir = tmp.path().join("override target");
    std::fs::create_dir_all(&override_dir).expect("override dir");
    // Empty project history: automatic resolution would fail with NotFound, so a
    // successful launch proves `--cd` bypassed resolution entirely.
    let config = tmp.path().join("claude-config");
    std::fs::create_dir_all(config.join("projects")).expect("projects");

    let stub_dir = tmp.path().join("stub");
    std::fs::create_dir_all(&stub_dir).expect("stub dir");
    nils_test_support::write_exe(
        &stub_dir,
        "claude",
        "#!/bin/sh\n: > launched-here\nexit 5\n",
    );

    let options = CmdOptions::default()
        .with_cwd(tmp.path())
        .with_env("CLAUDE_CONFIG_DIR", &path_str(&config))
        .with_path_prepend(&stub_dir);
    let output = run(
        &[
            "agent",
            "resume",
            "unresolved-id",
            "--cd",
            &path_str(&override_dir),
        ],
        &options,
    );

    assert_exit(&output, 5);
    assert!(
        override_dir.join("launched-here").exists(),
        "expected claude to be launched in the --cd override directory"
    );
}

#[cfg(unix)]
#[test]
fn agent_commit_uses_bounded_structured_output_and_semantic_commit_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    std::fs::write(repo.join("change.txt"), "change\n").expect("change");
    test_git::git(&repo, &["add", "change.txt"]);
    let old_head = test_git::git(&repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();

    let claude_argv = tmp.path().join("claude-argv.log");
    let claude_stdin = tmp.path().join("claude-stdin.log");
    let semantic_log = tmp.path().join("semantic.log");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --json-schema --safe-mode --strict-mcp-config --no-session-persistence --permission-mode --disable-slash-commands --no-chrome --tools --append-system-prompt --model --effort'
  exit 0
fi
: > "$CLAUDE_TEST_ARGV_LOG"
printf 'CWD:%s\n' "$PWD" >> "$CLAUDE_TEST_ARGV_LOG"
for arg in "$@"; do printf 'ARG:%s\n' "$arg" >> "$CLAUDE_TEST_ARGV_LOG"; done
cat > "$CLAUDE_TEST_STDIN_LOG"
printf '%s\n' '[{"type":"result","subtype":"success","is_error":false,"structured_output":{"type":"fix","scope":"agent","subject":"add safe commit workflow","body_bullets":["Keep the index staged on failure"]}}]'
"#,
    );
    nils_test_support::write_exe(
        &bin_dir,
        "semantic-commit",
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$SEMANTIC_TEST_LOG"
if [ "${1:-}" = "staged-context" ]; then
  printf '%s\n' 'STAGED BUNDLE'
  exit 0
fi
repo=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '--repo' ]; then repo="$arg"; fi
  previous="$arg"
done
"$REAL_GIT" -C "$repo" commit -m 'fix(agent): add safe commit workflow' >/dev/null
"#,
    );
    let real_git = nils_common::process::find_in_path("git").expect("git");
    let output = run(
        &[
            "agent", "commit", "--model", "sonnet", "--effort", "high", "prefer", "a", "small",
            "scope",
        ],
        &base_options(tmp.path())
            .with_cwd(&repo)
            .with_path_prepend(&bin_dir)
            .with_env("CLAUDE_TEST_ARGV_LOG", &path_str(&claude_argv))
            .with_env("CLAUDE_TEST_STDIN_LOG", &path_str(&claude_stdin))
            .with_env("SEMANTIC_TEST_LOG", &path_str(&semantic_log))
            .with_env("REAL_GIT", &path_str(&real_git)),
    );

    assert_exit(&output, 0);
    assert_ne!(
        test_git::git(&repo, &["rev-parse", "HEAD"]).trim(),
        old_head
    );
    let argv = std::fs::read_to_string(claude_argv).expect("claude argv");
    assert!(argv.contains("ARG:--json-schema\n"));
    assert!(argv.contains("ARG:--safe-mode\n"));
    assert!(argv.contains("ARG:--strict-mcp-config\n"));
    assert!(argv.contains("ARG:--no-session-persistence\n"));
    assert!(argv.contains("ARG:--tools\nARG:\n"));
    assert!(!argv.contains("Bash"));
    assert!(!argv.contains(&format!("CWD:{}", repo.display())));
    let prompt = std::fs::read_to_string(claude_stdin).expect("claude stdin");
    assert!(prompt.contains("STAGED BUNDLE"));
    assert!(prompt.contains("prefer a small scope"));
    let semantic = std::fs::read_to_string(semantic_log).expect("semantic log");
    assert!(semantic.contains("staged-context --format bundle --repo"));
    assert!(semantic.contains(
        "commit --type fix --scope agent --subject add safe commit workflow \
--body-bullet Keep the index staged on failure"
    ));
    assert!(semantic.contains(&format!("--expect-head {old_head}")));
    assert!(semantic.contains("--automation"));
}

#[cfg(unix)]
#[test]
fn agent_commit_rejects_index_drift_and_leaves_changes_staged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    std::fs::write(repo.join("change.txt"), "original\n").expect("change");
    test_git::git(&repo, &["add", "change.txt"]);
    let old_head = test_git::git(&repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();

    let semantic_log = tmp.path().join("semantic.log");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --json-schema --safe-mode --strict-mcp-config --no-session-persistence --permission-mode --disable-slash-commands --no-chrome --tools --append-system-prompt --model --effort'
  exit 0
fi
printf '%s\n' 'mutated during model call' > "$CLAUDE_TEST_REPO/change.txt"
"$REAL_GIT" -C "$CLAUDE_TEST_REPO" add change.txt
cat >/dev/null
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"structured_output":{"type":"fix","scope":null,"subject":"must not commit drift","body_bullets":[]}}'
"#,
    );
    nils_test_support::write_exe(
        &bin_dir,
        "semantic-commit",
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$SEMANTIC_TEST_LOG"
if [ "${1:-}" = "staged-context" ]; then
  printf '%s\n' 'STAGED BUNDLE'
  exit 0
fi
exit 97
"#,
    );
    let real_git = nils_common::process::find_in_path("git").expect("git");
    let output = run(
        &["agent", "commit"],
        &base_options(tmp.path())
            .with_cwd(&repo)
            .with_path_prepend(&bin_dir)
            .with_env("SEMANTIC_TEST_LOG", &path_str(&semantic_log))
            .with_env("CLAUDE_TEST_REPO", &path_str(&repo))
            .with_env("REAL_GIT", &path_str(&real_git)),
    );

    assert_exit(&output, 1);
    assert!(
        stderr(&output).contains("repository changed during message generation"),
        "stderr: {}",
        stderr(&output)
    );
    assert_eq!(
        test_git::git(&repo, &["rev-parse", "HEAD"]).trim(),
        old_head
    );
    assert!(
        test_git::git(&repo, &["diff", "--cached", "--name-only"])
            .lines()
            .any(|line| line == "change.txt")
    );
    let semantic = std::fs::read_to_string(semantic_log).expect("semantic log");
    assert_eq!(
        semantic
            .lines()
            .filter(|line| line.starts_with("commit "))
            .count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn agent_doctor_reports_secret_free_bounded_readiness_without_a_model_call() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let invoked = tmp.path().join("doctor.log");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --json-schema --safe-mode --strict-mcp-config --no-session-persistence --permission-mode --disable-slash-commands --no-chrome --tools --append-system-prompt --model --effort'
  exit 0
fi
if [ "${1:-}" = "doctor" ]; then
  printf '%s\n' 'private@example.com secret-token /private/path'
  printf '%s\n' 'private-stderr-token' >&2
  printf '%s\n' doctor >> "$CLAUDE_TEST_DOCTOR_LOG"
  exit 0
fi
printf '%s\n' model-call >> "$CLAUDE_TEST_DOCTOR_LOG"
exit 91
"#,
    );
    nils_test_support::write_exe(
        &bin_dir,
        "semantic-commit",
        r#"#!/bin/sh
set -eu
case "$*" in
  "staged-context --help")
    printf '%s\n' '  --format <mode>' '  --repo <path>'
    ;;
  "commit --help")
    printf '%s\n' \
      '  --type <type>' \
      '  --scope <scope>' \
      '  --subject <subject>' \
      '  --body-bullet <text>' \
      '  --expect-head <rev>' \
      '  --repo <path>' \
      '  --summary <mode>' \
      '  --automation'
    ;;
  *) exit 91 ;;
esac
"#,
    );
    let output = run(
        &["agent", "doctor", "--format", "json"],
        &base_options(tmp.path())
            .with_path_prepend(&bin_dir)
            .with_env("CLAUDE_TEST_DOCTOR_LOG", &path_str(&invoked)),
    );

    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(stdout(&output).trim()).expect("doctor json");
    assert_eq!(payload["schema_version"], "claude-cli.agent.doctor.v1");
    assert_eq!(payload["command"], "agent doctor");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["ready"], true);
    assert_eq!(payload["result"]["dependencies"]["claude"], true);
    assert_eq!(payload["result"]["dependencies"]["git"], true);
    assert_eq!(payload["result"]["dependencies"]["semantic_commit"], true);
    assert_eq!(
        payload["result"]["dependencies"]["semantic_commit_compatible"],
        true
    );
    assert_eq!(payload["result"]["upstream_doctor"], true);
    assert_eq!(payload["result"]["commit_profile"], true);
    assert_eq!(payload["result"]["configured_commit_profile"], true);
    assert_eq!(payload["result"]["flags"]["--model"], true);
    assert_eq!(payload["result"]["flags"]["--effort"], true);
    assert_eq!(
        std::fs::read_to_string(invoked).expect("doctor log"),
        "doctor\n"
    );
    for secret in [
        "private@example.com",
        "secret-token",
        "/private/path",
        "private-stderr-token",
    ] {
        assert!(!stdout(&output).contains(secret));
        assert!(!stderr(&output).contains(secret));
    }
}

#[cfg(unix)]
#[test]
fn agent_commit_auto_stages_and_pushes_the_verified_commit_to_its_captured_upstream() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let remote = tmp.path().join("remote.git");
    init_git_repo(&repo);
    test_git::git(&repo, &["branch", "-M", "agent-e2e"]);
    test_git::git(
        tmp.path(),
        &["init", "--bare", remote.to_str().expect("remote path")],
    );
    test_git::git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("remote path"),
        ],
    );
    test_git::git(&repo, &["push", "-u", "origin", "agent-e2e"]);
    std::fs::write(repo.join("auto-stage.txt"), "auto-stage and push\n").expect("change");
    let old_head = test_git::git(&repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let bin_dir = write_agent_commit_success_tools(tmp.path());
    let real_git = nils_common::process::find_in_path("git").expect("git");

    let output = run(
        &["agent", "commit", "--auto-stage", "--push"],
        &base_options(tmp.path())
            .with_cwd(&repo)
            .with_path_prepend(&bin_dir)
            .with_env("REAL_GIT", &path_str(&real_git)),
    );

    assert_exit(&output, 0);
    let new_head = test_git::git(&repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_ne!(new_head, old_head);
    assert_eq!(
        test_git::git(
            tmp.path(),
            &[
                "--git-dir",
                remote.to_str().expect("remote path"),
                "rev-parse",
                "refs/heads/agent-e2e",
            ],
        )
        .trim(),
        new_head
    );
    assert!(
        test_git::git(&repo, &["status", "--porcelain"])
            .trim()
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn agent_commit_refuses_push_when_the_captured_endpoint_is_retargeted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let original = tmp.path().join("original.git");
    let alternate = tmp.path().join("alternate.git");
    init_git_repo(&repo);
    test_git::git(&repo, &["branch", "-M", "agent-e2e"]);
    for remote in [&original, &alternate] {
        test_git::git(
            tmp.path(),
            &["init", "--bare", remote.to_str().expect("remote path")],
        );
    }
    test_git::git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            original.to_str().expect("original path"),
        ],
    );
    test_git::git(&repo, &["push", "-u", "origin", "agent-e2e"]);
    std::fs::write(repo.join("retarget.txt"), "do not retarget push\n").expect("change");
    test_git::git(&repo, &["add", "retarget.txt"]);
    let old_head = test_git::git(&repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let bin_dir = write_agent_commit_success_tools(tmp.path());
    let real_git = nils_common::process::find_in_path("git").expect("git");

    let output = run(
        &["agent", "commit", "--push"],
        &base_options(tmp.path())
            .with_cwd(&repo)
            .with_path_prepend(&bin_dir)
            .with_env("REAL_GIT", &path_str(&real_git))
            .with_env(
                "SEMANTIC_TEST_RETARGET_URL",
                alternate.to_str().expect("alternate path"),
            ),
    );

    assert_exit(&output, 1);
    assert!(
        stderr(&output).contains("push endpoint changed before push"),
        "stderr: {}",
        stderr(&output)
    );
    let new_head = test_git::git(&repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_ne!(new_head, old_head);
    assert_eq!(
        test_git::git(
            tmp.path(),
            &[
                "--git-dir",
                original.to_str().expect("original path"),
                "rev-parse",
                "refs/heads/agent-e2e",
            ],
        )
        .trim(),
        old_head
    );
    let alternate_head = test_git::git_output(
        tmp.path(),
        &[
            "--git-dir",
            alternate.to_str().expect("alternate path"),
            "rev-parse",
            "--verify",
            "refs/heads/agent-e2e",
        ],
    );
    assert!(!alternate_head.status.success());
}

#[cfg(unix)]
#[test]
fn agent_commit_pins_the_captured_endpoint_against_chained_url_rewrites() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let original = tmp.path().join("original.git");
    let alternate = tmp.path().join("alternate.git");
    init_git_repo(&repo);
    test_git::git(&repo, &["branch", "-M", "agent-e2e"]);
    for remote in [&original, &alternate] {
        test_git::git(
            tmp.path(),
            &["init", "--bare", remote.to_str().expect("remote path")],
        );
    }
    test_git::git(
        &repo,
        &[
            "push",
            original.to_str().expect("original path"),
            "agent-e2e:refs/heads/agent-e2e",
        ],
    );
    test_git::git(&repo, &["remote", "add", "origin", "seed:"]);
    test_git::git(&repo, &["config", "branch.agent-e2e.remote", "origin"]);
    test_git::git(
        &repo,
        &["config", "branch.agent-e2e.merge", "refs/heads/agent-e2e"],
    );
    test_git::git(
        &repo,
        &[
            "config",
            &format!(
                "url.{}.pushInsteadOf",
                original.to_str().expect("original path")
            ),
            "seed:",
        ],
    );
    test_git::git(
        &repo,
        &[
            "config",
            &format!(
                "url.{}.insteadOf",
                alternate.to_str().expect("alternate path")
            ),
            original.to_str().expect("original path"),
        ],
    );
    assert_eq!(
        test_git::git(&repo, &["remote", "get-url", "--push", "origin"]).trim(),
        original.to_str().expect("original path")
    );
    std::fs::write(repo.join("rewrite.txt"), "pin captured push endpoint\n").expect("change");
    test_git::git(&repo, &["add", "rewrite.txt"]);
    let bin_dir = write_agent_commit_success_tools(tmp.path());
    let real_git = nils_common::process::find_in_path("git").expect("git");

    let output = run(
        &["agent", "commit", "--push"],
        &base_options(tmp.path())
            .with_cwd(&repo)
            .with_path_prepend(&bin_dir)
            .with_env("REAL_GIT", &path_str(&real_git)),
    );

    assert_exit(&output, 0);
    let new_head = test_git::git(&repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_eq!(
        test_git::git(
            tmp.path(),
            &[
                "--git-dir",
                original.to_str().expect("original path"),
                "rev-parse",
                "refs/heads/agent-e2e",
            ],
        )
        .trim(),
        new_head
    );
    let alternate_head = test_git::git_output(
        tmp.path(),
        &[
            "--git-dir",
            alternate.to_str().expect("alternate path"),
            "rev-parse",
            "--verify",
            "refs/heads/agent-e2e",
        ],
    );
    assert!(!alternate_head.status.success());
}

#[cfg(unix)]
#[test]
fn agent_commit_preserves_the_local_commit_when_push_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    test_git::git(&repo, &["branch", "-M", "agent-e2e"]);
    let missing_remote = tmp.path().join("missing-remote.git");
    test_git::git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            missing_remote.to_str().expect("remote path"),
        ],
    );
    test_git::git(&repo, &["config", "branch.agent-e2e.remote", "origin"]);
    test_git::git(
        &repo,
        &["config", "branch.agent-e2e.merge", "refs/heads/agent-e2e"],
    );
    std::fs::write(repo.join("push-failure.txt"), "preserve local commit\n").expect("change");
    test_git::git(&repo, &["add", "push-failure.txt"]);
    let old_head = test_git::git(&repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let bin_dir = write_agent_commit_success_tools(tmp.path());
    let real_git = nils_common::process::find_in_path("git").expect("git");

    let output = run(
        &["agent", "commit", "--push"],
        &base_options(tmp.path())
            .with_cwd(&repo)
            .with_path_prepend(&bin_dir)
            .with_env("REAL_GIT", &path_str(&real_git)),
    );

    assert_ne!(output.code, 0);
    let new_head = test_git::git(&repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_ne!(new_head, old_head);
    assert!(
        stderr(&output).contains("push failed; local commit was preserved"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(
        test_git::git(&repo, &["show", "--pretty=", "--name-only", "HEAD"])
            .lines()
            .any(|line| line == "push-failure.txt")
    );
}

#[cfg(unix)]
#[test]
fn agent_commit_rejects_invalid_structured_message_and_preserves_index() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    std::fs::write(repo.join("change.txt"), "change\n").expect("change");
    test_git::git(&repo, &["add", "change.txt"]);
    let old_head = test_git::git(&repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let semantic_log = tmp.path().join("semantic.log");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --json-schema --safe-mode --strict-mcp-config --no-session-persistence --permission-mode --disable-slash-commands --no-chrome --tools --append-system-prompt'
  exit 0
fi
cat >/dev/null
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"structured_output":{"type":"feature","scope":null,"subject":"invalid type","body_bullets":[]}}'
"#,
    );
    nils_test_support::write_exe(
        &bin_dir,
        "semantic-commit",
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$SEMANTIC_TEST_LOG"
if [ "${1:-}" = "staged-context" ]; then
  printf '%s\n' 'STAGED BUNDLE'
  exit 0
fi
exit 97
"#,
    );
    let output = run(
        &["agent", "commit"],
        &base_options(tmp.path())
            .with_cwd(&repo)
            .with_path_prepend(&bin_dir)
            .with_env("SEMANTIC_TEST_LOG", &path_str(&semantic_log)),
    );

    assert_exit(&output, 65);
    assert!(stderr(&output).contains("invalid commit type"));
    assert!(!stdout(&output).contains("feature"));
    assert_eq!(
        test_git::git(&repo, &["rev-parse", "HEAD"]).trim(),
        old_head
    );
    assert!(
        test_git::git(&repo, &["diff", "--cached", "--name-only"])
            .lines()
            .any(|line| line == "change.txt")
    );
    let semantic = std::fs::read_to_string(semantic_log).expect("semantic log");
    assert_eq!(
        semantic
            .lines()
            .filter(|line| line.starts_with("commit "))
            .count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn agent_commit_rejects_secret_like_staged_context_before_claude_launch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    std::fs::write(repo.join("change.txt"), "change\n").expect("change");
    test_git::git(&repo, &["add", "change.txt"]);
    let launched = tmp.path().join("claude-launched");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --json-schema --safe-mode --strict-mcp-config --no-session-persistence --permission-mode --disable-slash-commands --no-chrome --tools --append-system-prompt'
  exit 0
fi
: > "$CLAUDE_TEST_LAUNCHED"
exit 91
"#,
    );
    nils_test_support::write_exe(
        &bin_dir,
        "semantic-commit",
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "staged-context" ]; then
  printf '%s\n' 'api_key=supersecretvalue123'
  exit 0
fi
exit 97
"#,
    );
    let output = run(
        &["agent", "commit"],
        &base_options(tmp.path())
            .with_cwd(&repo)
            .with_path_prepend(&bin_dir)
            .with_env("CLAUDE_TEST_LAUNCHED", &path_str(&launched)),
    );

    assert_exit(&output, 65);
    assert!(stderr(&output).contains("secret-like content"));
    assert!(stderr(&output).contains("generic-secret-kv"));
    assert!(!stderr(&output).contains("supersecretvalue123"));
    assert!(!launched.exists());
    assert!(
        test_git::git(&repo, &["diff", "--cached", "--name-only"])
            .lines()
            .any(|line| line == "change.txt")
    );
}

#[cfg(unix)]
#[test]
fn agent_commit_probes_optional_model_and_effort_capabilities() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    std::fs::write(repo.join("change.txt"), "change\n").expect("change");
    test_git::git(&repo, &["add", "change.txt"]);
    let launched = tmp.path().join("claude-launched");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --json-schema --safe-mode --strict-mcp-config --no-session-persistence --permission-mode --disable-slash-commands --no-chrome --tools --append-system-prompt --model'
  exit 0
fi
: > "$CLAUDE_TEST_LAUNCHED"
exit 91
"#,
    );
    nils_test_support::write_exe(
        &bin_dir,
        "semantic-commit",
        "#!/bin/sh\nprintf '%s\\n' 'STAGED BUNDLE'\n",
    );
    let output = run(
        &["agent", "commit", "--effort", "high"],
        &base_options(tmp.path())
            .with_cwd(&repo)
            .with_path_prepend(&bin_dir)
            .with_env("CLAUDE_TEST_LAUNCHED", &path_str(&launched)),
    );

    assert_exit(&output, 69);
    assert!(stderr(&output).contains("--effort"));
    assert!(!launched.exists());
}

#[cfg(unix)]
#[test]
fn agent_commit_rejects_created_commit_when_tree_does_not_match_snapshot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    std::fs::write(repo.join("change.txt"), "expected\n").expect("change");
    test_git::git(&repo, &["add", "change.txt"]);
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --json-schema --safe-mode --strict-mcp-config --no-session-persistence --permission-mode --disable-slash-commands --no-chrome --tools --append-system-prompt'
  exit 0
fi
cat >/dev/null
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"structured_output":{"type":"test","scope":"agent","subject":"verify commit tree","body_bullets":[]}}'
"#,
    );
    nils_test_support::write_exe(
        &bin_dir,
        "semantic-commit",
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "staged-context" ]; then
  printf '%s\n' 'STAGED BUNDLE'
  exit 0
fi
repo=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '--repo' ]; then repo="$arg"; fi
  previous="$arg"
done
printf '%s\n' 'unexpected' > "$repo/injected.txt"
"$REAL_GIT" -C "$repo" add injected.txt
"$REAL_GIT" -C "$repo" commit -m 'test(agent): verify commit tree' >/dev/null
"#,
    );
    let real_git = nils_common::process::find_in_path("git").expect("git");
    let output = run(
        &["agent", "commit"],
        &base_options(tmp.path())
            .with_cwd(&repo)
            .with_path_prepend(&bin_dir)
            .with_env("REAL_GIT", &path_str(&real_git)),
    );

    assert_exit(&output, 1);
    assert!(stderr(&output).contains("failed parent/tree integrity verification"));
    assert_eq!(
        test_git::git(&repo, &["show", "--pretty=", "--name-only", "HEAD"])
            .lines()
            .collect::<Vec<_>>(),
        vec!["change.txt", "injected.txt"]
    );
}

#[cfg(unix)]
#[test]
fn agent_doctor_requires_compatible_semantic_commit_surface() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --json-schema --safe-mode --strict-mcp-config --no-session-persistence --permission-mode --disable-slash-commands --no-chrome --tools --append-system-prompt --model --effort'
  exit 0
fi
if [ "${1:-}" = "doctor" ]; then exit 0; fi
exit 91
"#,
    );
    nils_test_support::write_exe(&bin_dir, "semantic-commit", "#!/bin/sh\nexit 0\n");
    let output = run(
        &["agent", "doctor", "--format", "json"],
        &base_options(tmp.path()).with_path_prepend(&bin_dir),
    );

    assert_exit(&output, 1);
    let payload: Value = serde_json::from_str(stdout(&output).trim()).expect("doctor json");
    assert_eq!(payload["result"]["dependencies"]["semantic_commit"], true);
    assert_eq!(
        payload["result"]["dependencies"]["semantic_commit_compatible"],
        false
    );
    assert_eq!(payload["result"]["ready"], false);
}

#[cfg(unix)]
#[test]
fn agent_doctor_bounds_upstream_output_and_reports_stable_failure() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --json-schema --safe-mode --strict-mcp-config --no-session-persistence --permission-mode --disable-slash-commands --no-chrome --tools --append-system-prompt'
  exit 0
fi
if [ "${1:-}" = "doctor" ]; then
  while :; do printf '%s\n' 'private-secret-growing-output'; done
fi
exit 91
"#,
    );
    nils_test_support::write_exe(&bin_dir, "semantic-commit", "#!/bin/sh\nexit 0\n");
    let started = Instant::now();
    let output = run(
        &["agent", "doctor", "--format", "json"],
        &base_options(tmp.path()).with_path_prepend(&bin_dir),
    );

    assert_exit(&output, 1);
    assert!(started.elapsed() < Duration::from_secs(3));
    let payload: Value = serde_json::from_str(stdout(&output).trim()).expect("doctor json");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["ready"], false);
    assert_eq!(payload["result"]["upstream_doctor"], false);
    assert_eq!(
        payload["result"]["upstream_doctor_status"],
        "output-too-large"
    );
    assert!(!stdout(&output).contains("private-secret-growing-output"));
    assert!(!stderr(&output).contains("private-secret-growing-output"));
}

#[cfg(unix)]
#[test]
fn agent_doctor_reports_configured_capability_and_upstream_failures() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --json-schema --safe-mode --strict-mcp-config --no-session-persistence --permission-mode --disable-slash-commands --no-chrome --tools --append-system-prompt --model'
  exit 0
fi
if [ "${1:-}" = "doctor" ]; then exit 7; fi
exit 91
"#,
    );
    nils_test_support::write_exe(
        &bin_dir,
        "semantic-commit",
        r#"#!/bin/sh
case "$*" in
  "staged-context --help") printf '%s\n' '  --format <mode>' '  --repo <path>' ;;
  "commit --help") printf '%s\n' '--type --scope --subject --body-bullet --expect-head --repo --summary --automation' ;;
  *) exit 91 ;;
esac
"#,
    );
    let output = run(
        &["agent", "doctor", "--format", "json"],
        &base_options(tmp.path())
            .with_path_prepend(&bin_dir)
            .with_env("CLAUDE_CLI_EFFORT", "high"),
    );

    assert_exit(&output, 1);
    let payload: Value = serde_json::from_str(stdout(&output).trim()).expect("doctor json");
    assert_eq!(payload["result"]["commit_profile"], true);
    assert_eq!(payload["result"]["configured_commit_profile"], false);
    assert_eq!(payload["result"]["flags"]["--effort"], false);
    assert_eq!(payload["result"]["upstream_doctor_status"], "failed");
    assert_eq!(payload["result"]["ready"], false);
}

#[test]
fn agent_doctor_reports_launch_failed_when_claude_is_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("missing-claude");
    let output = run(
        &["agent", "doctor", "--format", "json"],
        &base_options(tmp.path()).with_env("CLAUDE_CLI_BIN", &path_str(&missing)),
    );

    assert_exit(&output, 1);
    let payload: Value = serde_json::from_str(stdout(&output).trim()).expect("doctor json");
    assert_eq!(payload["result"]["dependencies"]["claude"], false);
    assert_eq!(payload["result"]["upstream_doctor_status"], "launch-failed");
    assert_eq!(payload["result"]["ready"], false);
}

#[cfg(unix)]
#[test]
fn agent_prompt_safe_runtime_probes_capabilities_and_keeps_input_out_of_argv() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let argv_log = tmp.path().join("argv.log");
    let stdin_log = tmp.path().join("stdin.log");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --safe-mode --strict-mcp-config --no-session-persistence --permission-mode --disable-slash-commands --no-chrome --tools --append-system-prompt'
  exit 0
fi
: > "$CLAUDE_TEST_ARGV_LOG"
for arg in "$@"; do
  printf '%s\n' "$arg" >> "$CLAUDE_TEST_ARGV_LOG"
done
cat > "$CLAUDE_TEST_STDIN_LOG"
printf '%s\n' 'safe model result'
"#,
    );
    let prompt = "--literal; $(touch should-not-run)";
    let output = run(
        &["agent", "prompt", prompt],
        &base_options(tmp.path())
            .with_path_prepend(&bin_dir)
            .with_env("CLAUDE_CLI_NO_SESSION_PERSISTENCE", "false")
            .with_env("CLAUDE_TEST_ARGV_LOG", &path_str(&argv_log))
            .with_env("CLAUDE_TEST_STDIN_LOG", &path_str(&stdin_log)),
    );

    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "safe model result\n");
    assert_eq!(stderr(&output), "");
    assert!(!tmp.path().join("should-not-run").exists());
    assert_eq!(
        std::fs::read_to_string(argv_log)
            .expect("argv log")
            .lines()
            .collect::<Vec<_>>(),
        vec![
            "--print",
            "--output-format",
            "text",
            "--safe-mode",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--permission-mode",
            "dontAsk",
            "--disable-slash-commands",
            "--no-chrome",
            "--tools",
            "Read,Glob,Grep",
        ]
    );
    assert_eq!(
        std::fs::read_to_string(stdin_log).expect("stdin log"),
        prompt
    );
}

#[cfg(unix)]
#[test]
fn agent_prompt_accepts_stdin_and_fails_closed_when_capability_is_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --safe-mode'
  exit 0
fi
exit 99
"#,
    );

    let output = run(
        &["agent", "prompt"],
        &base_options(tmp.path())
            .with_path_prepend(&bin_dir)
            .with_stdin_str("stdin prompt\n"),
    );

    assert_exit(&output, 69);
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).contains("missing required Claude capabilities"));
    assert!(!stderr(&output).contains("stdin prompt"));
}

#[cfg(unix)]
#[test]
fn agent_capability_probe_stops_unbounded_output_before_launch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let launched = tmp.path().join("launched");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
if [ "${1:-}" = "--help" ]; then
  while :; do printf '%s\n' 'unbounded help output'; done
fi
: > "$CLAUDE_TEST_LAUNCHED"
exit 99
"#,
    );
    let started = Instant::now();

    let output = run(
        &["agent", "prompt", "hello"],
        &base_options(tmp.path())
            .with_path_prepend(&bin_dir)
            .with_env("CLAUDE_TEST_LAUNCHED", &path_str(&launched)),
    );

    assert_exit(&output, 69);
    assert!(stderr(&output).contains("bounded `claude --help` output"));
    assert!(!launched.exists());
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[cfg(unix)]
#[test]
fn agent_advice_and_knowledge_share_safe_runtime_with_versioned_templates() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let argv_log = tmp.path().join("argv.log");
    let stdin_log = tmp.path().join("stdin.log");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --safe-mode --strict-mcp-config --no-session-persistence --permission-mode --disable-slash-commands --no-chrome --tools --append-system-prompt'
  exit 0
fi
: > "$CLAUDE_TEST_ARGV_LOG"
for arg in "$@"; do printf '%s\n' "$arg" >> "$CLAUDE_TEST_ARGV_LOG"; done
cat > "$CLAUDE_TEST_STDIN_LOG"
printf '%s\n' ok
"#,
    );
    let options = base_options(tmp.path())
        .with_path_prepend(&bin_dir)
        .with_env("CLAUDE_TEST_ARGV_LOG", &path_str(&argv_log))
        .with_env("CLAUDE_TEST_STDIN_LOG", &path_str(&stdin_log));

    let advice = run(&["agent", "advice", "review", "this"], &options);
    assert_exit(&advice, 0);
    let advice_argv = std::fs::read_to_string(&argv_log).expect("advice argv");
    assert!(advice_argv.contains("nils-claude-cli.agent-advice.v1"));
    assert!(!advice_argv.contains("review this"));
    assert_eq!(
        std::fs::read_to_string(&stdin_log).expect("advice stdin"),
        "review this"
    );

    let knowledge = run(&["agent", "knowledge", "borrow", "checker"], &options);
    assert_exit(&knowledge, 0);
    let knowledge_argv = std::fs::read_to_string(&argv_log).expect("knowledge argv");
    assert!(knowledge_argv.contains("nils-claude-cli.agent-knowledge.v1"));
    assert!(!knowledge_argv.contains("borrow checker"));
    assert!(knowledge_argv.lines().any(|line| line.is_empty()));
    assert_eq!(
        std::fs::read_to_string(&stdin_log).expect("knowledge stdin"),
        "borrow checker"
    );
}

#[cfg(unix)]
#[test]
fn agent_inherited_runtime_has_explicit_session_persistence_contract() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let argv_log = tmp.path().join("argv.log");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--print --output-format --safe-mode --strict-mcp-config --no-session-persistence --permission-mode --disable-slash-commands --no-chrome --tools'
  exit 0
fi
: > "$CLAUDE_TEST_ARGV_LOG"
for arg in "$@"; do printf '%s\n' "$arg" >> "$CLAUDE_TEST_ARGV_LOG"; done
cat >/dev/null
"#,
    );
    let base = base_options(tmp.path())
        .with_path_prepend(&bin_dir)
        .with_env("CLAUDE_TEST_ARGV_LOG", &path_str(&argv_log));

    let default = run(
        &["agent", "prompt", "--runtime", "inherited", "hello"],
        &base,
    );
    assert_exit(&default, 0);
    let default_argv = std::fs::read_to_string(&argv_log).expect("default argv");
    assert!(
        default_argv
            .lines()
            .any(|line| line == "--no-session-persistence")
    );
    assert!(!default_argv.lines().any(|line| line == "--safe-mode"));
    assert!(
        !default_argv
            .lines()
            .any(|line| line == "--strict-mcp-config")
    );

    let persistent = run(
        &["agent", "prompt", "--runtime", "inherited", "hello"],
        &base
            .clone()
            .with_env("CLAUDE_CLI_NO_SESSION_PERSISTENCE", "false"),
    );
    assert_exit(&persistent, 0);
    let persistent_argv = std::fs::read_to_string(&argv_log).expect("persistent argv");
    assert!(
        !persistent_argv
            .lines()
            .any(|line| line == "--no-session-persistence")
    );
    assert!(!persistent_argv.lines().any(|line| line == "--safe-mode"));
}

#[cfg(unix)]
#[test]
fn agent_rejects_oversized_stdin_before_launch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let launched = tmp.path().join("launched");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
: > "$CLAUDE_TEST_LAUNCHED"
exit 99
"#,
    );
    let oversized = "x".repeat(1024 * 1024 + 1);

    let output = run(
        &["agent", "prompt"],
        &base_options(tmp.path())
            .with_path_prepend(&bin_dir)
            .with_env("CLAUDE_TEST_LAUNCHED", &path_str(&launched))
            .with_stdin_str(&oversized),
    );

    assert_exit(&output, 65);
    assert!(stderr(&output).contains("1 MiB safety limit"));
    assert!(!launched.exists());
}

#[cfg(unix)]
#[test]
fn auth_status_wraps_public_fields_and_redacts_identity_and_tokens() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
if [ "$*" = "auth status --json" ]; then
  cat <<'JSON'
{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"private@example.com","orgId":"private-org-id","orgName":"Private Org","subscriptionType":"team","accessToken":"secret-token"}
JSON
  exit 0
fi
exit 98
"#,
    );

    let output = run(
        &["auth", "status", "--format", "json"],
        &base_options(tmp.path()).with_path_prepend(&bin_dir),
    );

    assert_exit(&output, 0);
    assert!(!stdout(&output).contains("private@example.com"));
    assert!(!stdout(&output).contains("private-org-id"));
    assert!(!stdout(&output).contains("Private Org"));
    assert!(!stdout(&output).contains("secret-token"));
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "claude-cli.auth.v1");
    assert_eq!(payload["command"], "auth status");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["logged_in"], true);
    assert_eq!(payload["result"]["auth_method"], "claude.ai");
    assert_eq!(payload["result"]["api_provider"], "firstParty");
    assert_eq!(payload["result"]["subscription_type"], "team");
}

#[cfg(unix)]
#[test]
fn auth_status_validates_upstream_shape_and_exit_semantics() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
case "${CLAUDE_TEST_AUTH_CASE:-}" in
  logged-out) printf '%s\n' '{"loggedIn":false}'; exit 1 ;;
  invalid-json) printf '%s\n' 'not json'; exit 0 ;;
  null) printf '%s\n' 'null'; exit 0 ;;
  missing) printf '%s\n' '{"authMethod":"claude.ai"}'; exit 0 ;;
  wrong-type) printf '%s\n' '{"loggedIn":"true"}'; exit 0 ;;
  unexpected-exit) printf '%s\n' '{"loggedIn":true}'; exit 2 ;;
  unexpected-empty) exit 2 ;;
  unexpected-diagnostic) printf '%s\n' 'upstream diagnostic'; exit 2 ;;
  inconsistent) printf '%s\n' '{"loggedIn":false}'; exit 0 ;;
  oversized) while :; do printf '%s\n' 'unbounded auth output'; done ;;
  timeout) sleep 30 ;;
esac
exit 99
"#,
    );
    let base = base_options(tmp.path()).with_path_prepend(&bin_dir);

    let logged_out = run(
        &["auth", "status", "--format", "json"],
        &base.clone().with_env("CLAUDE_TEST_AUTH_CASE", "logged-out"),
    );
    assert_exit(&logged_out, 1);
    let payload: Value = serde_json::from_str(&stdout(&logged_out)).expect("logged-out json");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["logged_in"], false);

    for (case, expected_exit, expected_error) in [
        ("invalid-json", 65, "invalid-upstream-output"),
        ("null", 65, "invalid-upstream-shape"),
        ("missing", 65, "invalid-upstream-shape"),
        ("wrong-type", 65, "invalid-upstream-shape"),
        ("unexpected-exit", 1, "unexpected-upstream-status"),
        ("unexpected-empty", 1, "unexpected-upstream-status"),
        ("unexpected-diagnostic", 1, "unexpected-upstream-status"),
        ("inconsistent", 65, "inconsistent-upstream-status"),
        ("oversized", 65, "output-too-large"),
    ] {
        let output = run(
            &["auth", "status", "--format", "json"],
            &base.clone().with_env("CLAUDE_TEST_AUTH_CASE", case),
        );
        assert_exit(&output, expected_exit);
        let payload: Value = serde_json::from_str(&stdout(&output)).expect("error json");
        assert_eq!(payload["ok"], false, "case: {case}");
        assert_eq!(payload["error"]["code"], expected_error, "case: {case}");
    }

    let started = Instant::now();
    let timed_out = run(
        &["auth", "status", "--format", "json"],
        &base.with_env("CLAUDE_TEST_AUTH_CASE", "timeout"),
    );
    assert_exit(&timed_out, 1);
    let payload: Value = serde_json::from_str(&stdout(&timed_out)).expect("timeout json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "upstream-timeout");
    assert!(started.elapsed() < Duration::from_secs(7));
}

#[cfg(unix)]
#[test]
fn auth_login_and_logout_delegate_exact_argv_and_propagate_status() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let argv_log = tmp.path().join("auth-argv.log");
    let bin_dir = write_fake_claude(
        tmp.path(),
        r#"#!/bin/sh
: > "$CLAUDE_TEST_ARGV_LOG"
for arg in "$@"; do printf '%s\n' "$arg" >> "$CLAUDE_TEST_ARGV_LOG"; done
case "$*" in
  "auth login --console --email operator@example.com") exit 7 ;;
  "auth logout") exit 8 ;;
esac
exit 99
"#,
    );
    let options = base_options(tmp.path())
        .with_path_prepend(&bin_dir)
        .with_env("CLAUDE_TEST_ARGV_LOG", &path_str(&argv_log));

    let login = run(
        &[
            "auth",
            "login",
            "--console",
            "--email",
            "operator@example.com",
        ],
        &options,
    );
    assert_exit(&login, 7);
    assert_eq!(
        std::fs::read_to_string(&argv_log).expect("login argv"),
        "auth\nlogin\n--console\n--email\noperator@example.com\n"
    );

    let logout = run(&["auth", "logout"], &options);
    assert_exit(&logout, 8);
    assert_eq!(
        std::fs::read_to_string(&argv_log).expect("logout argv"),
        "auth\nlogout\n"
    );
}

#[test]
fn config_show_and_set_are_secret_free_validated_shell_contracts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let options = base_options(tmp.path())
        .with_env("CLAUDE_CLI_MODEL", "sonnet")
        .with_env("CLAUDE_CLI_EFFORT", "high")
        .with_env("CLAUDE_CLI_AGENT_RUNTIME", "safe")
        .with_env("ANTHROPIC_API_KEY", "must-not-leak")
        .with_env("CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN", "also-secret");

    let show = run(&["config", "show"], &options);
    assert_exit(&show, 0);
    assert!(stdout(&show).contains("CLAUDE_CLI_MODEL=sonnet"));
    assert!(stdout(&show).contains("CLAUDE_CLI_EFFORT=high"));
    assert!(stdout(&show).contains("CLAUDE_CLI_AGENT_RUNTIME=safe"));
    assert!(!stdout(&show).contains("must-not-leak"));
    assert!(!stdout(&show).contains("also-secret"));

    let set = run(&["config", "set", "model", "name with ' quote"], &options);
    assert_exit(&set, 0);
    assert_eq!(
        stdout(&set),
        "export CLAUDE_CLI_MODEL='name with '\"'\"' quote'\n"
    );

    let invalid = run(&["config", "set", "effort", "unbounded"], &options);
    assert_exit(&invalid, 64);
    assert!(stderr(&invalid).contains("low|medium|high|xhigh|max"));

    let model_at_limit = "m".repeat(256);
    let accepted = run(
        &["config", "set", "model", &model_at_limit],
        &base_options(tmp.path()),
    );
    assert_exit(&accepted, 0);

    let model_over_limit = "m".repeat(257);
    let rejected = run(
        &["config", "set", "model", &model_over_limit],
        &base_options(tmp.path()),
    );
    assert_exit(&rejected, 64);
    assert!(stderr(&rejected).contains("at most 256 bytes"));

    let invalid_show = run(
        &["config", "show"],
        &base_options(tmp.path()).with_env("CLAUDE_CLI_EFFORT", "unbounded"),
    );
    assert_exit(&invalid_show, 64);
    assert_eq!(stdout(&invalid_show), "");
    assert!(stderr(&invalid_show).contains("low|medium|high|xhigh|max"));
}

#[test]
fn prompt_segment_render_flags_filter_window_show_timezone_and_escape_zsh_percent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_cache(tmp.path(), &usage_json(25.0, 50.0));
    let options = base_options(tmp.path())
        .with_env("NO_COLOR", "1")
        .with_env("TZ", "UTC")
        .with_env("CLAUDE_PROMPT_SEGMENT_ZSH_ESCAPE_ENABLED", "1");

    let output = run(&["prompt-segment", "--no-5h", "--show-timezone"], &options);

    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "W:50%% 01-03 12:30 +00:00\n");
}

#[test]
fn prompt_segment_stale_cache_returns_immediately_and_refreshes_in_background() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_file = write_cache(tmp.path(), &usage_json(25.0, 50.0));
    make_old(&cache_file);
    let refreshed_body = usage_json(10.0, 20.0);
    let response_body = refreshed_body.clone();
    let server = TestServer::new(move |_| {
        thread::sleep(Duration::from_millis(800));
        HttpResponse::new(200, response_body.clone())
            .with_header("Content-Type", "application/json")
    })
    .expect("server");
    let options = base_options(tmp.path())
        .with_env("CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN", "background-token")
        .with_env(
            "CLAUDE_PROMPT_SEGMENT_ENDPOINT",
            &format!("{}/usage", server.url()),
        )
        .with_env("NO_COLOR", "1")
        .with_env("TZ", "UTC");

    let started = Instant::now();
    let output = run(
        &["prompt-segment", "--ttl", "1s", "--time-format", "%Y-%m-%d"],
        &options,
    );
    let elapsed = started.elapsed();

    assert_exit(&output, 0);
    assert!(
        elapsed < Duration::from_millis(400),
        "cached prompt path blocked for {elapsed:?}"
    );
    assert_eq!(stdout(&output), "5h:75% W:50% 2026-01-03 (stale)\n");

    assert!(
        wait_for_background_refresh_settled(&cache_file, &refreshed_body, Duration::from_secs(4)),
        "background refresh did not update the cache"
    );
}

#[test]
fn prompt_segment_missing_cache_returns_immediately_then_renders_refreshed_cache() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_file = tmp.path().join("usage.json");
    let refreshed_body = usage_json(10.0, 20.0);
    let response_body = refreshed_body.clone();
    let server = TestServer::new(move |_| {
        thread::sleep(Duration::from_millis(800));
        HttpResponse::new(200, response_body.clone())
            .with_header("Content-Type", "application/json")
    })
    .expect("server");
    let options = base_options(tmp.path())
        .with_env("CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN", "missing-cache-token")
        .with_env(
            "CLAUDE_PROMPT_SEGMENT_ENDPOINT",
            &format!("{}/usage", server.url()),
        )
        .with_env("NO_COLOR", "1")
        .with_env("TZ", "UTC");

    let started = Instant::now();
    let output = run(
        &["prompt-segment", "--ttl", "1s", "--time-format", "%Y-%m-%d"],
        &options,
    );
    let elapsed = started.elapsed();

    assert_exit(&output, 0);
    assert!(
        elapsed < Duration::from_millis(400),
        "missing-cache prompt path blocked for {elapsed:?}"
    );
    assert_eq!(stdout(&output), "");
    assert_eq!(stderr(&output), "");

    assert!(
        wait_for_background_refresh_settled(&cache_file, &refreshed_body, Duration::from_secs(4)),
        "missing-cache background refresh did not create the cache"
    );
    let rendered = run(
        &["prompt-segment", "--ttl", "1h", "--time-format", "%Y-%m-%d"],
        &options,
    );
    assert_exit(&rendered, 0);
    assert_eq!(stdout(&rendered), "5h:90% W:80% 2026-01-03\n");
}

/// Pins the predicate that keeps the background-refresh fixtures leak-free.
///
/// Asserted against a synthetic lock rather than a real refresh: the real race
/// is won by the child on an idle host, so a test that waits for an actual
/// detached refresh passes with or without the fix and pins nothing.
#[test]
fn settled_wait_blocks_until_the_refresh_lock_is_released() {
    const HOLD: Duration = Duration::from_millis(300);

    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_file = write_cache(tmp.path(), "settled-body");
    let held = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(refresh_lock_path(&cache_file))
        .expect("hold refresh lock");
    // SAFETY: `flock` observes the valid descriptor owned by `held`.
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "test could not take the refresh lock"
    );

    // Measured *inside* the scope: `thread::scope` joins the releasing thread on
    // the way out, so an elapsed time taken after the scope would satisfy this
    // assertion even if the wait returned immediately.
    let started = Instant::now();
    let waited = thread::scope(|scope| {
        let held = &held;
        scope.spawn(move || {
            thread::sleep(HOLD);
            // SAFETY: `flock` observes the descriptor still owned by `held`.
            unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_UN) };
        });
        assert!(
            wait_for_background_refresh_settled(
                &cache_file,
                "settled-body",
                Duration::from_secs(5)
            ),
            "settled wait gave up while the lock was still held"
        );
        started.elapsed()
    });

    assert!(
        waited >= HOLD,
        "settled wait returned after {waited:?}, before the lock was released"
    );
}

/// A child that never releases must surface as a failure, not a silent pass.
#[test]
fn settled_wait_times_out_when_the_refresh_lock_is_never_released() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_file = write_cache(tmp.path(), "stuck-body");
    let held = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(refresh_lock_path(&cache_file))
        .expect("hold refresh lock");
    // SAFETY: `flock` observes the valid descriptor owned by `held`.
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "test could not take the refresh lock"
    );

    assert!(
        !wait_for_background_refresh_settled(&cache_file, "stuck-body", Duration::from_millis(200)),
        "settled wait reported success while the lock was still held"
    );
}
