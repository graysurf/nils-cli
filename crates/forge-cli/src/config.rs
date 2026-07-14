//! Per-repo `.forge-cli.toml` loader and setting resolver.
//!
//! Spec: `crates/forge-cli/docs/specs/forge-cli-spec-v1.md` §"Configuration".
//! The loader walks the file system upward from the starting directory until
//! it either finds a `.forge-cli.toml` or hits the git toplevel (inclusive).
//! Unknown keys never error — they accumulate in `warnings` as
//! `unknown-config-key:<section>.<key>` entries so the v1 binary stays
//! forward-compatible with v2 fields documented later.
//!
//! Resolution order for any setting (per spec):
//!
//! ```text
//! explicit flag > .forge-cli.toml > user-global config > spec default
//! ```
//!
//! Review convergence is the safety exception: without an explicit flag, a
//! repository may enable or strengthen a user-global gate but cannot disable
//! `require = true`, shorten its quiet window, or remove its configured bots.
//!
//! Callers obtain a [`ForgeConfig`] from [`ForgeConfig::load_from`] (or
//! [`ForgeConfig::default`] when no repo override applies) and then ask for a
//! resolved value via the `resolve_*` helpers, passing the explicit flag (if
//! any) as the first argument.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use toml::Value;

use crate::cli::parse_duration;

/// File name searched upward from CWD.
pub const CONFIG_FILE_NAME: &str = ".forge-cli.toml";

/// File name of the user-global config under
/// `${XDG_CONFIG_HOME:-$HOME/.config}/forge-cli/`.
pub const GLOBAL_CONFIG_FILE_NAME: &str = "config.toml";

/// Top-level config sections recognised by this version.
const KNOWN_SECTIONS: &[&str] = &[
    "merge",
    "body",
    "branch",
    "checks",
    "inbox",
    "test_first",
    "review_convergence",
];

/// Recognised keys per section.
const KNOWN_MERGE_KEYS: &[&str] = &["method", "delete_branch"];
const KNOWN_BODY_KEYS: &[&str] = &["summary_heading", "test_plan_heading"];
const KNOWN_BRANCH_KEYS: &[&str] = &["feature_prefix", "bug_prefix"];
const KNOWN_TEST_FIRST_KEYS: &[&str] = &["require"];
const KNOWN_CHECKS_KEYS: &[&str] = &["timeout", "interval", "required_only"];
const KNOWN_INBOX_KEYS: &[&str] = &[
    "gitlab_vpn",
    "gitlab_vpn_check",
    "gitlab_vpn_check_timeout",
    "gitlab_openvpn_profile",
    "provider_timeout",
    "strict_providers",
    "cache_fallback",
    "cache_max_age",
    "no_cache",
];
const KNOWN_REVIEW_CONVERGENCE_KEYS: &[&str] = &["require", "quiet_period", "timeout", "bots"];
pub const MAX_REVIEW_QUIET_PERIOD: Duration = Duration::from_secs(60 * 60);
pub const MAX_REVIEW_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// Merge strategy choices supported by both backends. The string form matches
/// the spec catalog and `[merge].method` in `.forge-cli.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

impl MergeMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Squash => "squash",
            Self::Merge => "merge",
            Self::Rebase => "rebase",
        }
    }
}

impl fmt::Display for MergeMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MergeMethod {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "squash" => Ok(Self::Squash),
            "merge" => Ok(Self::Merge),
            "rebase" => Ok(Self::Rebase),
            other => Err(format!("unknown merge method {other:?}")),
        }
    }
}

/// Waiting contract for a configured review actor. `observed` is the only v1
/// mode: absence never waits or fails, while an already-submitted current-head
/// review participates in the quiet window and snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewBotMode {
    Observed,
}

impl ReviewBotMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
        }
    }
}

impl FromStr for ReviewBotMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "observed" => Ok(Self::Observed),
            other => Err(format!("unknown review bot mode {other:?}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewConvergenceBot {
    pub login: String,
    pub mode: ReviewBotMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewConvergencePolicy {
    pub require: bool,
    #[serde(serialize_with = "serialize_duration_ms")]
    pub quiet_period: Duration,
    #[serde(serialize_with = "serialize_duration_ms")]
    pub timeout: Duration,
    pub bots: Vec<ReviewConvergenceBot>,
}

fn serialize_duration_ms<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_u64(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
}

/// Loaded `.forge-cli.toml` settings. Every field is optional; the resolver
/// helpers fall back to the spec default when both the explicit override and
/// this field are unset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForgeConfig {
    pub merge_method: Option<MergeMethod>,
    pub merge_delete_branch: Option<bool>,
    pub body_summary_heading: Option<String>,
    pub body_test_plan_heading: Option<String>,
    pub branch_feature_prefix: Option<String>,
    pub branch_bug_prefix: Option<String>,
    pub checks_timeout: Option<Duration>,
    pub checks_interval: Option<Duration>,
    pub checks_required_only: Option<bool>,
    pub inbox_gitlab_vpn: Option<String>,
    pub inbox_gitlab_vpn_check: Option<String>,
    pub inbox_gitlab_vpn_check_timeout: Option<Duration>,
    pub inbox_gitlab_openvpn_profile: Option<PathBuf>,
    pub inbox_provider_timeout: Option<Duration>,
    pub inbox_strict_providers: Option<bool>,
    pub inbox_cache_fallback: Option<bool>,
    pub inbox_cache_max_age: Option<Duration>,
    pub inbox_no_cache: Option<bool>,
    /// `[test_first].require` — when true, `pr create` / `pr deliver` for
    /// feature/bug kinds must carry verified test-first evidence (a failing
    /// test or explicit waiver plus a passing final validation).
    pub test_first_required: Option<bool>,
    pub review_convergence_required: Option<bool>,
    pub review_convergence_quiet_period: Option<Duration>,
    pub review_convergence_timeout: Option<Duration>,
    /// Whole-list override. `Some([])` intentionally clears a lower-precedence
    /// global bot list unless that global layer enables the monotonic safety
    /// gate; `None` inherits it.
    pub review_convergence_bots: Option<Vec<ReviewConvergenceBot>>,
    /// Forward-compat warnings collected while parsing (unknown keys, bad
    /// scalar types). Each entry is prefixed `unknown-config-key:` or
    /// `invalid-config-value:` so callers can render them verbatim under
    /// `data.warnings[]` in any envelope that surfaces config state.
    pub warnings: Vec<String>,
    /// Absolute path of the `.forge-cli.toml` that produced this config.
    /// `None` when the loader returned defaults (no file found).
    pub source_path: Option<PathBuf>,
}

impl ForgeConfig {
    /// Search upward from `start_dir` (inclusive) for `.forge-cli.toml`,
    /// stopping at `git_toplevel` (inclusive). Both paths must be absolute.
    /// If `git_toplevel` is `None`, the loader walks all the way up to the
    /// filesystem root.
    ///
    /// Returns `Ok(ForgeConfig::default())` when no file is found — that is
    /// not an error condition.
    ///
    /// Read or parse failures are reported as warnings rather than errors so
    /// the binary remains usable even with a malformed repo override; the
    /// returned config falls back to spec defaults in that case.
    pub fn load_from(start_dir: &Path, git_toplevel: Option<&Path>) -> Self {
        let Some(found) = find_config_file(start_dir, git_toplevel) else {
            return Self::default();
        };
        let contents = match std::fs::read_to_string(&found) {
            Ok(s) => s,
            Err(err) => {
                let mut cfg = Self::default();
                cfg.warnings
                    .push(format!("invalid-config-value:read_error:{err}"));
                cfg.source_path = Some(found);
                return cfg;
            }
        };
        let value: Value = match toml::from_str(&contents) {
            Ok(v) => v,
            Err(err) => {
                let mut cfg = Self::default();
                cfg.warnings
                    .push(format!("invalid-config-value:parse_error:{err}"));
                cfg.source_path = Some(found);
                return cfg;
            }
        };
        let mut cfg = parse_value(&value);
        cfg.source_path = Some(found);
        cfg
    }

    /// Load the user-global config from
    /// `${XDG_CONFIG_HOME:-$HOME/.config}/forge-cli/config.toml`, if present.
    /// Returns `ForgeConfig::default()` when no global file exists. Read or
    /// parse failures degrade to defaults with a recorded warning, matching
    /// [`load_from`](Self::load_from).
    pub fn load_global() -> Self {
        let Some(path) = global_config_path() else {
            return Self::default();
        };
        if !path.is_file() {
            return Self::default();
        }
        let contents = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) => {
                let mut cfg = Self::default();
                cfg.warnings
                    .push(format!("invalid-config-value:global_read_error:{err}"));
                cfg.source_path = Some(path);
                return cfg;
            }
        };
        match toml::from_str::<Value>(&contents) {
            Ok(value) => {
                let mut cfg = parse_value(&value);
                cfg.source_path = Some(path);
                cfg
            }
            Err(err) => {
                let mut cfg = Self::default();
                cfg.warnings
                    .push(format!("invalid-config-value:global_parse_error:{err}"));
                cfg.source_path = Some(path);
                cfg
            }
        }
    }

    /// Layered load: the user-global config supplies defaults, the per-repo
    /// `.forge-cli.toml` overrides field-by-field, and explicit flags still win
    /// at `resolve_*` time. Precedence:
    ///
    /// ```text
    /// explicit flag > repo .forge-cli.toml > global config > spec default
    /// ```
    pub fn load_layered(start_dir: &Path, git_toplevel: Option<&Path>) -> Self {
        let global = Self::load_global();
        let repo = Self::load_from(start_dir, git_toplevel);
        global.overlaid_by(repo)
    }

    /// Overlay `top` onto `self`: fields set in `top` normally win and unset
    /// fields fall back to `self`. An enabled base review-convergence policy is
    /// monotonic: the top layer cannot disable it, shorten its quiet period, or
    /// remove its bots. Warnings are concatenated (base first), and the
    /// `source_path` reports the higher-precedence file when present.
    ///
    /// Exposed to the crate so callers that must distinguish layers (e.g. the
    /// `pr merge` rule-10 conflict check, which only fires on a repo-explicit
    /// `delete_branch`) can compose the layers themselves.
    pub(crate) fn overlaid_by(self, top: Self) -> Self {
        let protect_global_review = self.review_convergence_required == Some(true);
        let review_convergence_required = if protect_global_review {
            Some(true)
        } else {
            top.review_convergence_required
                .or(self.review_convergence_required)
        };
        let review_convergence_quiet_period = if protect_global_review {
            top.review_convergence_quiet_period.map_or(
                self.review_convergence_quiet_period,
                |repo_quiet_period| {
                    Some(
                        repo_quiet_period.max(
                            self.review_convergence_quiet_period
                                .unwrap_or_else(|| Duration::from_secs(2 * 60)),
                        ),
                    )
                },
            )
        } else {
            top.review_convergence_quiet_period
                .or(self.review_convergence_quiet_period)
        };
        let review_convergence_bots = merge_review_bot_layers(
            self.review_convergence_bots.clone(),
            top.review_convergence_bots.clone(),
            protect_global_review,
        );
        Self {
            merge_method: top.merge_method.or(self.merge_method),
            merge_delete_branch: top.merge_delete_branch.or(self.merge_delete_branch),
            body_summary_heading: top.body_summary_heading.or(self.body_summary_heading),
            body_test_plan_heading: top.body_test_plan_heading.or(self.body_test_plan_heading),
            branch_feature_prefix: top.branch_feature_prefix.or(self.branch_feature_prefix),
            branch_bug_prefix: top.branch_bug_prefix.or(self.branch_bug_prefix),
            checks_timeout: top.checks_timeout.or(self.checks_timeout),
            checks_interval: top.checks_interval.or(self.checks_interval),
            checks_required_only: top.checks_required_only.or(self.checks_required_only),
            inbox_gitlab_vpn: top.inbox_gitlab_vpn.or(self.inbox_gitlab_vpn),
            inbox_gitlab_vpn_check: top.inbox_gitlab_vpn_check.or(self.inbox_gitlab_vpn_check),
            inbox_gitlab_vpn_check_timeout: top
                .inbox_gitlab_vpn_check_timeout
                .or(self.inbox_gitlab_vpn_check_timeout),
            inbox_gitlab_openvpn_profile: top
                .inbox_gitlab_openvpn_profile
                .or(self.inbox_gitlab_openvpn_profile),
            inbox_provider_timeout: top.inbox_provider_timeout.or(self.inbox_provider_timeout),
            inbox_strict_providers: top.inbox_strict_providers.or(self.inbox_strict_providers),
            inbox_cache_fallback: top.inbox_cache_fallback.or(self.inbox_cache_fallback),
            inbox_cache_max_age: top.inbox_cache_max_age.or(self.inbox_cache_max_age),
            inbox_no_cache: top.inbox_no_cache.or(self.inbox_no_cache),
            test_first_required: top.test_first_required.or(self.test_first_required),
            review_convergence_required,
            review_convergence_quiet_period,
            review_convergence_timeout: top
                .review_convergence_timeout
                .or(self.review_convergence_timeout),
            review_convergence_bots,
            warnings: {
                let mut merged = self.warnings;
                merged.extend(top.warnings);
                merged
            },
            source_path: top.source_path.or(self.source_path),
        }
    }

    /// Resolve `[merge].method` against an optional explicit flag.
    pub fn resolve_merge_method(&self, explicit: Option<MergeMethod>) -> MergeMethod {
        explicit
            .or(self.merge_method)
            .unwrap_or(MergeMethod::Squash)
    }

    /// Resolve `[merge].delete_branch`. Default is `true` per spec.
    pub fn resolve_delete_branch(&self, explicit: Option<bool>) -> bool {
        explicit.or(self.merge_delete_branch).unwrap_or(true)
    }

    /// Resolve `[body].summary_heading`. Default is `## Summary` per spec.
    pub fn resolve_summary_heading(&self, explicit: Option<&str>) -> String {
        explicit
            .map(|s| s.to_string())
            .or_else(|| self.body_summary_heading.clone())
            .unwrap_or_else(|| "## Summary".to_string())
    }

    /// Resolve `[body].test_plan_heading`. Default is `## Test plan` per spec.
    pub fn resolve_test_plan_heading(&self, explicit: Option<&str>) -> String {
        explicit
            .map(|s| s.to_string())
            .or_else(|| self.body_test_plan_heading.clone())
            .unwrap_or_else(|| "## Test plan".to_string())
    }

    /// Resolve `[branch].feature_prefix`. Default is `feat/` per spec.
    pub fn resolve_feature_prefix(&self, explicit: Option<&str>) -> String {
        explicit
            .map(|s| s.to_string())
            .or_else(|| self.branch_feature_prefix.clone())
            .unwrap_or_else(|| "feat/".to_string())
    }

    /// Resolve `[branch].bug_prefix`. Default is `fix/` per spec.
    pub fn resolve_bug_prefix(&self, explicit: Option<&str>) -> String {
        explicit
            .map(|s| s.to_string())
            .or_else(|| self.branch_bug_prefix.clone())
            .unwrap_or_else(|| "fix/".to_string())
    }

    /// Resolve `[checks].timeout`. Default is `30m` per spec.
    pub fn resolve_checks_timeout(&self, explicit: Option<Duration>) -> Duration {
        explicit
            .or(self.checks_timeout)
            .unwrap_or_else(|| Duration::from_secs(30 * 60))
    }

    /// Resolve `[checks].interval`. Default is `20s` per spec.
    pub fn resolve_checks_interval(&self, explicit: Option<Duration>) -> Duration {
        explicit
            .or(self.checks_interval)
            .unwrap_or_else(|| Duration::from_secs(20))
    }

    /// Resolve `[checks].required_only`. Default is `true` per spec.
    pub fn resolve_required_only(&self, explicit: Option<bool>) -> bool {
        explicit.or(self.checks_required_only).unwrap_or(true)
    }

    /// Resolve `[test_first].require`. Default is `false` per spec — the gate
    /// is off unless a repo `.forge-cli.toml` or the user's global config
    /// opts in.
    pub fn resolve_test_first_required(&self, explicit: Option<bool>) -> bool {
        explicit.or(self.test_first_required).unwrap_or(false)
    }

    /// Resolve the default-off native-review convergence policy.
    pub fn resolve_review_convergence(
        &self,
        explicit_required: Option<bool>,
    ) -> ReviewConvergencePolicy {
        ReviewConvergencePolicy {
            require: explicit_required
                .or(self.review_convergence_required)
                .unwrap_or(false),
            quiet_period: self
                .review_convergence_quiet_period
                .unwrap_or_else(|| Duration::from_secs(2 * 60)),
            timeout: self
                .review_convergence_timeout
                .unwrap_or_else(|| Duration::from_secs(20 * 60)),
            bots: self.review_convergence_bots.clone().unwrap_or_default(),
        }
    }

    pub fn invalid_review_convergence_warnings(&self) -> Vec<&str> {
        self.warnings
            .iter()
            .map(String::as_str)
            .filter(|warning| {
                warning.starts_with("invalid-config-value:review_convergence")
                    || [
                        "invalid-config-value:read_error:",
                        "invalid-config-value:parse_error:",
                        "invalid-config-value:global_read_error:",
                        "invalid-config-value:global_parse_error:",
                        "invalid-config-value:root_not_table",
                    ]
                    .iter()
                    .any(|prefix| warning.starts_with(prefix))
            })
            .collect()
    }
}

fn merge_review_bot_layers(
    base: Option<Vec<ReviewConvergenceBot>>,
    top: Option<Vec<ReviewConvergenceBot>>,
    protect_base: bool,
) -> Option<Vec<ReviewConvergenceBot>> {
    if !protect_base {
        return top.or(base);
    }
    let mut merged = base.unwrap_or_default();
    for bot in top.unwrap_or_default() {
        let duplicate = merged.iter().any(|existing| {
            existing.mode == bot.mode && existing.login.eq_ignore_ascii_case(&bot.login)
        });
        if !duplicate {
            merged.push(bot);
        }
    }
    Some(merged)
}

/// Resolve the user-global config path
/// `${XDG_CONFIG_HOME:-$HOME/.config}/forge-cli/config.toml`. Returns `None`
/// when neither `XDG_CONFIG_HOME` nor `HOME` is set in the environment.
fn global_config_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("forge-cli").join(GLOBAL_CONFIG_FILE_NAME))
}

fn find_config_file(start_dir: &Path, git_toplevel: Option<&Path>) -> Option<PathBuf> {
    let toplevel = git_toplevel.map(canonical_or_path);
    let mut cursor = Some(canonical_or_path(start_dir));
    while let Some(dir) = cursor {
        let candidate = dir.join(CONFIG_FILE_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        if let Some(top) = toplevel.as_ref()
            && &dir == top
        {
            return None;
        }
        cursor = dir.parent().map(Path::to_path_buf);
    }
    None
}

fn canonical_or_path(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn parse_value(value: &Value) -> ForgeConfig {
    let mut cfg = ForgeConfig::default();
    let Value::Table(table) = value else {
        cfg.warnings
            .push("invalid-config-value:root_not_table".to_string());
        return cfg;
    };
    for (section, section_value) in table {
        if !KNOWN_SECTIONS.contains(&section.as_str()) {
            cfg.warnings.push(format!("unknown-config-key:{section}"));
            continue;
        }
        let Value::Table(section_table) = section_value else {
            cfg.warnings
                .push(format!("invalid-config-value:{section}:not_a_table"));
            continue;
        };
        match section.as_str() {
            "merge" => parse_merge(section_table, &mut cfg),
            "body" => parse_body(section_table, &mut cfg),
            "branch" => parse_branch(section_table, &mut cfg),
            "checks" => parse_checks(section_table, &mut cfg),
            "inbox" => parse_inbox(section_table, &mut cfg),
            "test_first" => parse_test_first(section_table, &mut cfg),
            "review_convergence" => parse_review_convergence(section_table, &mut cfg),
            _ => unreachable!("section filtered above"),
        }
    }
    cfg
}

fn parse_merge(table: &toml::map::Map<String, Value>, cfg: &mut ForgeConfig) {
    for (key, value) in table {
        if !KNOWN_MERGE_KEYS.contains(&key.as_str()) {
            cfg.warnings.push(format!("unknown-config-key:merge.{key}"));
            continue;
        }
        match key.as_str() {
            "method" => match value.as_str() {
                Some(s) => match MergeMethod::from_str(s) {
                    Ok(m) => cfg.merge_method = Some(m),
                    Err(_) => cfg
                        .warnings
                        .push(format!("invalid-config-value:merge.method:{s}")),
                },
                None => cfg
                    .warnings
                    .push("invalid-config-value:merge.method:not_a_string".to_string()),
            },
            "delete_branch" => match value.as_bool() {
                Some(b) => cfg.merge_delete_branch = Some(b),
                None => cfg
                    .warnings
                    .push("invalid-config-value:merge.delete_branch:not_a_bool".to_string()),
            },
            _ => unreachable!("key filtered above"),
        }
    }
}

fn parse_test_first(table: &toml::map::Map<String, Value>, cfg: &mut ForgeConfig) {
    for (key, value) in table {
        if !KNOWN_TEST_FIRST_KEYS.contains(&key.as_str()) {
            cfg.warnings
                .push(format!("unknown-config-key:test_first.{key}"));
            continue;
        }
        match key.as_str() {
            "require" => match value.as_bool() {
                Some(b) => cfg.test_first_required = Some(b),
                None => cfg
                    .warnings
                    .push("invalid-config-value:test_first.require:not_a_bool".to_string()),
            },
            _ => unreachable!("key filtered above"),
        }
    }
}

fn parse_review_convergence(table: &toml::map::Map<String, Value>, cfg: &mut ForgeConfig) {
    for (key, value) in table {
        if !KNOWN_REVIEW_CONVERGENCE_KEYS.contains(&key.as_str()) {
            cfg.warnings
                .push(format!("unknown-config-key:review_convergence.{key}"));
            continue;
        }
        match key.as_str() {
            "require" => match value.as_bool() {
                Some(required) => cfg.review_convergence_required = Some(required),
                None => cfg
                    .warnings
                    .push("invalid-config-value:review_convergence.require:not_a_bool".to_string()),
            },
            "quiet_period" | "timeout" => match value.as_str() {
                Some(raw) => match parse_duration(raw) {
                    Ok(duration) => match validate_review_duration(key, duration) {
                        Ok(()) if key == "quiet_period" => {
                            cfg.review_convergence_quiet_period = Some(duration);
                        }
                        Ok(()) => cfg.review_convergence_timeout = Some(duration),
                        Err(max) => cfg.warnings.push(format!(
                            "invalid-config-value:review_convergence.{key}:exceeds_max:{max}"
                        )),
                    },
                    Err(_) => cfg.warnings.push(format!(
                        "invalid-config-value:review_convergence.{key}:{raw}"
                    )),
                },
                None => cfg.warnings.push(format!(
                    "invalid-config-value:review_convergence.{key}:not_a_string"
                )),
            },
            "bots" => parse_review_convergence_bots(value, cfg),
            _ => unreachable!("key filtered above"),
        }
    }
}

fn parse_review_convergence_bots(value: &Value, cfg: &mut ForgeConfig) {
    let Some(items) = value.as_array() else {
        cfg.warnings
            .push("invalid-config-value:review_convergence.bots:not_an_array".to_string());
        return;
    };
    let mut bots = Vec::new();
    let mut valid = true;
    for (index, item) in items.iter().enumerate() {
        let Some(table) = item.as_table() else {
            valid = false;
            cfg.warnings.push(format!(
                "invalid-config-value:review_convergence.bots.{index}:not_a_table"
            ));
            continue;
        };
        for key in table.keys() {
            if !matches!(key.as_str(), "login" | "mode") {
                cfg.warnings.push(format!(
                    "unknown-config-key:review_convergence.bots.{index}.{key}"
                ));
            }
        }
        let Some(login) = table
            .get("login")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|login| !login.is_empty())
        else {
            valid = false;
            cfg.warnings.push(format!(
                "invalid-config-value:review_convergence.bots.{index}.login"
            ));
            continue;
        };
        let Some(mode_raw) = table.get("mode").and_then(Value::as_str) else {
            valid = false;
            cfg.warnings.push(format!(
                "invalid-config-value:review_convergence.bots.{index}.mode"
            ));
            continue;
        };
        let Ok(mode) = ReviewBotMode::from_str(mode_raw) else {
            valid = false;
            cfg.warnings.push(format!(
                "invalid-config-value:review_convergence.bots.{index}.mode:{mode_raw}"
            ));
            continue;
        };
        bots.push(ReviewConvergenceBot {
            login: login.to_string(),
            mode,
        });
    }
    if valid {
        cfg.review_convergence_bots = Some(bots);
    }
}

fn validate_review_duration(key: &str, duration: Duration) -> Result<(), &'static str> {
    let (max, label) = if key == "quiet_period" {
        (MAX_REVIEW_QUIET_PERIOD, "1h")
    } else {
        (MAX_REVIEW_TIMEOUT, "24h")
    };
    if duration <= max { Ok(()) } else { Err(label) }
}

fn parse_body(table: &toml::map::Map<String, Value>, cfg: &mut ForgeConfig) {
    for (key, value) in table {
        if !KNOWN_BODY_KEYS.contains(&key.as_str()) {
            cfg.warnings.push(format!("unknown-config-key:body.{key}"));
            continue;
        }
        let Some(s) = value.as_str() else {
            cfg.warnings
                .push(format!("invalid-config-value:body.{key}:not_a_string"));
            continue;
        };
        match key.as_str() {
            "summary_heading" => cfg.body_summary_heading = Some(s.to_string()),
            "test_plan_heading" => cfg.body_test_plan_heading = Some(s.to_string()),
            _ => unreachable!("key filtered above"),
        }
    }
}

fn parse_branch(table: &toml::map::Map<String, Value>, cfg: &mut ForgeConfig) {
    for (key, value) in table {
        if !KNOWN_BRANCH_KEYS.contains(&key.as_str()) {
            cfg.warnings
                .push(format!("unknown-config-key:branch.{key}"));
            continue;
        }
        let Some(s) = value.as_str() else {
            cfg.warnings
                .push(format!("invalid-config-value:branch.{key}:not_a_string"));
            continue;
        };
        match key.as_str() {
            "feature_prefix" => cfg.branch_feature_prefix = Some(s.to_string()),
            "bug_prefix" => cfg.branch_bug_prefix = Some(s.to_string()),
            _ => unreachable!("key filtered above"),
        }
    }
}

fn parse_checks(table: &toml::map::Map<String, Value>, cfg: &mut ForgeConfig) {
    for (key, value) in table {
        if !KNOWN_CHECKS_KEYS.contains(&key.as_str()) {
            cfg.warnings
                .push(format!("unknown-config-key:checks.{key}"));
            continue;
        }
        match key.as_str() {
            "timeout" | "interval" => match value.as_str() {
                Some(s) => match parse_duration(s) {
                    Ok(d) => {
                        if key == "timeout" {
                            cfg.checks_timeout = Some(d);
                        } else {
                            cfg.checks_interval = Some(d);
                        }
                    }
                    Err(err) => cfg
                        .warnings
                        .push(format!("invalid-config-value:checks.{key}:{err}")),
                },
                None => cfg
                    .warnings
                    .push(format!("invalid-config-value:checks.{key}:not_a_string")),
            },
            "required_only" => match value.as_bool() {
                Some(b) => cfg.checks_required_only = Some(b),
                None => cfg
                    .warnings
                    .push("invalid-config-value:checks.required_only:not_a_bool".to_string()),
            },
            _ => unreachable!("key filtered above"),
        }
    }
}

fn parse_inbox(table: &toml::map::Map<String, Value>, cfg: &mut ForgeConfig) {
    for (key, value) in table {
        if !KNOWN_INBOX_KEYS.contains(&key.as_str()) {
            cfg.warnings.push(format!("unknown-config-key:inbox.{key}"));
            continue;
        }
        match key.as_str() {
            "gitlab_vpn" | "gitlab_vpn_check" | "gitlab_openvpn_profile" => {
                let Some(s) = value.as_str() else {
                    cfg.warnings
                        .push(format!("invalid-config-value:inbox.{key}:not_a_string"));
                    continue;
                };
                match key.as_str() {
                    "gitlab_vpn" => cfg.inbox_gitlab_vpn = Some(s.to_string()),
                    "gitlab_vpn_check" => cfg.inbox_gitlab_vpn_check = Some(s.to_string()),
                    "gitlab_openvpn_profile" => {
                        cfg.inbox_gitlab_openvpn_profile = Some(PathBuf::from(s))
                    }
                    _ => unreachable!("key filtered above"),
                }
            }
            "gitlab_vpn_check_timeout" | "provider_timeout" | "cache_max_age" => {
                match value.as_str() {
                    Some(s) => match parse_duration(s) {
                        Ok(d) => match key.as_str() {
                            "gitlab_vpn_check_timeout" => {
                                cfg.inbox_gitlab_vpn_check_timeout = Some(d)
                            }
                            "provider_timeout" => cfg.inbox_provider_timeout = Some(d),
                            "cache_max_age" => cfg.inbox_cache_max_age = Some(d),
                            _ => unreachable!("key filtered above"),
                        },
                        Err(err) => cfg
                            .warnings
                            .push(format!("invalid-config-value:inbox.{key}:{err}")),
                    },
                    None => cfg
                        .warnings
                        .push(format!("invalid-config-value:inbox.{key}:not_a_string")),
                }
            }
            "strict_providers" | "cache_fallback" | "no_cache" => match value.as_bool() {
                Some(b) => match key.as_str() {
                    "strict_providers" => cfg.inbox_strict_providers = Some(b),
                    "cache_fallback" => cfg.inbox_cache_fallback = Some(b),
                    "no_cache" => cfg.inbox_no_cache = Some(b),
                    _ => unreachable!("key filtered above"),
                },
                None => cfg
                    .warnings
                    .push(format!("invalid-config-value:inbox.{key}:not_a_bool")),
            },
            _ => unreachable!("key filtered above"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join(CONFIG_FILE_NAME);
        fs::write(&path, body).expect("write config");
        path
    }

    #[test]
    fn default_when_no_file_found() {
        let tmp = TempDir::new().unwrap();
        let cfg = ForgeConfig::load_from(tmp.path(), Some(tmp.path()));
        assert_eq!(cfg, ForgeConfig::default());
    }

    #[test]
    fn parses_all_known_keys() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            r###"
[merge]
method = "rebase"
delete_branch = false

[body]
summary_heading = "## Overview"
test_plan_heading = "## Verification"

[branch]
feature_prefix = "feature/"
bug_prefix = "bugfix/"

[checks]
timeout = "45m"
interval = "5s"
required_only = false

[inbox]
gitlab_vpn = "required"
gitlab_vpn_check = "tcp:gitlab.example.com:443"
gitlab_vpn_check_timeout = "2s"
gitlab_openvpn_profile = "~/vpn/example.ovpn"
provider_timeout = "20s"
strict_providers = true
cache_fallback = true
cache_max_age = "30m"
no_cache = false

[review_convergence]
require = true
quiet_period = "2m"
timeout = "20m"

[[review_convergence.bots]]
login = "example-review-bot"
mode = "observed"
"###,
        );
        let cfg = ForgeConfig::load_from(tmp.path(), Some(tmp.path()));
        assert!(cfg.warnings.is_empty(), "warnings={:?}", cfg.warnings);
        assert_eq!(cfg.merge_method, Some(MergeMethod::Rebase));
        assert_eq!(cfg.merge_delete_branch, Some(false));
        assert_eq!(cfg.body_summary_heading.as_deref(), Some("## Overview"));
        assert_eq!(
            cfg.body_test_plan_heading.as_deref(),
            Some("## Verification")
        );
        assert_eq!(cfg.branch_feature_prefix.as_deref(), Some("feature/"));
        assert_eq!(cfg.branch_bug_prefix.as_deref(), Some("bugfix/"));
        assert_eq!(cfg.checks_timeout, Some(Duration::from_secs(45 * 60)));
        assert_eq!(cfg.checks_interval, Some(Duration::from_secs(5)));
        assert_eq!(cfg.checks_required_only, Some(false));
        assert_eq!(cfg.inbox_gitlab_vpn.as_deref(), Some("required"));
        assert_eq!(
            cfg.inbox_gitlab_vpn_check.as_deref(),
            Some("tcp:gitlab.example.com:443")
        );
        assert_eq!(
            cfg.inbox_gitlab_vpn_check_timeout,
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            cfg.inbox_gitlab_openvpn_profile,
            Some(PathBuf::from("~/vpn/example.ovpn"))
        );
        assert_eq!(cfg.inbox_provider_timeout, Some(Duration::from_secs(20)));
        assert_eq!(cfg.inbox_strict_providers, Some(true));
        assert_eq!(cfg.inbox_cache_fallback, Some(true));
        assert_eq!(cfg.inbox_cache_max_age, Some(Duration::from_secs(30 * 60)));
        assert_eq!(cfg.inbox_no_cache, Some(false));
        assert_eq!(cfg.review_convergence_required, Some(true));
        assert_eq!(
            cfg.review_convergence_quiet_period,
            Some(Duration::from_secs(2 * 60))
        );
        assert_eq!(
            cfg.review_convergence_timeout,
            Some(Duration::from_secs(20 * 60))
        );
        assert_eq!(
            cfg.review_convergence_bots,
            Some(vec![ReviewConvergenceBot {
                login: "example-review-bot".to_string(),
                mode: ReviewBotMode::Observed,
            }])
        );
    }

    #[test]
    fn loader_walks_upward_to_git_toplevel() {
        let tmp = TempDir::new().unwrap();
        let top = tmp.path();
        let nested = top.join("crates/forge-cli/src");
        fs::create_dir_all(&nested).unwrap();
        write(
            top,
            r#"[merge]
method = "merge"
"#,
        );
        let cfg = ForgeConfig::load_from(&nested, Some(top));
        assert_eq!(cfg.merge_method, Some(MergeMethod::Merge));
        assert!(
            cfg.source_path
                .as_ref()
                .map(|p| p.ends_with(CONFIG_FILE_NAME))
                .unwrap_or(false),
            "source_path should point at the discovered file, got {:?}",
            cfg.source_path
        );
    }

    #[test]
    fn loader_stops_at_git_toplevel_even_if_higher_file_exists() {
        // A higher ancestor outside the git toplevel must be ignored so
        // unrelated parent repos cannot poison the loader.
        let tmp = TempDir::new().unwrap();
        let outer = tmp.path();
        let toplevel = outer.join("repo");
        let nested = toplevel.join("crates/forge-cli/src");
        fs::create_dir_all(&nested).unwrap();
        // File at outer should be invisible — toplevel is the boundary.
        write(
            outer,
            r#"[merge]
method = "rebase"
"#,
        );
        let cfg = ForgeConfig::load_from(&nested, Some(&toplevel));
        assert_eq!(cfg.merge_method, None);
        assert!(cfg.source_path.is_none());
    }

    #[test]
    fn unknown_top_level_key_emits_one_warning() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            r#"
[merge]
method = "squash"

[experimental]
flag = true
"#,
        );
        let cfg = ForgeConfig::load_from(tmp.path(), Some(tmp.path()));
        assert_eq!(cfg.merge_method, Some(MergeMethod::Squash));
        assert_eq!(cfg.warnings, vec!["unknown-config-key:experimental"]);
    }

    #[test]
    fn unknown_section_key_emits_one_warning_per_key() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            r#"
[merge]
method = "squash"
mystery_key = 7
another_unknown = "x"
"#,
        );
        let cfg = ForgeConfig::load_from(tmp.path(), Some(tmp.path()));
        let mut warnings = cfg.warnings.clone();
        warnings.sort();
        assert_eq!(
            warnings,
            vec![
                "unknown-config-key:merge.another_unknown".to_string(),
                "unknown-config-key:merge.mystery_key".to_string(),
            ]
        );
    }

    #[test]
    fn invalid_merge_method_value_warns_and_falls_back() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            r#"[merge]
method = "fast-forward"
"#,
        );
        let cfg = ForgeConfig::load_from(tmp.path(), Some(tmp.path()));
        assert_eq!(cfg.merge_method, None);
        assert_eq!(
            cfg.resolve_merge_method(None),
            MergeMethod::Squash,
            "must fall back to spec default"
        );
        assert!(
            cfg.warnings
                .iter()
                .any(|w| w == "invalid-config-value:merge.method:fast-forward"),
            "warnings={:?}",
            cfg.warnings
        );
    }

    #[test]
    fn invalid_toml_yields_warning_not_error() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "not = valid = toml\n");
        let cfg = ForgeConfig::load_from(tmp.path(), Some(tmp.path()));
        assert!(
            cfg.warnings
                .iter()
                .any(|w| w.starts_with("invalid-config-value:parse_error:"))
        );
        // Resolution still falls back to spec defaults.
        assert_eq!(cfg.resolve_merge_method(None), MergeMethod::Squash);
    }

    #[test]
    fn resolution_precedence_explicit_wins_over_config_and_default() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            r###"
[merge]
method = "merge"
delete_branch = false

[body]
summary_heading = "## Overview"
test_plan_heading = "## Verification"

[branch]
feature_prefix = "feature/"
bug_prefix = "bugfix/"

[checks]
timeout = "10m"
interval = "5s"
required_only = false
"###,
        );
        let cfg = ForgeConfig::load_from(tmp.path(), Some(tmp.path()));
        // merge.method: explicit > config > default
        assert_eq!(
            cfg.resolve_merge_method(Some(MergeMethod::Rebase)),
            MergeMethod::Rebase
        );
        assert_eq!(cfg.resolve_merge_method(None), MergeMethod::Merge);
        // merge.delete_branch
        assert!(cfg.resolve_delete_branch(Some(true)));
        assert!(!cfg.resolve_delete_branch(None));
        // body headings
        assert_eq!(
            cfg.resolve_summary_heading(Some("## Why")),
            "## Why".to_string()
        );
        assert_eq!(cfg.resolve_summary_heading(None), "## Overview".to_string());
        assert_eq!(
            cfg.resolve_test_plan_heading(Some("## How")),
            "## How".to_string()
        );
        assert_eq!(
            cfg.resolve_test_plan_heading(None),
            "## Verification".to_string()
        );
        // branch prefixes
        assert_eq!(
            cfg.resolve_feature_prefix(Some("feat/")),
            "feat/".to_string()
        );
        assert_eq!(cfg.resolve_feature_prefix(None), "feature/".to_string());
        assert_eq!(cfg.resolve_bug_prefix(Some("fix/")), "fix/".to_string());
        assert_eq!(cfg.resolve_bug_prefix(None), "bugfix/".to_string());
        // checks timing
        assert_eq!(
            cfg.resolve_checks_timeout(Some(Duration::from_secs(120))),
            Duration::from_secs(120)
        );
        assert_eq!(
            cfg.resolve_checks_timeout(None),
            Duration::from_secs(10 * 60)
        );
        assert_eq!(
            cfg.resolve_checks_interval(Some(Duration::from_secs(2))),
            Duration::from_secs(2)
        );
        assert_eq!(cfg.resolve_checks_interval(None), Duration::from_secs(5));
        // checks.required_only
        assert!(cfg.resolve_required_only(Some(true)));
        assert!(!cfg.resolve_required_only(None));
    }

    #[test]
    fn defaults_apply_when_neither_explicit_nor_config_set() {
        let cfg = ForgeConfig::default();
        assert_eq!(cfg.resolve_merge_method(None), MergeMethod::Squash);
        assert!(cfg.resolve_delete_branch(None));
        assert_eq!(cfg.resolve_summary_heading(None), "## Summary".to_string());
        assert_eq!(
            cfg.resolve_test_plan_heading(None),
            "## Test plan".to_string()
        );
        assert_eq!(cfg.resolve_feature_prefix(None), "feat/".to_string());
        assert_eq!(cfg.resolve_bug_prefix(None), "fix/".to_string());
        assert_eq!(
            cfg.resolve_checks_timeout(None),
            Duration::from_secs(30 * 60)
        );
        assert_eq!(cfg.resolve_checks_interval(None), Duration::from_secs(20));
        assert!(cfg.resolve_required_only(None));
        assert_eq!(
            cfg.resolve_review_convergence(None),
            ReviewConvergencePolicy {
                require: false,
                quiet_period: Duration::from_secs(2 * 60),
                timeout: Duration::from_secs(20 * 60),
                bots: Vec::new(),
            }
        );
    }

    #[test]
    fn review_convergence_repo_layer_cannot_downgrade_an_enabled_global_gate() {
        let global = ForgeConfig {
            review_convergence_required: Some(true),
            review_convergence_quiet_period: Some(Duration::from_secs(300)),
            review_convergence_timeout: Some(Duration::from_secs(1800)),
            review_convergence_bots: Some(vec![ReviewConvergenceBot {
                login: "global-bot".to_string(),
                mode: ReviewBotMode::Observed,
            }]),
            ..ForgeConfig::default()
        };
        let repo = ForgeConfig {
            review_convergence_required: Some(false),
            review_convergence_quiet_period: Some(Duration::from_secs(120)),
            review_convergence_bots: Some(vec![ReviewConvergenceBot {
                login: "repo-bot".to_string(),
                mode: ReviewBotMode::Observed,
            }]),
            ..ForgeConfig::default()
        };
        let layered = global.overlaid_by(repo);

        let repo_policy = layered.resolve_review_convergence(None);
        assert!(repo_policy.require);
        assert_eq!(repo_policy.quiet_period, Duration::from_secs(300));
        assert_eq!(repo_policy.timeout, Duration::from_secs(1800));
        assert_eq!(repo_policy.bots.len(), 2);
        assert!(repo_policy.bots.iter().any(|bot| bot.login == "global-bot"));
        assert!(repo_policy.bots.iter().any(|bot| bot.login == "repo-bot"));

        let explicit_policy = layered.resolve_review_convergence(Some(false));
        assert!(!explicit_policy.require);
    }

    #[test]
    fn enabled_global_gate_preserves_an_explicit_zero_quiet_period_when_repo_omits_it() {
        let global = ForgeConfig {
            review_convergence_required: Some(true),
            review_convergence_quiet_period: Some(Duration::ZERO),
            ..ForgeConfig::default()
        };

        let policy = global
            .overlaid_by(ForgeConfig::default())
            .resolve_review_convergence(None);
        assert!(policy.require);
        assert_eq!(policy.quiet_period, Duration::ZERO);
    }

    #[test]
    fn invalid_repo_bot_list_does_not_clear_a_valid_global_policy() {
        let global = ForgeConfig {
            review_convergence_required: Some(true),
            review_convergence_bots: Some(vec![ReviewConvergenceBot {
                login: "global-bot".to_string(),
                mode: ReviewBotMode::Observed,
            }]),
            ..ForgeConfig::default()
        };
        let value: Value = toml::from_str(
            r#"
[[review_convergence.bots]]
login = ""
mode = "unsupported"
"#,
        )
        .expect("valid TOML");
        let repo = parse_value(&value);
        assert!(!repo.warnings.is_empty());

        let policy = global.overlaid_by(repo).resolve_review_convergence(None);
        assert!(policy.require);
        assert_eq!(policy.bots.len(), 1);
        assert_eq!(policy.bots[0].login, "global-bot");
    }

    #[test]
    fn intentional_empty_repo_bot_list_still_clears_the_global_list() {
        let global = ForgeConfig {
            review_convergence_bots: Some(vec![ReviewConvergenceBot {
                login: "global-bot".to_string(),
                mode: ReviewBotMode::Observed,
            }]),
            ..ForgeConfig::default()
        };
        let value: Value = toml::from_str("[review_convergence]\nbots = []\n").expect("valid TOML");
        let repo = parse_value(&value);
        assert!(
            global
                .overlaid_by(repo)
                .resolve_review_convergence(None)
                .bots
                .is_empty()
        );
    }

    #[test]
    fn excessive_review_convergence_durations_are_rejected() {
        let value: Value = toml::from_str(
            r#"
[review_convergence]
quiet_period = "3601s"
timeout = "86401s"
"#,
        )
        .expect("valid TOML");
        let cfg = parse_value(&value);
        assert_eq!(cfg.review_convergence_quiet_period, None);
        assert_eq!(cfg.review_convergence_timeout, None);
        assert!(
            cfg.warnings
                .iter()
                .any(|warning| warning.contains("quiet_period") && warning.contains("exceeds_max"))
        );
        assert!(
            cfg.warnings
                .iter()
                .any(|warning| warning.contains("timeout") && warning.contains("exceeds_max"))
        );
    }

    #[test]
    fn overflowing_review_convergence_durations_are_rejected_without_panicking() {
        let value: Value = toml::from_str(
            r#"
[review_convergence]
require = true
quiet_period = "1152921504606846976h"
timeout = "1152921504606846976h"
"#,
        )
        .expect("valid TOML");
        let cfg = parse_value(&value);
        assert_eq!(cfg.review_convergence_quiet_period, None);
        assert_eq!(cfg.review_convergence_timeout, None);
        assert!(
            cfg.warnings
                .iter()
                .any(|warning| warning.contains("quiet_period"))
        );
        assert!(
            cfg.warnings
                .iter()
                .any(|warning| warning.contains("timeout"))
        );
    }

    #[test]
    fn bad_scalar_type_yields_invalid_warning_not_panic() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            r#"
[merge]
method = 42
delete_branch = "yes"

[checks]
required_only = 1
timeout = 42
"#,
        );
        let cfg = ForgeConfig::load_from(tmp.path(), Some(tmp.path()));
        let mut wanted = vec![
            "invalid-config-value:merge.method:not_a_string".to_string(),
            "invalid-config-value:merge.delete_branch:not_a_bool".to_string(),
            "invalid-config-value:checks.required_only:not_a_bool".to_string(),
            "invalid-config-value:checks.timeout:not_a_string".to_string(),
        ];
        wanted.sort();
        let mut got = cfg.warnings.clone();
        got.sort();
        assert_eq!(got, wanted);
        // All values fall back to defaults via resolver.
        assert_eq!(cfg.resolve_merge_method(None), MergeMethod::Squash);
        assert!(cfg.resolve_delete_branch(None));
        assert!(cfg.resolve_required_only(None));
        assert_eq!(
            cfg.resolve_checks_timeout(None),
            Duration::from_secs(30 * 60)
        );
    }

    #[test]
    fn merge_method_from_str_round_trip() {
        for m in [MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase] {
            assert_eq!(MergeMethod::from_str(m.as_str()), Ok(m));
        }
        assert!(MergeMethod::from_str("Squash").is_err());
        assert!(MergeMethod::from_str("").is_err());
    }
}
