//! Cross-repo personal work inbox.
//!
//! `forge-cli inbox` is intentionally separate from repo-local lifecycle
//! commands. It can query more than one provider in a single invocation,
//! normalize items into one JSON contract, and report provider-local failures
//! without hiding successful results from another provider.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::{Deserialize, Serialize};

use crate::backend::{BackendCall, BackendProgram, BackendRunner, BackendSuccess};
use crate::cli::{
    BINARY, GitlabVpnModeFlag, GlobalFlags, InboxCommand, InboxItemTypeFlag, InboxKindFlag,
    InboxNextArgs, InboxQueryArgs, parse_duration,
};
use crate::config::ForgeConfig;
use crate::envelope::emit_success_with_warnings;
use crate::error::ForgeError;
use crate::provider::{Provider, classify_host, git_remote_url, parse_host};
use crate::rate_limit::default_runner;

const LIST_SCHEMA: &str = "inbox.list";
const STATUS_SCHEMA: &str = "inbox.status";
const NEXT_SCHEMA: &str = "inbox.next";
const SCHEMA_VERSION: u32 = 1;
const DEFAULT_QUERY_LIMIT: u32 = 30;
const DEFAULT_PROVIDER_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_VPN_CHECK_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_CACHE_MAX_AGE: Duration = Duration::from_secs(30 * 60);
const GH_JSON_FIELDS: &str = "number,url,title,updatedAt,author,repository";
const ENV_INBOX_GITLAB_HOST: &str = "FORGE_CLI_INBOX_GITLAB_HOST";
const ENV_INBOX_GITLAB_VPN: &str = "FORGE_CLI_INBOX_GITLAB_VPN";
const ENV_INBOX_GITLAB_VPN_CHECK: &str = "FORGE_CLI_INBOX_GITLAB_VPN_CHECK";
const ENV_INBOX_GITLAB_VPN_CHECK_TIMEOUT: &str = "FORGE_CLI_INBOX_GITLAB_VPN_CHECK_TIMEOUT";
const ENV_INBOX_GITLAB_OPENVPN_PROFILE: &str = "FORGE_CLI_INBOX_GITLAB_OPENVPN_PROFILE";
const ENV_INBOX_PROVIDER_TIMEOUT: &str = "FORGE_CLI_INBOX_PROVIDER_TIMEOUT";
const ENV_INBOX_STRICT_PROVIDERS: &str = "FORGE_CLI_INBOX_STRICT_PROVIDERS";
const ENV_INBOX_CACHE_FALLBACK: &str = "FORGE_CLI_INBOX_CACHE_FALLBACK";
const ENV_INBOX_CACHE_MAX_AGE: &str = "FORGE_CLI_INBOX_CACHE_MAX_AGE";
const ENV_INBOX_NO_CACHE: &str = "FORGE_CLI_INBOX_NO_CACHE";
const ENV_INBOX_CACHE_DIR: &str = "FORGE_CLI_INBOX_CACHE_DIR";

#[derive(Debug, Clone)]
struct ProviderTarget {
    provider: Provider,
    host: String,
}

#[derive(Debug, Clone)]
struct QueryConfig {
    reasons: Vec<InboxKindFlag>,
    item_type: InboxItemTypeFlag,
    query_limit: u32,
}

#[derive(Debug, Clone)]
struct InboxRuntimeConfig {
    gitlab_vpn_mode: GitlabVpnMode,
    gitlab_vpn_check: Option<VpnCheck>,
    gitlab_vpn_check_timeout: Duration,
    gitlab_openvpn_profile: Option<PathBuf>,
    provider_timeout: Option<Duration>,
    strict_providers: bool,
    cache: CachePolicy,
}

#[derive(Debug, Clone)]
struct CachePolicy {
    no_cache: bool,
    fallback: bool,
    max_age: Duration,
    dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitlabVpnMode {
    Off,
    Optional,
    Required,
}

impl GitlabVpnMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Optional => "optional",
            Self::Required => "required",
        }
    }
}

#[derive(Debug, Clone)]
enum VpnCheck {
    Tcp { host: String, port: u16 },
    Cmd { program: String },
    OpenVpn,
}

impl VpnCheck {
    fn kind(&self) -> &'static str {
        match self {
            Self::Tcp { .. } => "tcp",
            Self::Cmd { .. } => "cmd",
            Self::OpenVpn => "openvpn",
        }
    }
}

/// Classification of a GitLab `todo` target. `Unknown` covers payloads where
/// neither `target_type` nor the URL gives a confident PR/Issue answer; those
/// rows are included only in all-items mode so PR-only and issue-only callers
/// never see noise from unclassifiable todos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TodoTarget {
    MergeRequest,
    Issue,
    Unknown,
}

#[derive(Debug, Clone)]
struct ProviderSuccess {
    items: Vec<InboxItem>,
    limited: bool,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct InboxCollection {
    providers: Vec<InboxProviderStatus>,
    items: Vec<InboxItem>,
    warnings: Vec<String>,
    successes: usize,
    failures: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InboxProviderStatus {
    pub provider: &'static str,
    pub host: String,
    pub ok: bool,
    pub item_count: usize,
    pub limited: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<InboxProviderError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<InboxProviderCacheStatus>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InboxProviderError {
    pub kind: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InboxProviderCacheStatus {
    pub used: bool,
    pub stale: bool,
    pub age_seconds: u64,
    pub item_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboxStaleMetadata {
    pub reason: String,
    pub cached_at_unix: u64,
    pub age_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboxItem {
    pub provider: String,
    pub host: String,
    pub kind: String,
    pub reasons: Vec<String>,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub updated_at: String,
    pub author: Option<String>,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale: Option<InboxStaleMetadata>,
}

#[derive(Debug, Clone, Serialize)]
struct InboxListPayload {
    providers: Vec<InboxProviderStatus>,
    limit: u32,
    items: Vec<InboxItem>,
}

#[derive(Debug, Clone, Serialize)]
struct InboxStatusPayload {
    providers: Vec<InboxProviderStatus>,
    limit: u32,
    item_count: usize,
    counts: Vec<InboxCount>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct InboxCount {
    provider: &'static str,
    host: String,
    kind: String,
    reason: String,
    count: usize,
    limited: bool,
}

#[derive(Debug, Clone, Serialize)]
struct InboxNextPayload {
    providers: Vec<InboxProviderStatus>,
    limit: u32,
    query_limit: u32,
    items: Vec<InboxItem>,
}

#[derive(Debug, Clone, Serialize)]
struct InboxDryRunPayload {
    providers: Vec<InboxDryRunProvider>,
    limit: u32,
    provider_timeout_seconds: Option<u64>,
    strict_providers: bool,
    cache: InboxDryRunCache,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct InboxDryRunProvider {
    provider: &'static str,
    host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    vpn: Option<InboxDryRunVpn>,
    plans: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
struct InboxDryRunVpn {
    mode: &'static str,
    check_kind: &'static str,
    check_timeout_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    openvpn_profile: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
struct InboxDryRunCache {
    enabled: bool,
    fallback: bool,
    max_age_seconds: u64,
}

#[derive(Debug, Clone)]
struct ProviderQuery {
    reason: InboxKindFlag,
    source: &'static str,
    call: BackendCall,
}

#[derive(Debug, Clone)]
struct GitlabIdentity {
    id: String,
    username: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct ItemKey {
    provider: String,
    host: String,
    repo: String,
    number: u64,
    url: String,
}

#[derive(Debug, Clone)]
struct InboxOptionOverrides {
    gitlab_vpn: Option<GitlabVpnModeFlag>,
    gitlab_vpn_check: Option<String>,
    gitlab_vpn_check_timeout: Option<Duration>,
    gitlab_openvpn_profile: Option<PathBuf>,
    provider_timeout: Option<Duration>,
    strict_providers: bool,
    cache_fallback: bool,
    cache_max_age: Option<Duration>,
    no_cache: bool,
}

pub fn run(
    global: &GlobalFlags,
    command: InboxCommand,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let runner = default_runner();
    run_with(&runner, global, command, format)
}

pub fn run_with<R: BackendRunner + Sync>(
    runner: &R,
    global: &GlobalFlags,
    command: InboxCommand,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    if global.host.is_some() {
        return Err(ForgeError::provider_unsupported(
            schema_err(),
            "--host is not supported by inbox; use inbox --gitlab-host HOST for GitLab",
            Some("explicit GitHub Enterprise inbox search is unsupported in v1".into()),
        ));
    }
    match command {
        InboxCommand::List(args) => run_list(runner, global, args, format),
        InboxCommand::Status(args) => run_status(runner, global, args, format),
        InboxCommand::Next(args) => run_next(runner, global, args, format),
    }
}

fn run_list<R: BackendRunner + Sync>(
    runner: &R,
    global: &GlobalFlags,
    args: InboxQueryArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let targets = resolve_targets(global, args.gitlab_host.as_deref());
    let runtime = InboxRuntimeConfig::from_overrides(&InboxOptionOverrides::from_query(&args))?;
    let config = QueryConfig::new(args.kinds, args.item_type, args.limit.max(1));
    if global.dry_run {
        return Ok(emit_dry_run(
            schema_version_for(BINARY, LIST_SCHEMA, SCHEMA_VERSION),
            &targets,
            &config,
            &runtime,
            args.limit.max(1),
            None,
            format,
        ));
    }

    let collection = collect_inbox(runner, &targets, &config, &runtime)?;
    if let Some(code) = emit_failure_if_needed(
        schema_version_for(BINARY, LIST_SCHEMA, SCHEMA_VERSION),
        &collection,
        &runtime,
        format,
    ) {
        return Ok(code);
    }
    let payload = InboxListPayload {
        providers: collection.providers,
        limit: config.query_limit,
        items: collection.items,
    };
    Ok(emit_success_with_warnings(
        schema_version_for(BINARY, LIST_SCHEMA, SCHEMA_VERSION),
        payload,
        collection.warnings,
        format,
        render_list_text,
    ))
}

fn run_status<R: BackendRunner + Sync>(
    runner: &R,
    global: &GlobalFlags,
    args: InboxQueryArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let targets = resolve_targets(global, args.gitlab_host.as_deref());
    let runtime = InboxRuntimeConfig::from_overrides(&InboxOptionOverrides::from_query(&args))?;
    let config = QueryConfig::new(args.kinds, args.item_type, args.limit.max(1));
    if global.dry_run {
        return Ok(emit_dry_run(
            schema_version_for(BINARY, STATUS_SCHEMA, SCHEMA_VERSION),
            &targets,
            &config,
            &runtime,
            args.limit.max(1),
            None,
            format,
        ));
    }

    let collection = collect_inbox(runner, &targets, &config, &runtime)?;
    if let Some(code) = emit_failure_if_needed(
        schema_version_for(BINARY, STATUS_SCHEMA, SCHEMA_VERSION),
        &collection,
        &runtime,
        format,
    ) {
        return Ok(code);
    }
    let counts = summarize_counts(&collection.providers, &collection.items);
    let payload = InboxStatusPayload {
        providers: collection.providers,
        limit: config.query_limit,
        item_count: collection.items.len(),
        counts,
    };
    Ok(emit_success_with_warnings(
        schema_version_for(BINARY, STATUS_SCHEMA, SCHEMA_VERSION),
        payload,
        collection.warnings,
        format,
        render_status_text,
    ))
}

fn run_next<R: BackendRunner + Sync>(
    runner: &R,
    global: &GlobalFlags,
    args: InboxNextArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let targets = resolve_targets(global, args.gitlab_host.as_deref());
    let result_limit = args.limit.max(1);
    let query_limit = result_limit.max(DEFAULT_QUERY_LIMIT);
    let runtime = InboxRuntimeConfig::from_overrides(&InboxOptionOverrides::from_next(&args))?;
    let config = QueryConfig::new(args.kinds, args.item_type, query_limit);
    if global.dry_run {
        return Ok(emit_dry_run(
            schema_version_for(BINARY, NEXT_SCHEMA, SCHEMA_VERSION),
            &targets,
            &config,
            &runtime,
            result_limit,
            Some(query_limit),
            format,
        ));
    }

    let mut collection = collect_inbox(runner, &targets, &config, &runtime)?;
    if let Some(code) = emit_failure_if_needed(
        schema_version_for(BINARY, NEXT_SCHEMA, SCHEMA_VERSION),
        &collection,
        &runtime,
        format,
    ) {
        return Ok(code);
    }
    collection.items.truncate(result_limit as usize);
    let payload = InboxNextPayload {
        providers: collection.providers,
        limit: result_limit,
        query_limit,
        items: collection.items,
    };
    Ok(emit_success_with_warnings(
        schema_version_for(BINARY, NEXT_SCHEMA, SCHEMA_VERSION),
        payload,
        collection.warnings,
        format,
        render_next_text,
    ))
}

impl QueryConfig {
    fn new(kinds: Vec<InboxKindFlag>, item_type: InboxItemTypeFlag, query_limit: u32) -> Self {
        let mut reasons = if kinds.is_empty() {
            vec![
                InboxKindFlag::Review,
                InboxKindFlag::Assigned,
                InboxKindFlag::Todo,
                InboxKindFlag::Authored,
            ]
        } else {
            kinds
        };
        reasons.sort_by_key(|r| reason_rank(r.as_str()));
        reasons.dedup();
        Self {
            reasons,
            item_type,
            query_limit,
        }
    }

    fn wants(&self, reason: InboxKindFlag) -> bool {
        self.reasons.contains(&reason)
    }

    fn allows_pr(&self) -> bool {
        matches!(
            self.item_type,
            InboxItemTypeFlag::All | InboxItemTypeFlag::Pr
        )
    }

    fn allows_issue(&self) -> bool {
        matches!(
            self.item_type,
            InboxItemTypeFlag::All | InboxItemTypeFlag::Issue
        )
    }

    /// Whether at least one selected GitLab query family needs the user
    /// identity. Review filters by `reviewer_username`, so it needs identity
    /// only when PR-class results are allowed (review queries are MR-only).
    /// Authored filters by `author_id` and exists for both MR and issue, so it
    /// needs identity whenever it is selected (`--item-type` always allows at
    /// least one of `pr` / `issue`).
    ///
    /// Invariant locked by `gitlab_identity_predicate_matches_query_plan`:
    /// this predicate must equal `gitlab_queries(_, None, self)` losing at
    /// least one query family relative to `gitlab_queries(_, Some(_), self)`.
    fn gitlab_identity_needed(&self) -> bool {
        let review_needs = self.wants(InboxKindFlag::Review) && self.allows_pr();
        let authored_needs = self.wants(InboxKindFlag::Authored);
        review_needs || authored_needs
    }
}

impl InboxOptionOverrides {
    fn from_query(args: &InboxQueryArgs) -> Self {
        Self {
            gitlab_vpn: args.gitlab_vpn,
            gitlab_vpn_check: args.gitlab_vpn_check.clone(),
            gitlab_vpn_check_timeout: args.gitlab_vpn_check_timeout,
            gitlab_openvpn_profile: args.gitlab_openvpn_profile.clone(),
            provider_timeout: args.provider_timeout,
            strict_providers: args.strict_providers,
            cache_fallback: args.cache_fallback,
            cache_max_age: args.cache_max_age,
            no_cache: args.no_cache,
        }
    }

    fn from_next(args: &InboxNextArgs) -> Self {
        Self {
            gitlab_vpn: args.gitlab_vpn,
            gitlab_vpn_check: args.gitlab_vpn_check.clone(),
            gitlab_vpn_check_timeout: args.gitlab_vpn_check_timeout,
            gitlab_openvpn_profile: args.gitlab_openvpn_profile.clone(),
            provider_timeout: args.provider_timeout,
            strict_providers: args.strict_providers,
            cache_fallback: args.cache_fallback,
            cache_max_age: args.cache_max_age,
            no_cache: args.no_cache,
        }
    }
}

impl InboxRuntimeConfig {
    fn from_overrides(overrides: &InboxOptionOverrides) -> Result<Self, ForgeError> {
        let cfg = load_repo_config();
        let gitlab_vpn_mode = resolve_vpn_mode(overrides.gitlab_vpn, &cfg)?;
        let gitlab_vpn_check = resolve_vpn_check(overrides.gitlab_vpn_check.as_deref(), &cfg)?;
        let gitlab_vpn_check_timeout = resolve_duration_setting(
            overrides.gitlab_vpn_check_timeout,
            ENV_INBOX_GITLAB_VPN_CHECK_TIMEOUT,
            cfg.inbox_gitlab_vpn_check_timeout,
            DEFAULT_VPN_CHECK_TIMEOUT,
        )?;
        let gitlab_openvpn_profile = overrides
            .gitlab_openvpn_profile
            .clone()
            .or_else(|| env_path(ENV_INBOX_GITLAB_OPENVPN_PROFILE))
            .or_else(|| cfg.inbox_gitlab_openvpn_profile.clone());
        let provider_timeout = resolve_provider_timeout(overrides.provider_timeout, &cfg)?;
        let strict_providers = overrides.strict_providers
            || env_bool(ENV_INBOX_STRICT_PROVIDERS)?
            || cfg.inbox_strict_providers.unwrap_or(false);
        let no_cache = overrides.no_cache
            || env_bool(ENV_INBOX_NO_CACHE)?
            || cfg.inbox_no_cache.unwrap_or(false);
        let cache_fallback = overrides.cache_fallback
            || env_bool(ENV_INBOX_CACHE_FALLBACK)?
            || cfg.inbox_cache_fallback.unwrap_or(false);
        let cache_max_age = resolve_duration_setting(
            overrides.cache_max_age,
            ENV_INBOX_CACHE_MAX_AGE,
            cfg.inbox_cache_max_age,
            DEFAULT_CACHE_MAX_AGE,
        )?;
        Ok(Self {
            gitlab_vpn_mode,
            gitlab_vpn_check,
            gitlab_vpn_check_timeout,
            gitlab_openvpn_profile,
            provider_timeout,
            strict_providers,
            cache: CachePolicy {
                no_cache,
                fallback: cache_fallback,
                max_age: cache_max_age,
                dir: cache_dir(),
            },
        })
    }
}

fn load_repo_config() -> ForgeConfig {
    let workdir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    ForgeConfig::load_layered(&workdir, find_git_toplevel(&workdir).as_deref())
}

fn find_git_toplevel(start: &Path) -> Option<PathBuf> {
    let mut cursor = Some(start.to_path_buf());
    while let Some(dir) = cursor {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        cursor = dir.parent().map(Path::to_path_buf);
    }
    None
}

fn resolve_vpn_mode(
    explicit: Option<GitlabVpnModeFlag>,
    cfg: &ForgeConfig,
) -> Result<GitlabVpnMode, ForgeError> {
    if let Some(mode) = explicit {
        return Ok(match mode {
            GitlabVpnModeFlag::Off => GitlabVpnMode::Off,
            GitlabVpnModeFlag::Optional => GitlabVpnMode::Optional,
            GitlabVpnModeFlag::Required => GitlabVpnMode::Required,
        });
    }
    if let Some(mode) = env_string(ENV_INBOX_GITLAB_VPN) {
        return parse_vpn_mode(&mode);
    }
    if let Some(mode) = cfg.inbox_gitlab_vpn.as_deref() {
        return parse_vpn_mode(mode);
    }
    Ok(GitlabVpnMode::Off)
}

fn parse_vpn_mode(raw: &str) -> Result<GitlabVpnMode, ForgeError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "off" | "false" | "disabled" => Ok(GitlabVpnMode::Off),
        "optional" => Ok(GitlabVpnMode::Optional),
        "required" | "true" | "on" => Ok(GitlabVpnMode::Required),
        other => Err(ForgeError::validation(
            schema_err(),
            "vpn_mode_invalid",
            format!("invalid GitLab VPN mode {other:?}; expected off, optional, or required"),
            None,
        )),
    }
}

fn resolve_vpn_check(
    explicit: Option<&str>,
    cfg: &ForgeConfig,
) -> Result<Option<VpnCheck>, ForgeError> {
    let raw = explicit
        .map(str::to_string)
        .or_else(|| env_string(ENV_INBOX_GITLAB_VPN_CHECK))
        .or_else(|| cfg.inbox_gitlab_vpn_check.clone());
    raw.as_deref().map(parse_vpn_check).transpose()
}

fn parse_vpn_check(raw: &str) -> Result<VpnCheck, ForgeError> {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("openvpn") {
        return Ok(VpnCheck::OpenVpn);
    }
    if let Some(rest) = trimmed.strip_prefix("cmd:") {
        let program = rest.trim();
        if program.is_empty() {
            return Err(vpn_check_invalid("cmd check must include a program"));
        }
        return Ok(VpnCheck::Cmd {
            program: program.to_string(),
        });
    }
    if let Some(rest) = trimmed.strip_prefix("tcp:") {
        let Some((host, port)) = rest.rsplit_once(':') else {
            return Err(vpn_check_invalid("tcp check must use tcp:<host>:<port>"));
        };
        let host = host.trim();
        let port = port
            .trim()
            .parse::<u16>()
            .map_err(|_| vpn_check_invalid("tcp check port must be 1-65535"))?;
        if host.is_empty() || port == 0 {
            return Err(vpn_check_invalid(
                "tcp check host and port must be non-empty",
            ));
        }
        return Ok(VpnCheck::Tcp {
            host: host.to_string(),
            port,
        });
    }
    Err(vpn_check_invalid(
        "GitLab VPN check must be tcp:<host>:<port>, cmd:<program>, or openvpn",
    ))
}

fn vpn_check_invalid(message: impl Into<String>) -> ForgeError {
    ForgeError::validation(schema_err(), "vpn_check_invalid", message, None)
}

fn resolve_duration_setting(
    explicit: Option<Duration>,
    env_name: &str,
    configured: Option<Duration>,
    default: Duration,
) -> Result<Duration, ForgeError> {
    if let Some(duration) = explicit {
        return Ok(duration);
    }
    if let Some(raw) = env_string(env_name) {
        return parse_duration(&raw).map_err(|err| {
            ForgeError::validation(
                schema_err(),
                "duration_invalid",
                format!("{env_name} has invalid duration: {err}"),
                None,
            )
        });
    }
    Ok(configured.unwrap_or(default))
}

fn resolve_provider_timeout(
    explicit: Option<Duration>,
    cfg: &ForgeConfig,
) -> Result<Option<Duration>, ForgeError> {
    let duration = resolve_duration_setting(
        explicit,
        ENV_INBOX_PROVIDER_TIMEOUT,
        cfg.inbox_provider_timeout,
        DEFAULT_PROVIDER_TIMEOUT,
    )?;
    Ok((!duration.is_zero()).then_some(duration))
}

fn env_string(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn env_path(name: &str) -> Option<PathBuf> {
    env_string(name).map(PathBuf::from)
}

fn env_bool(name: &str) -> Result<bool, ForgeError> {
    let Some(raw) = env_string(name) else {
        return Ok(false);
    };
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ForgeError::validation(
            schema_err(),
            "bool_invalid",
            format!("{name} must be true or false"),
            None,
        )),
    }
}

fn cache_dir() -> Option<PathBuf> {
    env_path(ENV_INBOX_CACHE_DIR)
        .or_else(|| env_path("XDG_CACHE_HOME").map(|p| p.join("nils-cli/forge-cli/inbox")))
        .or_else(|| env_path("HOME").map(|p| p.join(".cache/nils-cli/forge-cli/inbox")))
}

fn resolve_targets(global: &GlobalFlags, gitlab_host: Option<&str>) -> Vec<ProviderTarget> {
    match global.provider {
        Some(crate::cli::ProviderFlag::Github) => vec![ProviderTarget {
            provider: Provider::GitHub,
            host: github_host(global),
        }],
        Some(crate::cli::ProviderFlag::Gitlab) => vec![ProviderTarget {
            provider: Provider::GitLab,
            host: gitlab_host_for(global, gitlab_host),
        }],
        // The local provider is a per-repo file store with no cross-repo work
        // inbox, so it contributes no targets.
        Some(crate::cli::ProviderFlag::Local) => Vec::new(),
        Some(crate::cli::ProviderFlag::Named(_)) => Vec::new(),
        None => vec![
            ProviderTarget {
                provider: Provider::GitHub,
                host: github_host(global),
            },
            ProviderTarget {
                provider: Provider::GitLab,
                host: gitlab_host_for(global, gitlab_host),
            },
        ],
    }
}

fn github_host(global: &GlobalFlags) -> String {
    host_from_remote(global, Provider::GitHub).unwrap_or_else(|| "github.com".to_string())
}

fn gitlab_host_for(global: &GlobalFlags, explicit: Option<&str>) -> String {
    if let Some(trimmed) = explicit.map(str::trim)
        && !trimmed.is_empty()
    {
        return trimmed.to_string();
    }
    if let Some(host) = env_gitlab_host() {
        return host;
    }
    host_from_remote(global, Provider::GitLab).unwrap_or_else(|| "gitlab.com".to_string())
}

fn env_gitlab_host() -> Option<String> {
    std::env::var(ENV_INBOX_GITLAB_HOST)
        .ok()
        .map(|host| host.trim().to_string())
        .filter(|host| !host.is_empty())
}

fn host_from_remote(global: &GlobalFlags, provider: Provider) -> Option<String> {
    let url = git_remote_url(&global.remote)?;
    let host = parse_host(&url)?;
    if classify_host(&host) == Some(provider) {
        Some(host)
    } else {
        None
    }
}

/// Pre-execution snapshot of a provider's query plan. Built from
/// `(target, config)` only — no backend calls are issued at build time.
/// Dry-run and live execution paths share this builder so the planned argv
/// can never drift from what live execution would actually run.
#[derive(Debug, Clone)]
struct ProviderPlan {
    target: ProviderTarget,
    config: QueryConfig,
}

impl ProviderPlan {
    fn build(target: &ProviderTarget, config: &QueryConfig) -> Self {
        Self {
            target: target.clone(),
            config: config.clone(),
        }
    }

    /// Render the argv list a dry-run would emit. GitLab queries that depend
    /// on identity are rendered with placeholder `<user_id>` / `<username>`
    /// values so callers can still inspect what the live path would call.
    fn dry_run_argv(&self, runtime: &InboxRuntimeConfig) -> Vec<Vec<String>> {
        match self.target.provider {
            // Local contributes no inbox targets (see `resolve_targets`), so
            // this arm is unreachable for it; fold for exhaustiveness.
            Provider::GitHub | Provider::Local => github_queries(&self.target.host, &self.config)
                .into_iter()
                .map(|q| q.call.plan_argv())
                .collect(),
            Provider::GitLab => {
                let mut out = Vec::new();
                if gitlab_vpn_check_for_target(&self.target, runtime).is_some() {
                    out.push(vec![
                        "vpn-check".to_string(),
                        runtime.gitlab_vpn_mode.as_str().to_string(),
                        gitlab_vpn_check_for_target(&self.target, runtime)
                            .expect("checked")
                            .kind()
                            .to_string(),
                    ]);
                }
                let needs_identity = self.config.gitlab_identity_needed();
                if needs_identity {
                    out.push(gitlab_identity_call(&self.target.host).plan_argv());
                }
                let placeholder = GitlabIdentity {
                    id: "<user_id>".to_string(),
                    username: "<username>".to_string(),
                };
                let identity = needs_identity.then_some(&placeholder);
                for q in gitlab_queries(&self.target.host, identity, &self.config) {
                    out.push(q.call.plan_argv());
                }
                out
            }
        }
    }

    fn execute<R: BackendRunner + Sync>(
        &self,
        runner: &R,
        runtime: &InboxRuntimeConfig,
    ) -> Result<ProviderSuccess, ForgeError> {
        match self.target.provider {
            Provider::GitHub | Provider::Local => {
                execute_github(runner, &self.target, &self.config, runtime)
            }
            Provider::GitLab => execute_gitlab(runner, &self.target, &self.config, runtime),
        }
    }
}

fn collect_inbox<R: BackendRunner + Sync>(
    runner: &R,
    targets: &[ProviderTarget],
    config: &QueryConfig,
    runtime: &InboxRuntimeConfig,
) -> Result<InboxCollection, ForgeError> {
    let plans: Vec<ProviderPlan> = targets
        .iter()
        .map(|t| ProviderPlan::build(t, config))
        .collect();

    // Run providers concurrently. Each provider does its own identity lookup
    // (if needed) and query-family fan-out internally. Mixed-provider mode
    // does not block GitHub work on the GitLab identity lookup.
    let mut slots: Vec<Option<Result<ProviderSuccess, ForgeError>>> =
        (0..plans.len()).map(|_| None).collect();
    std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(plans.len());
        for (i, plan) in plans.iter().enumerate() {
            handles.push(s.spawn(move || (i, plan.execute(runner, runtime))));
        }
        for h in handles {
            let (i, res) = h.join().expect("inbox provider task panicked");
            slots[i] = Some(res);
        }
    });

    // Aggregate strictly in target order so providers, warnings, and items
    // are deterministic regardless of completion order.
    let mut providers = Vec::with_capacity(targets.len());
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut failures = Vec::new();
    let mut successes = 0usize;

    for (i, target) in targets.iter().enumerate() {
        let result = slots[i].take().expect("provider slot filled");
        match result {
            Ok(success) => {
                successes += 1;
                let item_count = success.items.len();
                providers.push(InboxProviderStatus {
                    provider: target.provider.as_str(),
                    host: target.host.clone(),
                    ok: true,
                    item_count,
                    limited: success.limited,
                    error: None,
                    cache: None,
                });
                items.extend(success.items);
                warnings.extend(success.warnings);
                if !runtime.cache.no_cache
                    && let Some(warning) = write_provider_cache(
                        target,
                        config,
                        runtime,
                        &items_for_provider(&items, target),
                    )
                {
                    warnings.push(warning);
                }
            }
            Err(err) => {
                let provider_error = InboxProviderError {
                    kind: err.kind(),
                    message: err.to_string(),
                };
                let cached = read_provider_cache(target, config, runtime, provider_error.kind);
                failures.push(format!(
                    "{} {}: {}",
                    target.provider.as_str(),
                    target.host,
                    provider_error.message
                ));
                warnings.push(format!(
                    "provider_failed: {} {}: {}",
                    target.provider.as_str(),
                    target.host,
                    provider_error.message
                ));
                let (cached_items, cache_status) = match cached {
                    Some(cached) => {
                        warnings.push(format!(
                            "provider_cache_fallback: {} {}: using {} stale item(s), age={}s",
                            target.provider.as_str(),
                            target.host,
                            cached.items.len(),
                            cached.age_seconds
                        ));
                        let status = InboxProviderCacheStatus {
                            used: true,
                            stale: true,
                            age_seconds: cached.age_seconds,
                            item_count: cached.items.len(),
                        };
                        (cached.items, Some(status))
                    }
                    None => (Vec::new(), None),
                };
                let item_count = cached_items.len();
                items.extend(cached_items);
                providers.push(InboxProviderStatus {
                    provider: target.provider.as_str(),
                    host: target.host.clone(),
                    ok: false,
                    item_count,
                    limited: false,
                    error: Some(provider_error),
                    cache: cache_status,
                });
            }
        }
    }

    let mut items = dedupe_items(items);
    sort_items(&mut items);
    for provider in &mut providers {
        if provider.ok {
            provider.item_count = items
                .iter()
                .filter(|item| item.provider == provider.provider && item.host == provider.host)
                .count();
        }
    }

    Ok(InboxCollection {
        providers,
        items,
        warnings,
        successes,
        failures: failures.len(),
    })
}

fn execute_github<R: BackendRunner + Sync>(
    runner: &R,
    target: &ProviderTarget,
    config: &QueryConfig,
    runtime: &InboxRuntimeConfig,
) -> Result<ProviderSuccess, ForgeError> {
    let queries = github_queries(&target.host, config);
    let _ = runtime;
    let per_query = run_queries_in_parallel(runner, &queries, None, |query, output| {
        parse_github_items(target, query, output)
    })?;
    Ok(aggregate_query_results(per_query, config.query_limit))
}

fn execute_gitlab<R: BackendRunner + Sync>(
    runner: &R,
    target: &ProviderTarget,
    config: &QueryConfig,
    runtime: &InboxRuntimeConfig,
) -> Result<ProviderSuccess, ForgeError> {
    let mut warnings = Vec::new();
    if let Some(warning) = check_gitlab_vpn(target, runtime)? {
        warnings.push(warning);
    }
    let identity = if config.gitlab_identity_needed() {
        Some(parse_gitlab_identity(&runner.run_with_timeout(
            &gitlab_identity_call(&target.host),
            runtime.provider_timeout,
        )?)?)
    } else {
        None
    };
    let queries = gitlab_queries(&target.host, identity.as_ref(), config);
    let item_type = config.item_type;
    let per_query = run_queries_in_parallel(
        runner,
        &queries,
        runtime.provider_timeout,
        |query, output| parse_gitlab_items(target, query, output, item_type),
    )?;
    let mut success = aggregate_query_results(per_query, config.query_limit);
    success.warnings.extend(warnings);
    Ok(success)
}

/// Run a slice of independent provider query families concurrently and
/// return their parsed item lists in plan order. The first plan-order error
/// wins so failure reporting stays deterministic across thread completion
/// orders.
fn run_queries_in_parallel<R, F>(
    runner: &R,
    queries: &[ProviderQuery],
    timeout: Option<Duration>,
    parse: F,
) -> Result<Vec<Vec<InboxItem>>, ForgeError>
where
    R: BackendRunner + Sync,
    F: Fn(&ProviderQuery, &BackendSuccess) -> Result<Vec<InboxItem>, ForgeError> + Sync,
{
    if queries.is_empty() {
        return Ok(Vec::new());
    }
    let mut slots: Vec<Option<Result<Vec<InboxItem>, ForgeError>>> =
        (0..queries.len()).map(|_| None).collect();
    let parse_ref = &parse;
    std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(queries.len());
        for (i, query) in queries.iter().enumerate() {
            handles.push(s.spawn(move || {
                let result = runner
                    .run_with_timeout(&query.call, timeout)
                    .and_then(|output| parse_ref(query, &output));
                (i, result)
            }));
        }
        for h in handles {
            let (i, res) = h.join().expect("inbox query task panicked");
            slots[i] = Some(res);
        }
    });
    let mut out = Vec::with_capacity(slots.len());
    for slot in slots {
        out.push(slot.expect("query slot filled")?);
    }
    Ok(out)
}

fn aggregate_query_results(per_query: Vec<Vec<InboxItem>>, query_limit: u32) -> ProviderSuccess {
    let mut items = Vec::new();
    let mut limited = false;
    for q in per_query {
        if q.len() as u32 >= query_limit {
            limited = true;
        }
        items.extend(q);
    }
    ProviderSuccess {
        items: dedupe_items(items),
        limited,
        warnings: Vec::new(),
    }
}

fn gitlab_vpn_check_for_target(
    target: &ProviderTarget,
    runtime: &InboxRuntimeConfig,
) -> Option<VpnCheck> {
    if target.provider != Provider::GitLab || runtime.gitlab_vpn_mode == GitlabVpnMode::Off {
        return None;
    }
    runtime.gitlab_vpn_check.clone().or_else(|| {
        (runtime.gitlab_vpn_mode == GitlabVpnMode::Required).then(|| VpnCheck::Tcp {
            host: target.host.clone(),
            port: 443,
        })
    })
}

fn check_gitlab_vpn(
    target: &ProviderTarget,
    runtime: &InboxRuntimeConfig,
) -> Result<Option<String>, ForgeError> {
    let Some(check) = gitlab_vpn_check_for_target(target, runtime) else {
        return Ok(None);
    };
    match run_vpn_check(&check, target, runtime) {
        Ok(()) => Ok(None),
        Err(err) if runtime.gitlab_vpn_mode == GitlabVpnMode::Optional => Ok(Some(format!(
            "vpn_probe_failed: gitlab {}: {}",
            target.host,
            sanitize_sensitive(&err.to_string(), runtime)
        ))),
        Err(err) => Err(err),
    }
}

fn run_vpn_check(
    check: &VpnCheck,
    target: &ProviderTarget,
    runtime: &InboxRuntimeConfig,
) -> Result<(), ForgeError> {
    match check {
        VpnCheck::Tcp { host, port } => run_tcp_vpn_check(host, *port, runtime),
        VpnCheck::Cmd { program } => run_cmd_vpn_check(program, target, runtime),
        VpnCheck::OpenVpn => run_openvpn_check(runtime),
    }
}

fn run_tcp_vpn_check(
    host: &str,
    port: u16,
    runtime: &InboxRuntimeConfig,
) -> Result<(), ForgeError> {
    let timeout = runtime.gitlab_vpn_check_timeout;
    let host = host.to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn({
        let host = host.clone();
        move || {
            let _ = tx.send(resolve_and_connect_tcp_probe(&host, port, timeout));
        }
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(TcpProbeError::Resolve(err))) => Err(vpn_unavailable(
            format!("GitLab VPN TCP probe could not resolve host: {err}"),
            None,
        )),
        Ok(Err(TcpProbeError::NoAddress)) => Err(vpn_unavailable(
            "GitLab VPN TCP probe found no address",
            None,
        )),
        Ok(Err(TcpProbeError::Connect(err))) => Err(vpn_unavailable(
            format!(
                "GitLab VPN TCP probe failed for {host}:{port} within {}: {err}",
                format_duration(timeout)
            ),
            None,
        )),
        Ok(Err(TcpProbeError::Timeout)) | Err(mpsc::RecvTimeoutError::Timeout) => {
            Err(vpn_unavailable(
                format!(
                    "GitLab VPN TCP probe timed out for {host}:{port} after {}",
                    format_duration(timeout)
                ),
                None,
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(vpn_unavailable(
            "GitLab VPN TCP probe worker ended without a result",
            None,
        )),
    }
}

enum TcpProbeError {
    Resolve(String),
    NoAddress,
    Connect(String),
    Timeout,
}

fn resolve_and_connect_tcp_probe(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<(), TcpProbeError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|err| TcpProbeError::Resolve(err.to_string()))?;
    let mut saw_addr = false;
    let mut last_err = None;
    for addr in addrs {
        saw_addr = true;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            return Err(TcpProbeError::Timeout);
        }
        match TcpStream::connect_timeout(&addr, remaining) {
            Ok(_) => return Ok(()),
            Err(err) => last_err = Some(err.to_string()),
        }
    }
    if saw_addr {
        Err(TcpProbeError::Connect(
            last_err.unwrap_or_else(|| "connection failed".to_string()),
        ))
    } else {
        Err(TcpProbeError::NoAddress)
    }
}

fn run_cmd_vpn_check(
    program: &str,
    target: &ProviderTarget,
    runtime: &InboxRuntimeConfig,
) -> Result<(), ForgeError> {
    let mut cmd = Command::new(program);
    cmd.env("FORGE_CLI_INBOX_GITLAB_HOST", &target.host);
    if let Some(profile) = runtime.gitlab_openvpn_profile.as_ref() {
        cmd.env(ENV_INBOX_GITLAB_OPENVPN_PROFILE, profile);
    }
    match command_output_with_timeout(&mut cmd, runtime.gitlab_vpn_check_timeout) {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let stderr = sanitize_sensitive(
                &crate::backend::redact_and_tail(&String::from_utf8_lossy(&output.stderr)),
                runtime,
            );
            Err(vpn_unavailable(
                "GitLab VPN readiness command failed",
                (!stderr.is_empty()).then_some(stderr),
            ))
        }
        Err(CommandProbeError::Timeout) => Err(vpn_unavailable(
            format!(
                "GitLab VPN readiness command timed out after {}",
                format_duration(runtime.gitlab_vpn_check_timeout)
            ),
            None,
        )),
        Err(CommandProbeError::Io(err)) if err.kind() == io::ErrorKind::NotFound => Err(
            vpn_probe_dependency_missing("GitLab VPN readiness command not found"),
        ),
        Err(CommandProbeError::Io(err)) => Err(vpn_unavailable(
            format!("GitLab VPN readiness command could not run: {err}"),
            None,
        )),
    }
}

fn run_openvpn_check(runtime: &InboxRuntimeConfig) -> Result<(), ForgeError> {
    if let Some(profile) = runtime.gitlab_openvpn_profile.as_ref()
        && !profile.is_file()
    {
        return Err(vpn_unavailable(
            "OpenVPN profile is configured but is not readable (<redacted>)",
            None,
        ));
    }
    let mut cmd = Command::new("openvpn");
    cmd.arg("--version");
    match command_output_with_timeout(&mut cmd, runtime.gitlab_vpn_check_timeout) {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let stderr = sanitize_sensitive(
                &crate::backend::redact_and_tail(&String::from_utf8_lossy(&output.stderr)),
                runtime,
            );
            Err(vpn_unavailable(
                "openvpn probe failed",
                (!stderr.is_empty()).then_some(stderr),
            ))
        }
        Err(CommandProbeError::Timeout) => Err(vpn_unavailable(
            format!(
                "openvpn probe timed out after {}",
                format_duration(runtime.gitlab_vpn_check_timeout)
            ),
            None,
        )),
        Err(CommandProbeError::Io(err)) if err.kind() == io::ErrorKind::NotFound => {
            Err(vpn_probe_dependency_missing(
                "openvpn not found on PATH; install it with Homebrew or choose a tcp/cmd check",
            ))
        }
        Err(CommandProbeError::Io(err)) => Err(vpn_unavailable(
            format!("openvpn probe could not run: {err}"),
            None,
        )),
    }
}

fn vpn_unavailable(message: impl Into<String>, detail: Option<String>) -> ForgeError {
    ForgeError::unavailable(schema_err(), "vpn_unavailable", message, detail)
}

fn vpn_probe_dependency_missing(message: impl Into<String>) -> ForgeError {
    ForgeError::unavailable(schema_err(), "vpn_probe_dependency_missing", message, None)
}

enum CommandProbeError {
    Io(io::Error),
    Timeout,
}

fn command_output_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
) -> Result<std::process::Output, CommandProbeError> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    configure_child_group(cmd);
    let mut child = cmd.spawn().map_err(CommandProbeError::Io)?;
    let started = Instant::now();
    loop {
        if child.try_wait().map_err(CommandProbeError::Io)?.is_some() {
            return child.wait_with_output().map_err(CommandProbeError::Io);
        }
        if started.elapsed() >= timeout {
            kill_child_group(&mut child);
            let _ = child.wait();
            return Err(CommandProbeError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn configure_child_group(cmd: &mut Command) {
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
}

fn kill_child_group(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        let pgid = -(child.id() as libc::pid_t);
        let _ = libc::kill(pgid, libc::SIGKILL);
    }
    let _ = child.kill();
}

fn emit_failure_if_needed(
    schema_version: String,
    collection: &InboxCollection,
    runtime: &InboxRuntimeConfig,
    format: OutputFormat,
) -> Option<i32> {
    if collection.successes == 0 {
        return Some(emit_inbox_failure(
            schema_version,
            "backend_error",
            "all selected inbox providers failed",
            collection,
            format,
        ));
    }
    if runtime.strict_providers && collection.failures > 0 {
        return Some(emit_inbox_failure(
            schema_version,
            "provider_failed",
            "one or more selected inbox providers failed",
            collection,
            format,
        ));
    }
    None
}

fn emit_inbox_failure(
    schema_version: String,
    code: &'static str,
    message: &'static str,
    collection: &InboxCollection,
    format: OutputFormat,
) -> i32 {
    let details = serde_json::json!({
        "providers": collection.providers,
        "warnings": collection.warnings,
        "item_count": collection.items.len(),
    });
    match format {
        OutputFormat::Json => {
            let envelope: Envelope<()> = Envelope::failure(
                schema_version,
                EnvelopeError::new(code, message).with_details(details),
            );
            println!(
                "{}",
                serde_json::to_string(&envelope).unwrap_or_else(|_| "{\"ok\":false}".to_string())
            );
        }
        OutputFormat::Text => {
            eprintln!("error: {code}: {message}");
            for warning in &collection.warnings {
                eprintln!("warning: {warning}");
            }
        }
    }
    exit::RUNTIME
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InboxCacheSnapshot {
    schema_version: String,
    provider: String,
    host: String,
    item_type: String,
    reasons: Vec<String>,
    query_limit: u32,
    created_unix: u64,
    items: Vec<InboxItem>,
}

struct CachedProviderItems {
    age_seconds: u64,
    items: Vec<InboxItem>,
}

fn write_provider_cache(
    target: &ProviderTarget,
    config: &QueryConfig,
    runtime: &InboxRuntimeConfig,
    items: &[InboxItem],
) -> Option<String> {
    let dir = runtime.cache.dir.as_ref()?;
    if let Err(err) = fs::create_dir_all(dir) {
        return Some(format!(
            "provider_cache_write_failed: {} {}: {err}",
            target.provider.as_str(),
            target.host
        ));
    }
    let path = cache_file_path(dir, target, config);
    let snapshot = InboxCacheSnapshot {
        schema_version: "forge-cli.inbox.cache.v1".to_string(),
        provider: target.provider.as_str().to_string(),
        host: target.host.clone(),
        item_type: config.item_type.as_str().to_string(),
        reasons: config
            .reasons
            .iter()
            .map(|reason| reason.as_str().to_string())
            .collect(),
        query_limit: config.query_limit,
        created_unix: now_unix(),
        items: items.to_vec(),
    };
    match serde_json::to_vec_pretty(&snapshot)
        .map_err(|err| err.to_string())
        .and_then(|body| fs::write(&path, body).map_err(|err| err.to_string()))
    {
        Ok(()) => None,
        Err(err) => Some(format!(
            "provider_cache_write_failed: {} {}: {err}",
            target.provider.as_str(),
            target.host
        )),
    }
}

fn read_provider_cache(
    target: &ProviderTarget,
    config: &QueryConfig,
    runtime: &InboxRuntimeConfig,
    reason: &'static str,
) -> Option<CachedProviderItems> {
    if runtime.cache.no_cache || !runtime.cache.fallback {
        return None;
    }
    let dir = runtime.cache.dir.as_ref()?;
    let path = cache_file_path(dir, target, config);
    let body = fs::read_to_string(path).ok()?;
    let snapshot: InboxCacheSnapshot = serde_json::from_str(&body).ok()?;
    if snapshot.provider != target.provider.as_str()
        || snapshot.host != target.host
        || snapshot.item_type != config.item_type.as_str()
        || snapshot.query_limit != config.query_limit
    {
        return None;
    }
    let age_seconds = now_unix().saturating_sub(snapshot.created_unix);
    if age_seconds > runtime.cache.max_age.as_secs() {
        return None;
    }
    let items = snapshot
        .items
        .into_iter()
        .map(|mut item| {
            item.stale = Some(InboxStaleMetadata {
                reason: reason.to_string(),
                cached_at_unix: snapshot.created_unix,
                age_seconds,
            });
            item
        })
        .collect();
    Some(CachedProviderItems { age_seconds, items })
}

fn items_for_provider(items: &[InboxItem], target: &ProviderTarget) -> Vec<InboxItem> {
    let provider = target.provider.as_str();
    items
        .iter()
        .filter(|item| item.provider == provider && item.host == target.host)
        .cloned()
        .collect()
}

fn cache_file_path(dir: &Path, target: &ProviderTarget, config: &QueryConfig) -> PathBuf {
    let reasons = config
        .reasons
        .iter()
        .map(|reason| reason.as_str())
        .collect::<Vec<_>>()
        .join("+");
    dir.join(format!(
        "{}-{}-{}-{}-{}.json",
        target.provider.as_str(),
        sanitize_filename(&target.host),
        config.item_type.as_str(),
        sanitize_filename(&reasons),
        config.query_limit
    ))
}

fn sanitize_filename(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn sanitize_sensitive(raw: &str, runtime: &InboxRuntimeConfig) -> String {
    let mut out = raw.to_string();
    if let Some(profile) = runtime.gitlab_openvpn_profile.as_ref() {
        let profile = profile.to_string_lossy();
        if !profile.is_empty() {
            out = out.replace(profile.as_ref(), "<redacted-openvpn-profile>");
        }
    }
    out
}

fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1_000 {
        format!("{millis}ms")
    } else if millis.is_multiple_of(60_000) {
        format!("{}m", millis / 60_000)
    } else if millis.is_multiple_of(1_000) {
        format!("{}s", millis / 1_000)
    } else {
        format!("{millis}ms")
    }
}

fn github_queries(host: &str, config: &QueryConfig) -> Vec<ProviderQuery> {
    let mut queries = Vec::new();
    if config.wants(InboxKindFlag::Review) && config.allows_pr() {
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Review,
            source: "github_search_prs",
            call: github_search_call(host, "prs", "--review-requested", config.query_limit),
        });
    }
    if config.wants(InboxKindFlag::Assigned) {
        if config.allows_pr() {
            queries.push(ProviderQuery {
                reason: InboxKindFlag::Assigned,
                source: "github_search_prs",
                call: github_search_call(host, "prs", "--assignee", config.query_limit),
            });
        }
        if config.allows_issue() {
            queries.push(ProviderQuery {
                reason: InboxKindFlag::Assigned,
                source: "github_search_issues",
                call: github_search_call(host, "issues", "--assignee", config.query_limit),
            });
        }
    }
    if config.wants(InboxKindFlag::Authored) {
        if config.allows_pr() {
            queries.push(ProviderQuery {
                reason: InboxKindFlag::Authored,
                source: "github_search_prs",
                call: github_search_call(host, "prs", "--author", config.query_limit),
            });
        }
        if config.allows_issue() {
            queries.push(ProviderQuery {
                reason: InboxKindFlag::Authored,
                source: "github_search_issues",
                call: github_search_call(host, "issues", "--author", config.query_limit),
            });
        }
    }
    if config.wants(InboxKindFlag::Involved) {
        if config.allows_pr() {
            queries.push(ProviderQuery {
                reason: InboxKindFlag::Involved,
                source: "github_search_prs",
                call: github_search_call(host, "prs", "--involves", config.query_limit),
            });
        }
        if config.allows_issue() {
            queries.push(ProviderQuery {
                reason: InboxKindFlag::Involved,
                source: "github_search_issues",
                call: github_search_call(host, "issues", "--involves", config.query_limit),
            });
        }
    }
    queries
}

fn github_search_call(host: &str, kind: &str, qualifier: &str, limit: u32) -> BackendCall {
    BackendCall::new(
        BackendProgram::Gh,
        [
            OsString::from("search"),
            OsString::from(kind),
            OsString::from(qualifier),
            OsString::from("@me"),
            OsString::from("--state"),
            OsString::from("open"),
            OsString::from("--sort"),
            OsString::from("updated"),
            OsString::from("--order"),
            OsString::from("desc"),
            OsString::from("--limit"),
            OsString::from(limit.to_string()),
            OsString::from("--json"),
            OsString::from(GH_JSON_FIELDS),
        ],
    )
    .with_host(Provider::GitHub, host)
}

fn gitlab_identity_call(host: &str) -> BackendCall {
    BackendCall::new(
        BackendProgram::Glab,
        [
            OsString::from("api"),
            OsString::from("user"),
            OsString::from("--hostname"),
            OsString::from(host),
        ],
    )
}

fn gitlab_queries(
    host: &str,
    identity: Option<&GitlabIdentity>,
    config: &QueryConfig,
) -> Vec<ProviderQuery> {
    let mut queries = Vec::new();
    if config.wants(InboxKindFlag::Assigned) {
        if config.allows_pr() {
            queries.push(ProviderQuery {
                reason: InboxKindFlag::Assigned,
                source: "gitlab_merge_requests",
                call: gitlab_api_call(
                    host,
                    format!(
                        "merge_requests?scope=assigned_to_me&state=opened&order_by=updated_at&sort=desc&per_page={}",
                        config.query_limit
                    ),
                ),
            });
        }
        if config.allows_issue() {
            queries.push(ProviderQuery {
                reason: InboxKindFlag::Assigned,
                source: "gitlab_issues",
                call: gitlab_api_call(
                    host,
                    format!(
                        "issues?scope=assigned_to_me&state=opened&order_by=updated_at&sort=desc&per_page={}",
                        config.query_limit
                    ),
                ),
            });
        }
    }
    if config.wants(InboxKindFlag::Review)
        && config.allows_pr()
        && let Some(identity) = identity
    {
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Review,
            source: "gitlab_merge_requests",
            call: gitlab_api_call(
                host,
                format!(
                    "merge_requests?reviewer_username={}&state=opened&order_by=updated_at&sort=desc&per_page={}",
                    identity.username, config.query_limit
                ),
            ),
        });
    }
    if config.wants(InboxKindFlag::Authored)
        && let Some(identity) = identity
    {
        if config.allows_pr() {
            queries.push(ProviderQuery {
                reason: InboxKindFlag::Authored,
                source: "gitlab_merge_requests",
                call: gitlab_api_call(
                    host,
                    format!(
                        "merge_requests?author_id={}&state=opened&order_by=updated_at&sort=desc&per_page={}",
                        identity.id, config.query_limit
                    ),
                ),
            });
        }
        if config.allows_issue() {
            queries.push(ProviderQuery {
                reason: InboxKindFlag::Authored,
                source: "gitlab_issues",
                call: gitlab_api_call(
                    host,
                    format!(
                        "issues?author_id={}&state=opened&order_by=updated_at&sort=desc&per_page={}",
                        identity.id, config.query_limit
                    ),
                ),
            });
        }
    }
    if config.wants(InboxKindFlag::Todo) {
        queries.push(ProviderQuery {
            reason: InboxKindFlag::Todo,
            source: "gitlab_todos",
            call: gitlab_api_call(
                host,
                format!(
                    "todos?state=pending&order_by=updated_at&sort=desc&per_page={}",
                    config.query_limit
                ),
            ),
        });
    }
    queries
}

fn gitlab_api_call(host: &str, path: String) -> BackendCall {
    BackendCall::new(
        BackendProgram::Glab,
        [
            OsString::from("api"),
            OsString::from("--hostname"),
            OsString::from(host),
            OsString::from(path),
        ],
    )
}

fn parse_github_items(
    target: &ProviderTarget,
    query: &ProviderQuery,
    output: &BackendSuccess,
) -> Result<Vec<InboxItem>, ForgeError> {
    let values = parse_array(output, "GitHub inbox JSON is invalid")?;
    values
        .iter()
        .map(|raw| {
            let number = required_u64(raw, "number")?;
            let url = required_str(raw, "url")?;
            let repo = github_repo(raw).unwrap_or_else(|| repo_from_url(&url));
            Ok(InboxItem {
                provider: Provider::GitHub.as_str().to_string(),
                host: target.host.clone(),
                kind: query.reason.as_str().to_string(),
                reasons: vec![query.reason.as_str().to_string()],
                repo,
                number,
                title: required_str(raw, "title")?,
                url,
                updated_at: optional_str(raw, "updatedAt").unwrap_or_default(),
                author: github_author(raw),
                source: query.source.to_string(),
                stale: None,
            })
        })
        .collect()
}

fn parse_gitlab_items(
    target: &ProviderTarget,
    query: &ProviderQuery,
    output: &BackendSuccess,
    item_type: InboxItemTypeFlag,
) -> Result<Vec<InboxItem>, ForgeError> {
    let values = parse_array(output, "GitLab inbox JSON is invalid")?;
    if query.source == "gitlab_todos" {
        let mut out = Vec::with_capacity(values.len());
        for raw in &values {
            let target_kind = classify_gitlab_todo(raw);
            if !todo_target_matches(target_kind, item_type) {
                continue;
            }
            out.push(parse_gitlab_todo(target, query, raw)?);
        }
        Ok(out)
    } else {
        values
            .iter()
            .map(|raw| parse_gitlab_work_item(target, query, raw))
            .collect()
    }
}

fn classify_gitlab_todo(raw: &serde_json::Value) -> TodoTarget {
    // `target_type` is the authoritative field on the GitLab todos API; trust
    // it whenever present.
    if let Some(t) = optional_str(raw, "target_type") {
        match t.as_str() {
            "MergeRequest" => return TodoTarget::MergeRequest,
            "Issue" => return TodoTarget::Issue,
            _ => return TodoTarget::Unknown,
        }
    }
    // Fallback for payloads without `target_type` (older stubs / unusual
    // responses): only accept canonical GitLab path segments (`/-/issues/`,
    // `/-/merge_requests/`) so loose substrings on foreign hosts cannot
    // misclassify.
    let url = optional_str(raw, "target_url")
        .or_else(|| raw.get("target").and_then(|t| optional_str(t, "web_url")));
    if let Some(url) = url {
        if url.contains("/-/merge_requests/") {
            return TodoTarget::MergeRequest;
        }
        if url.contains("/-/issues/") {
            return TodoTarget::Issue;
        }
    }
    TodoTarget::Unknown
}

fn todo_target_matches(target: TodoTarget, item_type: InboxItemTypeFlag) -> bool {
    matches!(
        (item_type, target),
        (InboxItemTypeFlag::All, _)
            | (InboxItemTypeFlag::Pr, TodoTarget::MergeRequest)
            | (InboxItemTypeFlag::Issue, TodoTarget::Issue)
    )
}

fn parse_gitlab_work_item(
    target: &ProviderTarget,
    query: &ProviderQuery,
    raw: &serde_json::Value,
) -> Result<InboxItem, ForgeError> {
    let number = raw
        .get("iid")
        .and_then(|v| v.as_u64())
        .or_else(|| raw.get("id").and_then(|v| v.as_u64()))
        .ok_or_else(|| missing("iid"))?;
    let url = required_str(raw, "web_url")?;
    Ok(InboxItem {
        provider: Provider::GitLab.as_str().to_string(),
        host: target.host.clone(),
        kind: query.reason.as_str().to_string(),
        reasons: vec![query.reason.as_str().to_string()],
        repo: gitlab_repo(raw).unwrap_or_else(|| repo_from_url(&url)),
        number,
        title: required_str(raw, "title")?,
        url,
        updated_at: optional_str(raw, "updated_at").unwrap_or_default(),
        author: gitlab_author(raw),
        source: query.source.to_string(),
        stale: None,
    })
}

fn parse_gitlab_todo(
    target: &ProviderTarget,
    query: &ProviderQuery,
    raw: &serde_json::Value,
) -> Result<InboxItem, ForgeError> {
    let target_obj = raw.get("target").unwrap_or(raw);
    let url = optional_str(target_obj, "web_url")
        .or_else(|| optional_str(raw, "target_url"))
        .ok_or_else(|| missing("target.web_url"))?;
    let number = target_obj
        .get("iid")
        .and_then(|v| v.as_u64())
        .or_else(|| target_obj.get("id").and_then(|v| v.as_u64()))
        .or_else(|| raw.get("id").and_then(|v| v.as_u64()))
        .ok_or_else(|| missing("target.iid"))?;
    Ok(InboxItem {
        provider: Provider::GitLab.as_str().to_string(),
        host: target.host.clone(),
        kind: query.reason.as_str().to_string(),
        reasons: vec![query.reason.as_str().to_string()],
        repo: raw
            .get("project")
            .and_then(|p| optional_str(p, "path_with_namespace"))
            .or_else(|| gitlab_repo(target_obj))
            .unwrap_or_else(|| repo_from_url(&url)),
        number,
        title: optional_str(target_obj, "title")
            .or_else(|| optional_str(raw, "body"))
            .unwrap_or_else(|| "GitLab todo".to_string()),
        url,
        updated_at: optional_str(target_obj, "updated_at")
            .or_else(|| optional_str(raw, "updated_at"))
            .or_else(|| optional_str(raw, "created_at"))
            .unwrap_or_default(),
        author: target_obj
            .get("author")
            .and_then(gitlab_author_from_value)
            .or_else(|| raw.get("author").and_then(gitlab_author_from_value)),
        source: query.source.to_string(),
        stale: None,
    })
}

fn parse_gitlab_identity(output: &BackendSuccess) -> Result<GitlabIdentity, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).map_err(|e| {
        ForgeError::software(
            schema_err(),
            "GitLab user JSON is invalid",
            Some(e.to_string()),
        )
    })?;
    let id = value
        .get("id")
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .or_else(|| optional_str(&value, "id"))
        .ok_or_else(|| missing("id"))?;
    let username = required_str(&value, "username")?;
    Ok(GitlabIdentity { id, username })
}

fn parse_array(
    output: &BackendSuccess,
    message: &'static str,
) -> Result<Vec<serde_json::Value>, ForgeError> {
    let value: serde_json::Value = serde_json::from_str(output.stdout.trim())
        .map_err(|e| ForgeError::software(schema_err(), message, Some(e.to_string())))?;
    value
        .as_array()
        .cloned()
        .ok_or_else(|| ForgeError::software(schema_err(), message, Some(format!("got: {value}"))))
}

fn github_repo(raw: &serde_json::Value) -> Option<String> {
    let repo = raw.get("repository")?;
    optional_str(repo, "nameWithOwner")
        .or_else(|| optional_str(repo, "fullName"))
        .or_else(|| optional_str(repo, "name_with_owner"))
        .or_else(|| {
            let owner = repo
                .get("owner")
                .and_then(|v| optional_str(v, "login").or_else(|| optional_str(v, "name")))?;
            let name = optional_str(repo, "name")?;
            Some(format!("{owner}/{name}"))
        })
}

fn github_author(raw: &serde_json::Value) -> Option<String> {
    raw.get("author")
        .and_then(|v| optional_str(v, "login").or_else(|| optional_str(v, "name")))
}

fn gitlab_repo(raw: &serde_json::Value) -> Option<String> {
    raw.get("project")
        .and_then(|p| optional_str(p, "path_with_namespace"))
        .or_else(|| {
            raw.get("references")
                .and_then(|r| optional_str(r, "full"))
                .map(|full| strip_gitlab_reference(&full))
        })
        .or_else(|| {
            raw.get("web_url")
                .and_then(|v| v.as_str())
                .map(repo_from_url)
        })
}

fn strip_gitlab_reference(full: &str) -> String {
    full.split_once('!')
        .map(|(repo, _)| repo)
        .or_else(|| full.split_once('#').map(|(repo, _)| repo))
        .unwrap_or(full)
        .to_string()
}

fn gitlab_author(raw: &serde_json::Value) -> Option<String> {
    raw.get("author").and_then(gitlab_author_from_value)
}

fn gitlab_author_from_value(raw: &serde_json::Value) -> Option<String> {
    optional_str(raw, "username").or_else(|| optional_str(raw, "name"))
}

fn repo_from_url(url: &str) -> String {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let path = without_scheme
        .split_once('/')
        .map(|(_, path)| path)
        .unwrap_or(without_scheme);
    let (path, gitlab_style) = path
        .split_once("/-/")
        .map(|(repo, _)| (repo, true))
        .unwrap_or((path, false));
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    let first = parts.next().unwrap_or("unknown");
    let second = parts.next().unwrap_or("unknown");
    if gitlab_style {
        path.to_string()
    } else {
        format!("{first}/{second}")
    }
}

fn required_str(raw: &serde_json::Value, key: &str) -> Result<String, ForgeError> {
    optional_str(raw, key).ok_or_else(|| missing(key))
}

fn optional_str(raw: &serde_json::Value, key: &str) -> Option<String> {
    raw.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn required_u64(raw: &serde_json::Value, key: &str) -> Result<u64, ForgeError> {
    raw.get(key)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| missing(key))
}

fn missing(key: &str) -> ForgeError {
    ForgeError::software(
        schema_err(),
        format!("missing required field '{key}' in inbox JSON"),
        None,
    )
}

fn dedupe_items(items: Vec<InboxItem>) -> Vec<InboxItem> {
    let mut map: HashMap<ItemKey, InboxItem> = HashMap::new();
    for item in items {
        let key = ItemKey {
            provider: item.provider.clone(),
            host: item.host.clone(),
            repo: item.repo.clone(),
            number: item.number,
            url: item.url.clone(),
        };
        match map.get_mut(&key) {
            Some(existing) => merge_item(existing, item),
            None => {
                map.insert(key, item);
            }
        }
    }
    map.into_values().collect()
}

fn merge_item(existing: &mut InboxItem, incoming: InboxItem) {
    let previous_primary_rank = reason_rank(existing.kind.as_str());
    let incoming_rank = reason_rank(incoming.kind.as_str());
    for reason in incoming.reasons {
        if !existing.reasons.iter().any(|r| r == &reason) {
            existing.reasons.push(reason);
        }
    }
    existing.reasons.sort_by_key(|r| reason_rank(r));
    if let Some(primary) = existing.reasons.first() {
        existing.kind = primary.clone();
    }
    if incoming_rank < previous_primary_rank {
        existing.source = incoming.source;
    }
    if incoming.updated_at > existing.updated_at {
        existing.updated_at = incoming.updated_at;
    }
    if existing.author.is_none() {
        existing.author = incoming.author;
    }
}

fn sort_items(items: &mut [InboxItem]) {
    items.sort_by(|a, b| {
        reason_rank(a.kind.as_str())
            .cmp(&reason_rank(b.kind.as_str()))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.provider.cmp(&b.provider))
            .then_with(|| a.host.cmp(&b.host))
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.number.cmp(&b.number))
            .then_with(|| a.url.cmp(&b.url))
    });
}

fn reason_rank(reason: &str) -> u8 {
    match reason {
        "review" => 0,
        "assigned" => 1,
        "todo" => 2,
        "authored" => 3,
        "involved" => 4,
        _ => 9,
    }
}

fn summarize_counts(providers: &[InboxProviderStatus], items: &[InboxItem]) -> Vec<InboxCount> {
    let mut limited_by_provider: HashMap<(&'static str, &str), bool> = HashMap::new();
    for provider in providers {
        limited_by_provider.insert(
            (provider.provider, provider.host.as_str()),
            provider.limited,
        );
    }

    let mut counts: HashMap<(String, String, String), usize> = HashMap::new();
    for item in items {
        for reason in &item.reasons {
            *counts
                .entry((item.provider.clone(), item.host.clone(), reason.clone()))
                .or_insert(0) += 1;
        }
    }

    let mut rows: Vec<InboxCount> = counts
        .into_iter()
        .map(|((provider, host, reason), count)| InboxCount {
            provider: provider_static(&provider),
            host: host.clone(),
            kind: reason.clone(),
            reason,
            count,
            limited: *limited_by_provider
                .get(&(provider_static(&provider), host.as_str()))
                .unwrap_or(&false),
        })
        .collect();
    rows.sort_by(|a, b| {
        a.provider
            .cmp(b.provider)
            .then_with(|| a.host.cmp(&b.host))
            .then_with(|| reason_rank(a.reason.as_str()).cmp(&reason_rank(b.reason.as_str())))
            .then_with(|| {
                if a.count == b.count {
                    Ordering::Equal
                } else {
                    b.count.cmp(&a.count)
                }
            })
    });
    rows
}

fn provider_static(provider: &str) -> &'static str {
    match provider {
        "github" => Provider::GitHub.as_str(),
        "gitlab" => Provider::GitLab.as_str(),
        _ => "unknown",
    }
}

fn emit_dry_run(
    schema_version: String,
    targets: &[ProviderTarget],
    config: &QueryConfig,
    runtime: &InboxRuntimeConfig,
    limit: u32,
    query_limit: Option<u32>,
    format: OutputFormat,
) -> i32 {
    let providers = targets
        .iter()
        .map(|target| InboxDryRunProvider {
            provider: target.provider.as_str(),
            host: target.host.clone(),
            vpn: dry_run_vpn(target, runtime),
            plans: dry_run_plans(target, config, runtime),
        })
        .collect();
    let payload = InboxDryRunPayload {
        providers,
        limit,
        provider_timeout_seconds: runtime.provider_timeout.map(|d| d.as_secs()),
        strict_providers: runtime.strict_providers,
        cache: InboxDryRunCache {
            enabled: !runtime.cache.no_cache,
            fallback: runtime.cache.fallback,
            max_age_seconds: runtime.cache.max_age.as_secs(),
        },
        query_limit,
    };
    emit_success_with_warnings(schema_version, payload, Vec::new(), format, |payload| {
        for provider in &payload.providers {
            for plan in &provider.plans {
                println!("would run: {}", plan.join(" "));
            }
        }
    })
}

fn dry_run_plans(
    target: &ProviderTarget,
    config: &QueryConfig,
    runtime: &InboxRuntimeConfig,
) -> Vec<Vec<String>> {
    ProviderPlan::build(target, config).dry_run_argv(runtime)
}

fn dry_run_vpn(target: &ProviderTarget, runtime: &InboxRuntimeConfig) -> Option<InboxDryRunVpn> {
    if target.provider != Provider::GitLab {
        return None;
    }
    gitlab_vpn_check_for_target(target, runtime).map(|check| InboxDryRunVpn {
        mode: runtime.gitlab_vpn_mode.as_str(),
        check_kind: check.kind(),
        check_timeout_seconds: runtime.gitlab_vpn_check_timeout.as_secs(),
        openvpn_profile: runtime
            .gitlab_openvpn_profile
            .as_ref()
            .map(|_| "<redacted>"),
    })
}

fn render_list_text(payload: &InboxListPayload) {
    render_items_text(&payload.items);
}

fn render_next_text(payload: &InboxNextPayload) {
    render_items_text(&payload.items);
}

fn render_items_text(items: &[InboxItem]) {
    for item in items {
        println!(
            "[{provider}:{kind}] {repo}#{number} {title} - {url}",
            provider = item.provider,
            kind = item.kind,
            repo = item.repo,
            number = item.number,
            title = item.title,
            url = item.url,
        );
    }
}

fn render_status_text(payload: &InboxStatusPayload) {
    for provider in &payload.providers {
        println!(
            "{provider}@{host}: {count} item(s){limited}",
            provider = provider.provider,
            host = provider.host,
            count = provider.item_count,
            limited = if provider.limited { " (limited)" } else { "" },
        );
    }
    for count in &payload.counts {
        println!(
            "  {reason}: {count}",
            reason = count.reason,
            count = count.count
        );
    }
}

fn schema_err() -> String {
    schema_version_for(BINARY, "error", 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn test_runtime() -> InboxRuntimeConfig {
        InboxRuntimeConfig {
            gitlab_vpn_mode: GitlabVpnMode::Off,
            gitlab_vpn_check: None,
            gitlab_vpn_check_timeout: DEFAULT_VPN_CHECK_TIMEOUT,
            gitlab_openvpn_profile: None,
            provider_timeout: None,
            strict_providers: false,
            cache: CachePolicy {
                no_cache: true,
                fallback: false,
                max_age: DEFAULT_CACHE_MAX_AGE,
                dir: None,
            },
        }
    }

    #[test]
    fn inbox_provider_resolver_defaults_to_both_providers() {
        let global = GlobalFlags {
            format: None,
            remote: "missing-remote".into(),
            provider: None,
            host: None,
            repo: None,
            store_root: None,
            dry_run: false,
        };
        let targets = resolve_targets(&global, Some("gitlab.com"));
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].provider, Provider::GitHub);
        assert_eq!(targets[1].provider, Provider::GitLab);
        assert_eq!(targets[1].host, "gitlab.com");
    }

    #[test]
    fn github_inbox_queries_bind_target_host() {
        let config = QueryConfig::new(
            vec![InboxKindFlag::Review, InboxKindFlag::Assigned],
            InboxItemTypeFlag::All,
            5,
        );
        let queries = github_queries("internal.ghe.com", &config);

        assert!(!queries.is_empty());
        assert!(
            queries
                .iter()
                .all(|query| query.call.resolved_host() == Some("internal.ghe.com"))
        );
    }

    #[test]
    fn inbox_contract_dedupes_reasons_by_priority() {
        let item = InboxItem {
            provider: "github".to_string(),
            host: "github.com".into(),
            kind: "assigned".into(),
            reasons: vec!["assigned".into()],
            repo: "acme/widgets".into(),
            number: 7,
            title: "demo".into(),
            url: "https://github.com/acme/widgets/pull/7".into(),
            updated_at: "2026-05-21T00:00:00Z".into(),
            author: Some("alice".into()),
            source: "github_search_prs".to_string(),
            stale: None,
        };
        let mut duplicate = item.clone();
        duplicate.kind = "review".into();
        duplicate.reasons = vec!["review".into()];
        duplicate.source = "github_review_search".to_string();
        duplicate.updated_at = "2026-05-22T00:00:00Z".into();

        let items = dedupe_items(vec![item, duplicate]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "review");
        assert_eq!(items[0].reasons, vec!["review", "assigned"]);
        assert_eq!(items[0].source, "github_review_search");
        assert_eq!(items[0].updated_at, "2026-05-22T00:00:00Z");
    }

    #[test]
    fn inbox_vpn_settings_parse_aliases_and_reject_invalid_values() {
        assert_eq!(parse_vpn_mode("disabled").unwrap(), GitlabVpnMode::Off);
        assert_eq!(parse_vpn_mode("true").unwrap(), GitlabVpnMode::Required);
        assert_eq!(parse_vpn_mode("optional").unwrap(), GitlabVpnMode::Optional);
        assert_eq!(
            parse_vpn_mode("bogus").unwrap_err().kind(),
            "vpn_mode_invalid"
        );

        assert!(matches!(
            parse_vpn_check("openvpn").unwrap(),
            VpnCheck::OpenVpn
        ));
        assert!(matches!(
            parse_vpn_check("cmd: check-vpn").unwrap(),
            VpnCheck::Cmd { program } if program == "check-vpn"
        ));
        assert!(matches!(
            parse_vpn_check("tcp:gitlab.com:443").unwrap(),
            VpnCheck::Tcp { host, port } if host == "gitlab.com" && port == 443
        ));

        for raw in ["cmd:", "tcp:gitlab.com:not-a-port", "tcp::443", "bogus"] {
            assert_eq!(
                parse_vpn_check(raw).unwrap_err().kind(),
                "vpn_check_invalid"
            );
        }

        assert_eq!(format_duration(Duration::from_millis(250)), "250ms");
        assert_eq!(format_duration(Duration::from_millis(1_500)), "1500ms");
        assert_eq!(format_duration(Duration::from_secs(2)), "2s");
        assert_eq!(format_duration(Duration::from_secs(120)), "2m");
    }

    #[test]
    fn inbox_required_vpn_defaults_to_gitlab_https_tcp_probe() {
        let mut runtime = test_runtime();
        runtime.gitlab_vpn_mode = GitlabVpnMode::Required;

        let gitlab = ProviderTarget {
            provider: Provider::GitLab,
            host: "gitlab.com".into(),
        };
        assert!(matches!(
            gitlab_vpn_check_for_target(&gitlab, &runtime),
            Some(VpnCheck::Tcp { host, port }) if host == "gitlab.com" && port == 443
        ));

        let github = ProviderTarget {
            provider: Provider::GitHub,
            host: "github.com".into(),
        };
        assert!(gitlab_vpn_check_for_target(&github, &runtime).is_none());

        runtime.gitlab_vpn_mode = GitlabVpnMode::Off;
        assert!(gitlab_vpn_check_for_target(&gitlab, &runtime).is_none());
    }

    #[test]
    fn inbox_tcp_vpn_probe_connects_and_reports_unavailable_ports() {
        let mut runtime = test_runtime();
        runtime.gitlab_vpn_check_timeout = Duration::from_millis(250);

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let port = listener.local_addr().expect("listener addr").port();
        run_tcp_vpn_check("127.0.0.1", port, &runtime).expect("tcp probe connects");

        // Port 0 is reserved for local bind requests and cannot race with
        // another test binding a just-released ephemeral listener port.
        let err = run_tcp_vpn_check("127.0.0.1", 0, &runtime)
            .expect_err("reserved zero port should fail readiness");
        assert_eq!(err.kind(), "vpn_unavailable");
    }

    #[cfg(unix)]
    #[test]
    fn inbox_cmd_vpn_probe_sanitizes_profile_and_reports_missing_command() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let profile = tmp.path().join("profile.ovpn");
        std::fs::write(&profile, "client\n").expect("write profile");
        let script = tmp.path().join("check-vpn");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho '{}' >&2\nexit 7\n", profile.display()),
        )
        .expect("write script");
        let mut perms = std::fs::metadata(&script)
            .expect("script metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("chmod script");

        let mut runtime = test_runtime();
        runtime.gitlab_vpn_check_timeout = Duration::from_millis(250);
        runtime.gitlab_openvpn_profile = Some(profile.clone());
        let target = ProviderTarget {
            provider: Provider::GitLab,
            host: "gitlab.com".into(),
        };

        let err = run_cmd_vpn_check(script.to_str().expect("script path"), &target, &runtime)
            .expect_err("failing command should fail readiness");
        assert_eq!(err.kind(), "vpn_unavailable");

        let redacted = sanitize_sensitive(&format!("stderr: {}", profile.display()), &runtime);
        assert!(!redacted.contains(&profile.to_string_lossy().to_string()));
        assert!(redacted.contains("<redacted-openvpn-profile>"));

        let missing = tmp.path().join("missing-check");
        let err = run_cmd_vpn_check(missing.to_str().expect("missing path"), &target, &runtime)
            .expect_err("missing command should be unavailable");
        assert_eq!(err.kind(), "vpn_probe_dependency_missing");
    }

    #[test]
    fn inbox_openvpn_probe_redacts_unreadable_profile_path() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let profile = tmp.path().join("missing.ovpn");
        let mut runtime = test_runtime();
        runtime.gitlab_openvpn_profile = Some(profile.clone());

        let err = run_openvpn_check(&runtime).expect_err("missing profile should fail before exec");
        let rendered = err.to_string();
        assert_eq!(err.kind(), "vpn_unavailable");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains(&profile.to_string_lossy().to_string()));
    }

    #[test]
    fn inbox_provider_cache_round_trips_items_as_stale() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let mut runtime = test_runtime();
        runtime.cache.no_cache = false;
        runtime.cache.fallback = true;
        runtime.cache.dir = Some(tmp.path().to_path_buf());

        let target = ProviderTarget {
            provider: Provider::GitLab,
            host: "gitlab.com:8443".into(),
        };
        let config = QueryConfig::new(vec![InboxKindFlag::Assigned], InboxItemTypeFlag::All, 5);
        let items = vec![InboxItem {
            provider: "gitlab".to_string(),
            host: target.host.clone(),
            kind: "assigned".into(),
            reasons: vec!["assigned".into()],
            repo: "team/widgets".into(),
            number: 42,
            title: "Review timeout handling".into(),
            url: "https://gitlab.com/team/widgets/-/merge_requests/42".into(),
            updated_at: "2026-05-24T00:00:00Z".into(),
            author: Some("alice".into()),
            source: "gitlab_merge_requests".to_string(),
            stale: None,
        }];

        assert!(write_provider_cache(&target, &config, &runtime, &items).is_none());
        let cached = read_provider_cache(&target, &config, &runtime, "provider_failed")
            .expect("read provider cache");
        assert_eq!(cached.items.len(), 1);
        assert_eq!(cached.items[0].repo, "team/widgets");
        let stale = cached.items[0].stale.as_ref().expect("stale metadata");
        assert_eq!(stale.reason, "provider_failed");
        assert!(cached.age_seconds <= runtime.cache.max_age.as_secs());
    }

    /// Fake runner that records concurrent-call peak via atomic counters.
    /// Each call sleeps briefly so genuinely-serial callers can be detected
    /// (max_inflight stays at 1) and parallel callers can be confirmed
    /// (max_inflight reaches the parallelism width). Returns `[]` for every
    /// call so downstream parse paths succeed.
    struct ConcurrencyProbeRunner {
        inflight: std::sync::atomic::AtomicUsize,
        max_inflight: std::sync::atomic::AtomicUsize,
        call_count: std::sync::atomic::AtomicUsize,
    }

    impl ConcurrencyProbeRunner {
        fn new() -> Self {
            Self {
                inflight: std::sync::atomic::AtomicUsize::new(0),
                max_inflight: std::sync::atomic::AtomicUsize::new(0),
                call_count: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl BackendRunner for ConcurrencyProbeRunner {
        fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
            use std::sync::atomic::Ordering;
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
            let mut prev = self.max_inflight.load(Ordering::SeqCst);
            while now > prev
                && let Err(found) = self.max_inflight.compare_exchange(
                    prev,
                    now,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
            {
                prev = found;
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            // GitLab identity lookup must look like a user object; everything
            // else returns an empty array so parse paths succeed.
            let body = if call.argv.first().map(|s| s.to_string_lossy().into_owned())
                == Some("api".into())
                && call.argv.get(1).map(|s| s.to_string_lossy().into_owned()) == Some("user".into())
            {
                "{\"id\":42,\"username\":\"probe\"}".to_string()
            } else {
                "[]".to_string()
            };
            Ok(BackendSuccess {
                stdout: body,
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn inbox_parallel_query_families_observe_concurrent_inflight() {
        // GitHub default mode has 5 independent search families; expect at
        // least 2 to be inflight simultaneously when the parallel path is
        // wired correctly. (Serial execution would peak at 1.)
        let runner = ConcurrencyProbeRunner::new();
        let target = ProviderTarget {
            provider: Provider::GitHub,
            host: "github.com".into(),
        };
        let config = QueryConfig::new(Vec::new(), InboxItemTypeFlag::All, 30);
        let result =
            execute_github(&runner, &target, &config, &test_runtime()).expect("execute_github");
        assert!(result.items.is_empty(), "stub returns empty payloads");

        let calls = runner.call_count.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            calls >= 5,
            "expected at least 5 search families, observed {calls}"
        );
        let peak = runner
            .max_inflight
            .load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            peak >= 2,
            "expected concurrent inflight queries (>=2), observed peak {peak}"
        );
    }

    #[test]
    fn inbox_parallel_providers_observe_concurrent_inflight() {
        let runner = ConcurrencyProbeRunner::new();
        let targets = vec![
            ProviderTarget {
                provider: Provider::GitHub,
                host: "github.com".into(),
            },
            ProviderTarget {
                provider: Provider::GitLab,
                host: "gitlab.com".into(),
            },
        ];
        let config = QueryConfig::new(Vec::new(), InboxItemTypeFlag::All, 30);
        let collection =
            collect_inbox(&runner, &targets, &config, &test_runtime()).expect("collect_inbox");
        assert_eq!(collection.providers.len(), 2);
        assert_eq!(collection.providers[0].provider, "github");
        assert_eq!(collection.providers[1].provider, "gitlab");

        let peak = runner
            .max_inflight
            .load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            peak >= 2,
            "expected concurrent inflight across providers (>=2), observed {peak}"
        );
    }

    #[test]
    fn inbox_parallel_provider_failure_keeps_deterministic_order() {
        // First provider succeeds (returns empty), second fails. Even with
        // parallel execution, target order — and warning order — must stay
        // stable.
        struct FailingSecondProvider {
            calls: std::sync::Mutex<Vec<String>>,
        }
        impl BackendRunner for FailingSecondProvider {
            fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
                let argv = call.plan_argv().join(" ");
                self.calls.lock().unwrap().push(argv.clone());
                if argv.contains("glab") {
                    Err(ForgeError::backend_error(
                        schema_err(),
                        "boom",
                        Some("stderr".into()),
                    ))
                } else {
                    Ok(BackendSuccess {
                        stdout: "[]".into(),
                        stderr: String::new(),
                    })
                }
            }
        }
        let runner = FailingSecondProvider {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let targets = vec![
            ProviderTarget {
                provider: Provider::GitHub,
                host: "github.com".into(),
            },
            ProviderTarget {
                provider: Provider::GitLab,
                host: "gitlab.com".into(),
            },
        ];
        let config = QueryConfig::new(Vec::new(), InboxItemTypeFlag::All, 30);
        let collection =
            collect_inbox(&runner, &targets, &config, &test_runtime()).expect("collect_inbox");
        assert_eq!(collection.providers.len(), 2);
        assert_eq!(collection.providers[0].provider, "github");
        assert!(collection.providers[0].ok);
        assert_eq!(collection.providers[1].provider, "gitlab");
        assert!(!collection.providers[1].ok);
        assert_eq!(collection.warnings.len(), 1);
        assert!(
            collection.warnings[0].starts_with("provider_failed: gitlab"),
            "warning order must follow target order: {:?}",
            collection.warnings
        );
    }

    /// Invariant: `gitlab_identity_needed` must agree with the actual query
    /// plan. Whenever the predicate returns `true`, `gitlab_queries(_, None,
    /// config)` must drop at least one query that `gitlab_queries(_,
    /// Some(_), config)` would have produced. Whenever it returns `false`,
    /// both plans must be identical.
    #[test]
    fn gitlab_identity_predicate_matches_query_plan() {
        let host = "gitlab.com";
        let id = GitlabIdentity {
            id: "1".into(),
            username: "u".into(),
        };
        let kinds = [
            InboxKindFlag::Review,
            InboxKindFlag::Assigned,
            InboxKindFlag::Todo,
            InboxKindFlag::Authored,
            InboxKindFlag::Involved,
        ];
        let item_types = [
            InboxItemTypeFlag::All,
            InboxItemTypeFlag::Pr,
            InboxItemTypeFlag::Issue,
        ];
        // Cover every non-empty subset of kinds crossed with every item type.
        for mask in 1u32..(1 << kinds.len()) {
            let selected: Vec<InboxKindFlag> = kinds
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, k)| *k)
                .collect();
            for item_type in item_types {
                let config = QueryConfig::new(selected.clone(), item_type, 30);
                let without = gitlab_queries(host, None, &config).len();
                let with = gitlab_queries(host, Some(&id), &config).len();
                if config.gitlab_identity_needed() {
                    assert!(
                        with > without,
                        "predicate=true but plans match: kinds={:?} item_type={:?}",
                        selected,
                        item_type
                    );
                } else {
                    assert_eq!(
                        with, without,
                        "predicate=false but plans differ: kinds={:?} item_type={:?}",
                        selected, item_type
                    );
                }
            }
        }
    }

    #[test]
    fn classify_gitlab_todo_rejects_foreign_host_paths() {
        // No target_type, URL is a foreign host that happens to contain
        // `/issues/` as a substring — must classify as Unknown so PR/issue-only
        // filters do not surface it.
        let raw = serde_json::json!({
            "target_url": "https://customer.example.com/team/issues/42",
            "target": {
                "iid": 42,
                "web_url": "https://customer.example.com/team/issues/42",
            },
        });
        assert_eq!(classify_gitlab_todo(&raw), TodoTarget::Unknown);
    }

    #[test]
    fn classify_gitlab_todo_accepts_canonical_gitlab_paths() {
        let issue = serde_json::json!({
            "target": {"web_url": "https://gitlab.com/team/api/-/issues/32"},
        });
        assert_eq!(classify_gitlab_todo(&issue), TodoTarget::Issue);
        let mr = serde_json::json!({
            "target_url": "https://gitlab.com/team/api/-/merge_requests/77",
        });
        assert_eq!(classify_gitlab_todo(&mr), TodoTarget::MergeRequest);
    }

    #[test]
    fn classify_gitlab_todo_unknown_target_type_returns_unknown() {
        // Explicit unfamiliar `target_type` (e.g. `DesignManagement::Design`)
        // must short-circuit to Unknown without falling back to URL guessing.
        let raw = serde_json::json!({
            "target_type": "DesignManagement::Design",
            "target_url": "https://gitlab.com/team/api/-/issues/9",
        });
        assert_eq!(classify_gitlab_todo(&raw), TodoTarget::Unknown);
    }

    #[test]
    fn inbox_empty_query_plan_returns_success_without_identity_call() {
        // `--kind review --item-type issue`: review is MR-only (dropped under
        // item-type=issue), so GitLab ends up with zero query families and
        // identity lookup is skipped. The provider must still return ok.
        struct AssertNoIdentityRunner;
        impl BackendRunner for AssertNoIdentityRunner {
            fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
                let argv = call.plan_argv().join(" ");
                assert!(
                    !argv.contains("api user --hostname"),
                    "identity lookup must not be issued: {argv}"
                );
                Ok(BackendSuccess {
                    stdout: "[]".into(),
                    stderr: String::new(),
                })
            }
        }
        let target = ProviderTarget {
            provider: Provider::GitLab,
            host: "gitlab.com".into(),
        };
        let config = QueryConfig::new(vec![InboxKindFlag::Review], InboxItemTypeFlag::Issue, 30);
        let result = execute_gitlab(&AssertNoIdentityRunner, &target, &config, &test_runtime())
            .expect("execute_gitlab");
        assert!(result.items.is_empty());
        assert!(!result.limited);
    }

    #[test]
    fn inbox_partial_within_provider_failure_rolls_provider_up_as_failed() {
        // One GitHub query family succeeds; another fails. The provider as a
        // whole must surface a backend_error and the failing-provider warning
        // must follow target order.
        struct OneQueryFailsRunner;
        impl BackendRunner for OneQueryFailsRunner {
            fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
                let argv = call.plan_argv().join(" ");
                if argv.contains("--review-requested") {
                    Err(ForgeError::backend_error(
                        schema_err(),
                        "boom",
                        Some("stderr".into()),
                    ))
                } else {
                    Ok(BackendSuccess {
                        stdout: "[]".into(),
                        stderr: String::new(),
                    })
                }
            }
        }
        let targets = vec![ProviderTarget {
            provider: Provider::GitHub,
            host: "github.com".into(),
        }];
        let config = QueryConfig::new(Vec::new(), InboxItemTypeFlag::All, 30);
        let collection = collect_inbox(&OneQueryFailsRunner, &targets, &config, &test_runtime())
            .expect("collect_inbox");
        assert_eq!(collection.successes, 0);
        assert_eq!(collection.failures, 1);
        assert_eq!(
            collection.providers[0].error.as_ref().expect("error").kind,
            "backend_error"
        );
    }

    /// Within a single provider, plan-order error determinism: even if a
    /// later-planned query fails fast and an earlier-planned query fails
    /// slowly, the surfaced error must be the earlier-planned one.
    #[test]
    fn inbox_within_provider_error_follows_plan_order_not_completion_order() {
        struct PlanOrderRunner;
        impl BackendRunner for PlanOrderRunner {
            fn run(&self, call: &BackendCall) -> Result<BackendSuccess, ForgeError> {
                let argv = call.plan_argv().join(" ");
                // Plan order for default GitHub: review-requested, assigned-prs,
                // assigned-issues, authored-prs, authored-issues.
                // Make the EARLIER one fail slow, the LATER one fail fast.
                if argv.contains("--review-requested") {
                    std::thread::sleep(std::time::Duration::from_millis(60));
                    Err(ForgeError::backend_error(
                        schema_err(),
                        "slow-early-error",
                        Some(String::new()),
                    ))
                } else if argv.contains("--author") && argv.contains("issues") {
                    Err(ForgeError::backend_error(
                        schema_err(),
                        "fast-late-error",
                        Some(String::new()),
                    ))
                } else {
                    Ok(BackendSuccess {
                        stdout: "[]".into(),
                        stderr: String::new(),
                    })
                }
            }
        }
        let target = ProviderTarget {
            provider: Provider::GitHub,
            host: "github.com".into(),
        };
        let config = QueryConfig::new(Vec::new(), InboxItemTypeFlag::All, 30);
        let err = execute_github(&PlanOrderRunner, &target, &config, &test_runtime())
            .expect_err("must propagate first plan-order error");
        assert!(
            err.to_string().contains("slow-early-error"),
            "expected earlier-planned (slow) error to win, got: {err}"
        );
    }
}
