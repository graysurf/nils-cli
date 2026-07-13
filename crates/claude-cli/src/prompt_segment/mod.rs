use nils_common::cli_contract::exit;
use nils_common::diag_output;
use nils_common::env as shared_env;
use serde::Serialize;
use std::path::PathBuf;

mod auth;
mod cache;
mod client;
mod render;
pub mod usage;

#[derive(Clone, Debug, Default)]
pub struct PromptSegmentOptions {
    pub ttl: Option<String>,
    pub time_format: Option<String>,
    pub refresh: bool,
}

const DEFAULT_TTL_SECONDS: u64 = 60;
const DEFAULT_TIME_FORMAT: &str = "%m-%d %H:%M";
const DEFAULT_STALE_SUFFIX: &str = " (stale)";
const PROMPT_SEGMENT_SCHEMA_VERSION: &str = "claude-cli.prompt-segment.v1";

pub fn run(options: &PromptSegmentOptions) -> i32 {
    let ttl_seconds = match resolve_ttl_seconds(options.ttl.as_deref()) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("claude-cli prompt-segment: invalid --ttl");
            return exit::USAGE;
        }
    };

    let Some(cache_file) = cache::cache_file() else {
        return exit::SUCCESS;
    };

    let force_refresh = options.refresh || ttl_seconds == 0;
    let mut cache_snapshot = cache::snapshot(&cache_file);
    let display_expired = cache_snapshot.exists() && cache_snapshot.display_expired();
    let _refresh_guard = if display_expired && !force_refresh {
        cache::begin_expired_refresh(&cache_file)
    } else {
        None
    };
    let needs_refresh = force_refresh
        || if display_expired {
            _refresh_guard.is_some()
        } else {
            !cache_snapshot.exists() || cache_snapshot.stale(ttl_seconds)
        };
    let mut stale = false;

    if needs_refresh {
        match auth::resolve_access_token()
            .map(|token| client::fetch_usage(&token.value))
            .transpose()
        {
            Ok(Some(body)) => {
                if cache::write_cache_file(&cache_file, &body).is_err() {
                    stale = true;
                } else {
                    cache_snapshot = cache::snapshot(&cache_file);
                }
            }
            _ => {
                stale = true;
            }
        }
    }

    if cache_snapshot.display_expired() {
        return exit::SUCCESS;
    }

    let Some(raw_cache) = cache::read_cache_file(&cache_file) else {
        return exit::SUCCESS;
    };

    let time_format = options
        .time_format
        .as_deref()
        .unwrap_or(DEFAULT_TIME_FORMAT);
    let stale_suffix = resolve_stale_suffix();
    if let Some(line) = render::render_usage_json(&raw_cache, time_format, stale, &stale_suffix)
        && !line.trim().is_empty()
    {
        println!("{line}");
    }

    exit::SUCCESS
}

pub fn check() -> i32 {
    if auth::resolve_access_token().is_some() {
        exit::SUCCESS
    } else {
        exit::RUNTIME
    }
}

pub fn status(output_json: bool) -> i32 {
    let result = PromptSegmentStatusResult::inspect();

    if output_json {
        if diag_output::emit_success_result(
            PROMPT_SEGMENT_SCHEMA_VERSION,
            "prompt-segment status",
            &result,
        )
        .is_err()
        {
            return exit::RUNTIME;
        }
    } else {
        println!(
            "claude: prompt-segment status authenticated={} would_render={} reason={}",
            result.authenticated, result.would_render, result.reason
        );
    }

    exit::SUCCESS
}

fn resolve_ttl_seconds(cli_ttl: Option<&str>) -> Result<u64, ()> {
    if let Some(raw) = cli_ttl {
        return parse_ttl_seconds(raw).ok_or(());
    }

    for key in ["CLAUDE_PROMPT_SEGMENT_TTL", "CLAUDE_PROMPT_TTL"] {
        if let Ok(raw) = std::env::var(key)
            && let Some(value) = parse_ttl_seconds(&raw)
        {
            return Ok(value);
        }
    }

    Ok(DEFAULT_TTL_SECONDS)
}

fn parse_ttl_seconds(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw == "0" || raw.eq_ignore_ascii_case("0s") {
        return Some(0);
    }
    shared_env::parse_duration_seconds(raw)
}

fn resolve_stale_suffix() -> String {
    std::env::var("CLAUDE_PROMPT_SEGMENT_STALE_SUFFIX")
        .ok()
        .or_else(|| std::env::var("CLAUDE_PROMPT_STALE_SUFFIX").ok())
        .unwrap_or_else(|| DEFAULT_STALE_SUFFIX.to_string())
}

#[derive(Debug, Clone, Serialize)]
struct PromptSegmentStatusResult {
    authenticated: bool,
    auth_source: Option<String>,
    cache_file: Option<String>,
    cache_exists: bool,
    cache_stale: bool,
    would_render: bool,
    reason: String,
}

impl PromptSegmentStatusResult {
    fn inspect() -> Self {
        let token = auth::resolve_access_token();
        let cache_file = cache::cache_file();
        let ttl_seconds = resolve_ttl_seconds(None).unwrap_or(DEFAULT_TTL_SECONDS);
        let (cache_exists, cache_stale, cache_expired, would_render) =
            inspect_cache(cache_file.as_ref(), ttl_seconds);

        let authenticated = token.is_some();
        let reason = if authenticated && would_render && !cache_stale {
            "ready"
        } else if authenticated && would_render {
            "cache-stale"
        } else if !authenticated {
            "access-token-missing"
        } else if !cache_exists {
            "cache-missing"
        } else if cache_expired {
            "cache-expired"
        } else {
            "cache-empty-or-invalid"
        };

        Self {
            authenticated,
            auth_source: token.map(|token| match token.source {
                auth::TokenSource::AccessTokenEnv => "access-token-env".to_string(),
                auth::TokenSource::CredentialsJsonEnv => "credentials-json-env".to_string(),
                auth::TokenSource::Keychain => "keychain".to_string(),
            }),
            cache_file: cache_file.map(display_path),
            cache_exists,
            cache_stale,
            would_render,
            reason: reason.to_string(),
        }
    }
}

fn inspect_cache(cache_file: Option<&PathBuf>, ttl_seconds: u64) -> (bool, bool, bool, bool) {
    let Some(cache_file) = cache_file else {
        return (false, false, false, false);
    };
    let cache_snapshot = cache::snapshot(cache_file);
    if !cache_snapshot.exists() {
        return (false, false, false, false);
    }

    let cache_expired = cache_snapshot.display_expired();
    let cache_stale = cache_expired || cache_snapshot.stale(ttl_seconds);
    let would_render = !cache_expired
        && cache::read_cache_file(cache_file)
            .as_deref()
            .and_then(|raw| render::render_usage_json(raw, DEFAULT_TIME_FORMAT, false, ""))
            .map(|line| !line.trim().is_empty())
            .unwrap_or(false);

    (true, cache_stale, cache_expired, would_render)
}

fn display_path(path: PathBuf) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::{parse_ttl_seconds, resolve_ttl_seconds};
    use nils_test_support::{EnvGuard, GlobalStateLock};

    #[test]
    fn parse_ttl_seconds_accepts_zero_for_legacy_force_refresh() {
        assert_eq!(parse_ttl_seconds("0"), Some(0));
        assert_eq!(parse_ttl_seconds("0s"), Some(0));
        assert_eq!(parse_ttl_seconds("60"), Some(60));
        assert_eq!(parse_ttl_seconds("2m"), Some(120));
    }

    #[test]
    fn resolve_ttl_prefers_segment_env_before_legacy_env() {
        let lock = GlobalStateLock::new();
        let _segment = EnvGuard::set(&lock, "CLAUDE_PROMPT_SEGMENT_TTL", "2m");
        let _legacy = EnvGuard::set(&lock, "CLAUDE_PROMPT_TTL", "30");
        assert_eq!(resolve_ttl_seconds(None), Ok(120));
    }
}
