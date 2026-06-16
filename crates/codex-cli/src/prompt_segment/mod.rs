use std::path::Path;

use crate::auth;
use crate::auth::status::ActiveAuthStatus;
use crate::diag_output;
use crate::rate_limits::cache;
use nils_common::env as shared_env;
use serde::Serialize;

mod lock;
mod refresh;
mod render;

pub use render::CacheEntry;

pub struct PromptSegmentOptions {
    pub no_5h: bool,
    pub ttl: Option<String>,
    pub time_format: Option<String>,
    pub show_timezone: bool,
    pub refresh: bool,
}

const DEFAULT_TTL_SECONDS: u64 = 180;
const DEFAULT_TIME_FORMAT: &str = "%m-%d %H:%M";
const DEFAULT_TIME_FORMAT_WITH_TIMEZONE: &str = "%m-%d %H:%M %:z";
const PROMPT_SEGMENT_SCHEMA_VERSION: &str = "codex-cli.prompt-segment.v1";

pub fn run(options: &PromptSegmentOptions) -> i32 {
    let ttl_seconds = match resolve_ttl_seconds(options.ttl.as_deref()) {
        Ok(value) => value,
        Err(_) => {
            print_ttl_usage();
            return 2;
        }
    };

    if !prompt_segment_enabled() {
        return 0;
    }

    let auth_status = auth::status::inspect_active_auth();
    if !auth_status.prompt_segment_authenticated {
        return 0;
    }

    let target_file = match auth_status.auth_file {
        Some(path) => path,
        None => return 0,
    };

    let show_5h =
        shared_env::env_truthy_or("CODEX_PROMPT_SEGMENT_SHOW_5H_ENABLED", true) && !options.no_5h;
    let time_format = match options.time_format.as_deref() {
        Some(value) => value,
        None if options.show_timezone => DEFAULT_TIME_FORMAT_WITH_TIMEZONE,
        None => DEFAULT_TIME_FORMAT,
    };
    let stale_suffix = std::env::var("CODEX_PROMPT_SEGMENT_STALE_SUFFIX")
        .unwrap_or_else(|_| " (stale)".to_string());

    let prefix = resolve_name_prefix(&target_file);

    if options.refresh {
        if let Some(entry) = refresh::refresh_blocking(&target_file)
            && let Some(line) = render::render_line(&entry, &prefix, show_5h, time_format)
            && !line.trim().is_empty()
        {
            let line = apply_prompt_escape(line);
            println!("{line}");
        }
        return 0;
    }

    let (cached, is_stale) = read_cached_entry(&target_file, ttl_seconds);
    if let Some(entry) = cached.clone()
        && let Some(mut line) = render::render_line(&entry, &prefix, show_5h, time_format)
    {
        if is_stale {
            line.push_str(&stale_suffix);
        }
        if !line.trim().is_empty() {
            let line = apply_prompt_escape(line);
            println!("{line}");
        }
    }

    if cached.is_none() || is_stale {
        refresh::enqueue_background_refresh(&target_file);
    }

    0
}

pub fn check() -> i32 {
    if prompt_segment_enabled() && auth::status::inspect_active_auth().prompt_segment_authenticated
    {
        0
    } else {
        1
    }
}

pub fn status(output_json: bool) -> i32 {
    let enabled = prompt_segment_enabled();
    let auth_status = auth::status::inspect_active_auth();
    let ttl_seconds = resolve_ttl_seconds(None).unwrap_or(DEFAULT_TTL_SECONDS);
    let result = PromptSegmentStatusResult::from_state(enabled, &auth_status, ttl_seconds);

    if output_json {
        if diag_output::emit_success_result(
            PROMPT_SEGMENT_SCHEMA_VERSION,
            "prompt-segment status",
            &result,
        )
        .is_err()
        {
            return 1;
        }
    } else {
        println!(
            "codex: prompt-segment status enabled={} authenticated={} would_render={} reason={}",
            result.enabled, result.prompt_segment_authenticated, result.would_render, result.reason
        );
    }

    0
}

fn prompt_segment_enabled() -> bool {
    shared_env::env_truthy("CODEX_PROMPT_SEGMENT_ENABLED")
}

fn apply_prompt_escape(line: String) -> String {
    if shared_env::env_truthy("CODEX_PROMPT_SEGMENT_ZSH_ESCAPE_ENABLED") {
        return escape_zsh_prompt_percent(&line);
    }
    line
}

fn escape_zsh_prompt_percent(line: &str) -> String {
    line.replace('%', "%%")
}

fn resolve_ttl_seconds(cli_ttl: Option<&str>) -> Result<u64, ()> {
    if let Some(raw) = cli_ttl {
        return shared_env::parse_duration_seconds(raw).ok_or(());
    }

    if let Ok(raw) = std::env::var("CODEX_PROMPT_SEGMENT_TTL")
        && let Some(value) = shared_env::parse_duration_seconds(&raw)
    {
        return Ok(value);
    }

    Ok(DEFAULT_TTL_SECONDS)
}

fn print_ttl_usage() {
    eprintln!("codex-cli prompt-segment: invalid --ttl");
    eprintln!(
        "usage: codex-cli prompt-segment [--no-5h] [--ttl <duration>] [--time-format <strftime>] [--show-timezone] [--refresh]"
    );
}

fn read_cached_entry(target_file: &Path, ttl_seconds: u64) -> (Option<CacheEntry>, bool) {
    let cache_file = match cache::cache_file_for_target(target_file) {
        Ok(value) => value,
        Err(_) => return (None, false),
    };
    if !cache_file.is_file() {
        return (None, false);
    }

    let entry = render::read_cache_file(&cache_file);
    let Some(entry) = entry else {
        return (None, false);
    };

    let now_epoch = chrono::Utc::now().timestamp();
    if now_epoch <= 0 || entry.fetched_at_epoch <= 0 {
        return (Some(entry), true);
    }

    let ttl_i64 = i64::try_from(ttl_seconds).unwrap_or(i64::MAX);
    let stale = now_epoch.saturating_sub(entry.fetched_at_epoch) > ttl_i64;
    (Some(entry), stale)
}

fn resolve_name_prefix(target_file: &Path) -> String {
    let name = resolve_name(target_file);
    match name {
        Some(value) if !value.trim().is_empty() => format!("{} ", value.trim()),
        _ => String::new(),
    }
}

fn resolve_name(target_file: &Path) -> Option<String> {
    let name_source = std::env::var("CODEX_PROMPT_SEGMENT_NAME_SOURCE")
        .ok()
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_else(|| "secret".to_string());

    let show_fallback = shared_env::env_truthy("CODEX_PROMPT_SEGMENT_SHOW_FALLBACK_NAME_ENABLED");
    let show_full_email = shared_env::env_truthy("CODEX_PROMPT_SEGMENT_SHOW_FULL_EMAIL_ENABLED");

    if name_source == "email" {
        if let Ok(Some(email)) = auth::email_from_auth_file(target_file) {
            return Some(format_email_name(&email, show_full_email));
        }
        if show_fallback && let Ok(Some(identity)) = auth::identity_from_auth_file(target_file) {
            return Some(format_email_name(&identity, show_full_email));
        }
        return None;
    }

    if let Some(secret_name) = cache::secret_name_for_target(target_file) {
        return Some(secret_name);
    }

    if show_fallback && let Ok(Some(identity)) = auth::identity_from_auth_file(target_file) {
        return Some(format_email_name(&identity, show_full_email));
    }

    None
}

fn format_email_name(raw: &str, show_full_email: bool) -> String {
    let trimmed = raw.trim();
    if show_full_email {
        return trimmed.to_string();
    }
    trimmed.split('@').next().unwrap_or(trimmed).to_string()
}

#[derive(Debug, Clone, Serialize)]
struct PromptSegmentStatusResult {
    enabled: bool,
    authenticated: bool,
    prompt_segment_authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_file: Option<String>,
    auth_reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_file: Option<String>,
    cache_exists: bool,
    cache_stale: bool,
    would_render: bool,
    reason: String,
}

impl PromptSegmentStatusResult {
    fn from_state(enabled: bool, auth_status: &ActiveAuthStatus, ttl_seconds: u64) -> Self {
        let mut cache_file = None;
        let mut cache_exists = false;
        let mut cache_stale = false;
        let mut would_render = false;

        if enabled
            && auth_status.prompt_segment_authenticated
            && let Some(target_file) = auth_status.auth_file.as_deref()
        {
            cache_file = cache::cache_file_for_target(target_file)
                .ok()
                .map(|path| path.display().to_string());
            if let Some(path) = cache_file.as_deref() {
                cache_exists = Path::new(path).is_file();
            }
            let (cached, stale) = read_cached_entry(target_file, ttl_seconds);
            cache_stale = stale;
            would_render = cached
                .as_ref()
                .and_then(|entry| {
                    render::render_line(
                        entry,
                        &resolve_name_prefix(target_file),
                        shared_env::env_truthy_or("CODEX_PROMPT_SEGMENT_SHOW_5H_ENABLED", true),
                        DEFAULT_TIME_FORMAT,
                    )
                })
                .map(|line| !line.trim().is_empty())
                .unwrap_or(false);
        }

        let reason = if !enabled {
            "disabled"
        } else if auth_status.authenticated
            && !auth_status.prompt_segment_authenticated
            && !auth_status.has_oauth_access_token
        {
            "access-token-missing"
        } else if !auth_status.prompt_segment_authenticated {
            auth_status.reason.as_str()
        } else if would_render {
            "ready"
        } else if !cache_exists {
            "cache-missing"
        } else {
            "cache-empty-or-invalid"
        };

        Self {
            enabled,
            authenticated: auth_status.authenticated,
            prompt_segment_authenticated: auth_status.prompt_segment_authenticated,
            auth_file: auth_status
                .auth_file
                .as_ref()
                .map(|path| path.display().to_string()),
            auth_reason: auth_status.reason.as_str().to_string(),
            cache_file,
            cache_exists,
            cache_stale,
            would_render,
            reason: reason.to_string(),
        }
    }
}
