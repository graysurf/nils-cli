use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use pretty_assertions::assert_eq;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn run(args: &[&str], envs: &[(&str, &Path)], vars: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default()
        // Stabilize output for tests regardless of user shell prompt environment.
        .with_env("NO_COLOR", "1")
        .with_env("TZ", "UTC")
        .with_env_remove("STARSHIP_SESSION_KEY")
        .with_env_remove("STARSHIP_SHELL");
    for (key, path) in envs {
        let value = path.to_string_lossy();
        options = options.with_env(key, value.as_ref());
    }
    for (key, value) in vars {
        options = options.with_env(key, value);
    }
    let bin = codex_cli_bin();
    cmd::run_with(&bin, args, &options)
}

fn stdout(output: &CmdOutput) -> String {
    output.stdout_text()
}

fn stderr(output: &CmdOutput) -> String {
    output.stderr_text()
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(output.code, code);
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

fn write_auth_and_secret(dir: &tempfile::TempDir) -> (PathBuf, PathBuf, PathBuf) {
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let payload_alpha = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlcl8xMjMiLCJlbWFpbCI6ImFscGhhQGV4YW1wbGUuY29tIn19";
    let hdr = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
    let token = format!("{hdr}.{payload_alpha}.sig");

    let secret_alpha = secrets.join("alpha.json");
    fs::write(
        &secret_alpha,
        format!(
            r#"{{
  "tokens": {{
    "access_token": "tok",
    "refresh_token": "refresh_token_value",
    "id_token": "{token}",
    "account_id": "acct_001"
  }},
  "last_refresh": "2025-01-20T12:34:56Z"
}}"#
        ),
    )
    .expect("write alpha secret");

    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, fs::read(&secret_alpha).expect("read alpha")).expect("write auth");

    (auth_file, secrets, cache_root)
}

fn write_prompt_segment_cache_kv(cache_root: &Path, key: &str, kv: &str) -> PathBuf {
    let dir = cache_root.join("codex").join("prompt-segment-rate-limits");
    fs::create_dir_all(&dir).expect("cache dir");
    let path = dir.join(format!("{key}.kv"));
    fs::write(&path, kv).expect("write kv");
    path
}

#[test]
fn prompt_segment_disabled_prints_nothing() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);
    write_prompt_segment_cache_kv(
        &cache_root,
        "alpha",
        "fetched_at=1700000000\nnon_weekly_label=5h\nnon_weekly_remaining=94\nweekly_remaining=88\nweekly_reset_epoch=1700600000\n",
    );

    let output = run(
        &["prompt-segment"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[("CODEX_PROMPT_SEGMENT_ENABLED", "false")],
    );
    assert_exit(&output, 0);
    assert!(stdout(&output).trim().is_empty());
}

#[test]
fn prompt_segment_is_enabled_exit_codes() {
    let output = run(
        &["prompt-segment", "--is-enabled"],
        &[],
        &[("CODEX_PROMPT_SEGMENT_ENABLED", "false")],
    );
    assert_exit(&output, 1);
    assert!(stdout(&output).trim().is_empty());

    let output = run(
        &["prompt-segment", "--is-enabled"],
        &[],
        &[("CODEX_PROMPT_SEGMENT_ENABLED", "true")],
    );
    assert_exit(&output, 0);
    assert!(stdout(&output).trim().is_empty());
}

#[test]
fn prompt_segment_invalid_ttl_exits_2_and_prints_usage() {
    let output = run(
        &["prompt-segment", "--ttl", "bogus"],
        &[],
        &[("CODEX_PROMPT_SEGMENT_ENABLED", "true")],
    );
    assert_exit(&output, 2);
    assert!(stderr(&output).contains("usage:"));
}

#[test]
fn prompt_segment_cached_output_formats_and_supports_no_5h() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

    let fetched_at = now_epoch().saturating_sub(1).max(1);
    write_prompt_segment_cache_kv(
        &cache_root,
        "alpha",
        &format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=94\nweekly_remaining=88\nweekly_reset_epoch=1700600000\n"
        ),
    );

    let output = run(
        &["prompt-segment", "--time-format", "%Y-%m-%dT%H:%MZ"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[("CODEX_PROMPT_SEGMENT_ENABLED", "true")],
    );
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "alpha 5h:94% W:88% 2023-11-21T20:53Z\n");

    let output = run(
        &[
            "prompt-segment",
            "--no-5h",
            "--time-format",
            "%Y-%m-%dT%H:%MZ",
        ],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[("CODEX_PROMPT_SEGMENT_ENABLED", "true")],
    );
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "alpha W:88% 2023-11-21T20:53Z\n");
}

#[test]
fn prompt_segment_default_time_uses_local_format_and_show_timezone_flag() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

    let fetched_at = now_epoch().saturating_sub(1).max(1);
    write_prompt_segment_cache_kv(
        &cache_root,
        "alpha",
        &format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=94\nweekly_remaining=88\nweekly_reset_epoch=1700600000\n"
        ),
    );

    let output = run(
        &["prompt-segment"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[("CODEX_PROMPT_SEGMENT_ENABLED", "true")],
    );
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "alpha 5h:94% W:88% 11-21 20:53\n");

    let output = run(
        &["prompt-segment", "--show-timezone"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[("CODEX_PROMPT_SEGMENT_ENABLED", "true")],
    );
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "alpha 5h:94% W:88% 11-21 20:53 +00:00\n");
}

#[test]
fn prompt_segment_stale_cache_appends_suffix() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

    let fetched_at = now_epoch().saturating_sub(10).max(1);
    write_prompt_segment_cache_kv(
        &cache_root,
        "alpha",
        &format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=1\nweekly_remaining=2\nweekly_reset_epoch=1700600000\n"
        ),
    );

    let output = run(
        &[
            "prompt-segment",
            "--ttl",
            "1s",
            "--time-format",
            "%Y-%m-%dT%H:%MZ",
        ],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_PROMPT_SEGMENT_ENABLED", "true"),
            ("CODEX_PROMPT_SEGMENT_STALE_SUFFIX", " (STALE)"),
        ],
    );
    assert_exit(&output, 0);
    assert_eq!(
        stdout(&output),
        "alpha 5h:1% W:2% 2023-11-21T20:53Z (STALE)\n"
    );
}

#[test]
fn prompt_segment_name_source_email_uses_email_not_secret_name() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

    fs::rename(secrets.join("alpha.json"), secrets.join("personal.json")).expect("rename secret");

    let fetched_at = now_epoch().saturating_sub(1).max(1);
    write_prompt_segment_cache_kv(
        &cache_root,
        "personal",
        &format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=94\nweekly_remaining=88\nweekly_reset_epoch=1700600000\n"
        ),
    );

    let output = run(
        &["prompt-segment", "--time-format", "%Y-%m-%dT%H:%MZ"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_PROMPT_SEGMENT_ENABLED", "true"),
            ("CODEX_PROMPT_SEGMENT_NAME_SOURCE", "email"),
        ],
    );
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "alpha 5h:94% W:88% 2023-11-21T20:53Z\n");
}

#[test]
fn prompt_segment_name_source_email_can_show_full_email() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

    fs::rename(secrets.join("alpha.json"), secrets.join("personal.json")).expect("rename secret");

    let fetched_at = now_epoch().saturating_sub(1).max(1);
    write_prompt_segment_cache_kv(
        &cache_root,
        "personal",
        &format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=94\nweekly_remaining=88\nweekly_reset_epoch=1700600000\n"
        ),
    );

    let output = run(
        &["prompt-segment", "--time-format", "%Y-%m-%dT%H:%MZ"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_PROMPT_SEGMENT_ENABLED", "true"),
            ("CODEX_PROMPT_SEGMENT_NAME_SOURCE", "email"),
            ("CODEX_PROMPT_SEGMENT_SHOW_FULL_EMAIL_ENABLED", "true"),
        ],
    );
    assert_exit(&output, 0);
    assert_eq!(
        stdout(&output),
        "alpha@example.com 5h:94% W:88% 2023-11-21T20:53Z\n"
    );
}

#[test]
fn prompt_segment_env_can_disable_5h_without_flag() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

    let fetched_at = now_epoch().saturating_sub(1).max(1);
    write_prompt_segment_cache_kv(
        &cache_root,
        "alpha",
        &format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=94\nweekly_remaining=88\nweekly_reset_epoch=1700600000\n"
        ),
    );

    let output = run(
        &["prompt-segment", "--time-format", "%Y-%m-%dT%H:%MZ"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_PROMPT_SEGMENT_ENABLED", "true"),
            ("CODEX_PROMPT_SEGMENT_SHOW_5H_ENABLED", "false"),
        ],
    );
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "alpha W:88% 2023-11-21T20:53Z\n");
}
