use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};
use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit};
use nils_common::fs::display_path;
use nils_common::redact::redact_text;
use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::{
    PluginArgs, PluginCommand, PluginFetchArgs, PluginMaybeUpdateArgs, PluginStatusArgs,
    PluginUpdateArgs,
};

const PLUGIN_SCHEMA_VERSION: &str = "cli.zsh-kit.plugin.v1";
const PLUGIN_ERROR_SCHEMA_VERSION: &str = "cli.zsh-kit.plugin.error.v1";
const DEFAULT_INTERVAL_DAYS: u64 = 7;

pub fn run(args: PluginArgs) -> i32 {
    match args.command {
        PluginCommand::Fetch(args) => render_plugin(args.format, fetch_plugin(args)),
        PluginCommand::Update(args) => render_plugin(args.format, update_plugins(args)),
        PluginCommand::MaybeUpdate(args) => render_plugin(args.format, maybe_update_plugins(args)),
        PluginCommand::Status(args) => render_plugin(args.format, plugin_status(args)),
    }
}

fn fetch_plugin(args: PluginFetchArgs) -> Result<PluginResult, PluginError> {
    let plugins_dir = resolve_plugins_dir(args.plugins_dir.as_deref())?;
    let entry = parse_plugin_entry(&args.entry)?;
    let plugin_path = plugins_dir.join(&entry.plugin_id);

    let mut result = PluginResult::new("fetch");
    result.plugin_id = Some(entry.plugin_id.clone());
    result.git_url = entry.git_url.clone();
    result.plugins_dir = Some(display_path(&plugins_dir));
    result.dry_run = args.dry_run;
    result.force = args.force;

    if args.force && plugin_path.is_dir() {
        result
            .messages
            .push(format!("💥 Forcing re-clone: {}", entry.plugin_id));
        result.actions.push(PluginAction::new(
            "remove",
            "remove existing plugin directory before re-clone",
            true,
            Some(display_path(&plugin_path)),
            None,
            if args.dry_run { "planned" } else { "applied" },
        ));
        if !args.dry_run {
            fs::remove_dir_all(&plugin_path)
                .map_err(|err| plugin_io_error("io-error", &plugin_path, err))?;
        }
    }

    if plugin_path.is_dir() {
        result.mutation_status = "unchanged".to_string();
        result.skipped = 1;
        result.actions.push(PluginAction::new(
            "exists",
            "plugin directory already exists",
            false,
            Some(display_path(&plugin_path)),
            None,
            "skipped",
        ));
        return Ok(result);
    }

    let Some(git_url) = &entry.git_url else {
        result
            .messages
            .push(format!("⚠️  No git URL defined for: {}", entry.plugin_id));
        result.mutation_status = "unchanged".to_string();
        result.skipped = 1;
        return Ok(result);
    };

    result
        .messages
        .push(format!("🌐 Cloning {} from {}", entry.plugin_id, git_url));
    result.actions.push(PluginAction::new(
        "clone",
        "clone missing plugin repository",
        true,
        Some(display_path(&plugin_path)),
        Some(format!(
            "git clone {} {}",
            redacted(git_url),
            display_path(&plugin_path)
        )),
        if args.dry_run { "planned" } else { "applied" },
    ));

    if args.dry_run {
        result.mutation_status = "planned".to_string();
        return Ok(result);
    }

    if let Some(parent) = plugin_path.parent() {
        fs::create_dir_all(parent).map_err(|err| plugin_io_error("io-error", parent, err))?;
    }

    let output = git_output(&["clone", git_url, &display_path(&plugin_path)], None)?;
    if !output.status.success() {
        return Err(git_failed(
            &["clone", "<git-url>", "<plugin-path>"],
            &output,
            Some("failed to clone plugin repository"),
        ));
    }

    if plugin_path.join(".gitmodules").is_file() {
        result.messages.push(format!(
            "🔗 Initializing submodules for {}",
            entry.plugin_id
        ));
        result.actions.push(PluginAction::new(
            "submodules",
            "initialize plugin submodules",
            true,
            Some(display_path(&plugin_path)),
            Some("git submodule update --init --recursive".to_string()),
            "applied",
        ));
        let output = git_output(
            &["submodule", "update", "--init", "--recursive"],
            Some(&plugin_path),
        )?;
        if !output.status.success() {
            return Err(git_failed(
                &["submodule", "update", "--init", "--recursive"],
                &output,
                Some("failed to initialize plugin submodules"),
            ));
        }
    }

    result.mutation_status = "applied".to_string();
    result.updated = 1;
    Ok(result)
}

fn update_plugins(args: PluginUpdateArgs) -> Result<PluginResult, PluginError> {
    let plugins_dir = resolve_plugins_dir(args.plugins_dir.as_deref())?;
    update_plugins_impl(&plugins_dir, args.dry_run)
}

fn update_plugins_impl(plugins_dir: &Path, dry_run: bool) -> Result<PluginResult, PluginError> {
    let mut result = PluginResult::new("update");
    result.plugins_dir = Some(display_path(plugins_dir));
    result.dry_run = dry_run;

    if !plugins_dir.is_dir() {
        result.mutation_status = "unchanged".to_string();
        return Ok(result);
    }

    result.messages.push(format!(
        "🔄 Updating plugins in: {}",
        display_path(plugins_dir)
    ));
    result.messages.push(String::new());

    for plugin_dir in git_plugin_dirs(plugins_dir)? {
        let plugin_name = plugin_dir
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("<unknown>")
            .to_string();
        result
            .messages
            .push(format!("🔧 Updating {plugin_name} ..."));

        if dry_run {
            result.messages.push(format!(
                "    ↪ [dry-run] git -C {} pull --ff-only",
                display_path(&plugin_dir)
            ));
            result.actions.push(PluginAction::new(
                "pull",
                "fast-forward plugin repository",
                true,
                Some(display_path(&plugin_dir)),
                Some("git pull --ff-only".to_string()),
                "planned",
            ));
            result.skipped += 1;
            continue;
        }

        let before = git_head(&plugin_dir).unwrap_or_default();
        let output = match git_output(&["pull", "--ff-only"], Some(&plugin_dir)) {
            Ok(output) => output,
            Err(err) => {
                result.failed += 1;
                result.failures.push(PluginFailure {
                    plugin_id: plugin_name,
                    message: err.message,
                });
                continue;
            }
        };
        let combined = combined_output(&output);

        if output.status.success() {
            let after = git_head(&plugin_dir).unwrap_or_default();
            let short_after = after.chars().take(7).collect::<String>();
            if !before.is_empty() && before == after {
                if combined.contains("Already up to date") {
                    result
                        .messages
                        .push(format!("    ↪ Already up to date. (at {short_after})"));
                } else {
                    push_prefixed_lines(&mut result.messages, &combined, "    ↪ ");
                }
                result.unchanged += 1;
            } else {
                result
                    .messages
                    .push(format!("    ↪ Updated to {short_after}"));
                result.updated += 1;
            }
            result.actions.push(PluginAction::new(
                "pull",
                "fast-forward plugin repository",
                true,
                Some(display_path(&plugin_dir)),
                Some("git pull --ff-only".to_string()),
                "applied",
            ));
        } else {
            push_prefixed_lines(&mut result.messages, &combined, "    ❌ ");
            result
                .messages
                .push(format!("    ❌ Failed to update {plugin_name}"));
            result.failed += 1;
            result.failures.push(PluginFailure {
                plugin_id: plugin_name,
                message: combined.trim().to_string(),
            });
            result.actions.push(PluginAction::new(
                "pull",
                "fast-forward plugin repository",
                true,
                Some(display_path(&plugin_dir)),
                Some("git pull --ff-only".to_string()),
                "failed",
            ));
        }
    }

    result.mutation_status = if dry_run {
        "planned"
    } else if result.updated > 0 {
        "applied"
    } else {
        "unchanged"
    }
    .to_string();
    Ok(result)
}

fn maybe_update_plugins(args: PluginMaybeUpdateArgs) -> Result<PluginResult, PluginError> {
    let plugins_dir = resolve_plugins_dir(args.plugins_dir.as_deref())?;
    let timestamp_file = resolve_timestamp_file(args.timestamp_file.as_deref())?;
    let interval_days = resolve_interval_days(args.interval_days)?;
    let now = now_epoch()?;
    let last = read_epoch_file(&timestamp_file).unwrap_or(0);
    let due = now - last > interval_days.saturating_mul(86_400) as i64;

    if !due {
        let mut result = PluginResult::new("maybe-update");
        result.plugins_dir = Some(display_path(&plugins_dir));
        result.timestamp_file = Some(display_path(&timestamp_file));
        result.interval_days = Some(interval_days);
        result.last_epoch = Some(last);
        result.now_epoch = Some(now);
        result.update_due = Some(false);
        result.dry_run = args.dry_run;
        result.mutation_status = "unchanged".to_string();
        return Ok(result);
    }

    let mut result = update_plugins_impl(&plugins_dir, args.dry_run)?;
    result.command = "maybe-update".to_string();
    result.timestamp_file = Some(display_path(&timestamp_file));
    result.interval_days = Some(interval_days);
    result.last_epoch = Some(last);
    result.now_epoch = Some(now);
    result.update_due = Some(true);
    result.messages.insert(0, String::new());
    result.messages.insert(
        0,
        format!("📦 Auto-updating Zsh plugins (last update over {interval_days} days ago)..."),
    );

    if !args.dry_run {
        write_epoch_file(&timestamp_file, now)?;
        result.actions.push(PluginAction::new(
            "timestamp",
            "write plugin auto-update timestamp",
            true,
            Some(display_path(&timestamp_file)),
            None,
            "applied",
        ));
    }

    Ok(result)
}

fn plugin_status(args: PluginStatusArgs) -> Result<PluginResult, PluginError> {
    let timestamp_file = resolve_timestamp_file(args.timestamp_file.as_deref())?;
    let interval_days = resolve_interval_days(args.interval_days)?;
    let mut result = PluginResult::new("status");
    result.timestamp_file = Some(display_path(&timestamp_file));
    result.interval_days = Some(interval_days);
    result.mutation_status = "reported".to_string();

    if !timestamp_file.is_file() {
        result
            .messages
            .push("📦 Plugin update status: never updated".to_string());
        result
            .messages
            .push("⏱  Next auto-update expected: now".to_string());
        result.update_due = Some(true);
        return Ok(result);
    }

    let now = now_epoch()?;
    let last = read_epoch_file(&timestamp_file).unwrap_or(0);
    let days_ago = (now - last) / 86_400;
    let days_left = interval_days as i64 - days_ago;
    let last_date = format_epoch_date(last);

    result.last_epoch = Some(last);
    result.now_epoch = Some(now);
    result.days_ago = Some(days_ago);
    result.days_left = Some(days_left);
    result.last_date = Some(last_date.clone());
    result.update_due = Some(days_left <= 0);

    result.messages.push(format!(
        "📦 Plugin last updated: {last_date} ({days_ago} days ago)"
    ));
    if days_left <= 0 {
        result
            .messages
            .push("⏱  Next auto-update expected: now".to_string());
    } else {
        result
            .messages
            .push(format!("⏱  Next auto-update expected in: {days_left} days"));
    }
    Ok(result)
}

fn parse_plugin_entry(entry: &str) -> Result<ParsedPluginEntry, PluginError> {
    let parts: Vec<&str> = entry.split("::").collect();
    let plugin_id = trim(parts.first().copied().unwrap_or_default()).to_string();
    if !valid_plugin_id(&plugin_id) {
        return Err(PluginError::data(
            "invalid-plugin-entry",
            format!(
                "plugin_fetch_if_missing_from_entry: invalid plugin id in entry: {}",
                if entry.is_empty() { "<empty>" } else { entry }
            ),
            Some(json!({ "entry": entry })),
        ));
    }

    let git_url = parts
        .iter()
        .skip(1)
        .find_map(|part| part.strip_prefix("git="))
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);

    Ok(ParsedPluginEntry { plugin_id, git_url })
}

fn valid_plugin_id(plugin_id: &str) -> bool {
    !plugin_id.is_empty()
        && plugin_id != "."
        && plugin_id != ".."
        && plugin_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

fn trim(value: &str) -> &str {
    value.trim_matches(|ch: char| ch.is_whitespace())
}

fn git_plugin_dirs(plugins_dir: &Path) -> Result<Vec<PathBuf>, PluginError> {
    let read_dir =
        fs::read_dir(plugins_dir).map_err(|err| plugin_io_error("io-error", plugins_dir, err))?;
    let mut dirs = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|err| plugin_io_error("io-error", plugins_dir, err))?;
        let path = entry.path();
        if path.is_dir() && path.join(".git").is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn resolve_plugins_dir(path: Option<&Path>) -> Result<PathBuf, PluginError> {
    if let Some(path) = path {
        return Ok(expand_home(path));
    }
    if let Some(path) = non_empty_env_path("ZSH_PLUGINS_DIR") {
        return Ok(expand_home(&path));
    }
    if let Some(zdotdir) = non_empty_env_path("ZDOTDIR") {
        return Ok(expand_home(&zdotdir).join("plugins"));
    }
    if let Ok(home) = home_dir() {
        return Ok(home.join(".config/zsh/plugins"));
    }
    Err(PluginError::data(
        "plugin-dir-not-set",
        "plugin directory is required (pass --plugins-dir or set ZSH_PLUGINS_DIR)",
        None,
    ))
}

fn resolve_timestamp_file(path: Option<&Path>) -> Result<PathBuf, PluginError> {
    if let Some(path) = path {
        return Ok(expand_home(path));
    }
    if let Some(path) = non_empty_env_path("PLUGIN_UPDATE_FILE") {
        return Ok(expand_home(&path));
    }
    if let Some(cache_dir) = non_empty_env_path("ZSH_CACHE_DIR") {
        return Ok(expand_home(&cache_dir).join("plugin.timestamp"));
    }
    if let Ok(home) = home_dir() {
        return Ok(home.join(".cache/zsh/plugin.timestamp"));
    }
    Err(PluginError::data(
        "timestamp-file-not-set",
        "timestamp file is required (pass --timestamp-file or set ZSH_CACHE_DIR)",
        None,
    ))
}

fn resolve_interval_days(value: Option<u64>) -> Result<u64, PluginError> {
    if let Some(value) = value {
        return non_zero_interval(value);
    }
    match env::var("PLUGIN_UPDATE_INTERVAL_DAYS") {
        Ok(raw) if !raw.trim().is_empty() => {
            let parsed = raw.trim().parse::<u64>().map_err(|err| {
                PluginError::data(
                    "invalid-interval-days",
                    format!("invalid PLUGIN_UPDATE_INTERVAL_DAYS: {err}"),
                    Some(json!({ "value": raw })),
                )
            })?;
            non_zero_interval(parsed)
        }
        _ => Ok(DEFAULT_INTERVAL_DAYS),
    }
}

fn non_zero_interval(value: u64) -> Result<u64, PluginError> {
    if value == 0 {
        return Err(PluginError::data(
            "invalid-interval-days",
            "plugin update interval must be greater than zero",
            Some(json!({ "value": value })),
        ));
    }
    Ok(value)
}

fn non_empty_env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Result<PathBuf, PluginError> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| PluginError::data("home-not-set", "HOME is required", None))
}

fn expand_home(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~"
        && let Ok(home) = home_dir()
    {
        return home;
    }
    if let Some(rest) = text.strip_prefix("~/")
        && let Ok(home) = home_dir()
    {
        return home.join(rest);
    }
    path.to_path_buf()
}

fn now_epoch() -> Result<i64, PluginError> {
    if let Ok(raw) = env::var("ZSH_KIT_PLUGIN_NOW_EPOCH")
        && !raw.trim().is_empty()
    {
        return raw.trim().parse::<i64>().map_err(|err| {
            PluginError::data(
                "invalid-now-epoch",
                format!("invalid ZSH_KIT_PLUGIN_NOW_EPOCH: {err}"),
                Some(json!({ "value": raw })),
            )
        });
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| {
            PluginError::runtime("clock-error", format!("system clock error: {err}"), None)
        })?;
    Ok(now.as_secs() as i64)
}

fn read_epoch_file(path: &Path) -> Option<i64> {
    fs::read_to_string(path).ok()?.trim().parse::<i64>().ok()
}

fn write_epoch_file(path: &Path, epoch: i64) -> Result<(), PluginError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| plugin_io_error("io-error", parent, err))?;
    }
    fs::write(path, format!("{epoch}\n")).map_err(|err| plugin_io_error("io-error", path, err))
}

fn format_epoch_date(epoch: i64) -> String {
    Local
        .timestamp_opt(epoch, 0)
        .single()
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| epoch.to_string())
}

fn git_head(cwd: &Path) -> Option<String> {
    let output = git_output(&["rev-parse", "HEAD"], Some(cwd)).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_output(args: &[&str], cwd: Option<&Path>) -> Result<std::process::Output, PluginError> {
    let mut command = ProcessCommand::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| {
            PluginError::runtime(
                "git-command-failed",
                format!("failed to spawn git: {err}"),
                None,
            )
        })
}

fn git_failed(args: &[&str], output: &std::process::Output, prefix: Option<&str>) -> PluginError {
    let mut message = format!(
        "git {} failed with status {}",
        args.join(" "),
        output.status
    );
    if let Some(prefix) = prefix {
        message = format!("{prefix}: {message}");
    }
    PluginError::runtime(
        "git-command-failed",
        message,
        Some(json!({
            "args": args,
            "stdout": redacted(&String::from_utf8_lossy(&output.stdout)),
            "stderr": redacted(&String::from_utf8_lossy(&output.stderr)),
        })),
    )
}

fn combined_output(output: &std::process::Output) -> String {
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    if !output.stderr.is_empty() {
        if !combined.ends_with('\n') && !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    combined
}

fn push_prefixed_lines(messages: &mut Vec<String>, text: &str, prefix: &str) {
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return;
    }
    for line in trimmed.lines() {
        messages.push(format!("{prefix}{line}"));
    }
}

fn plugin_io_error(code: &'static str, path: &Path, err: io::Error) -> PluginError {
    PluginError::runtime(
        code,
        format!("{}: {err}", display_path(path)),
        Some(json!({ "path": display_path(path) })),
    )
}

fn render_plugin(format: OutputFormat, result: Result<PluginResult, PluginError>) -> i32 {
    match result {
        Ok(result) => render_success(format, &result),
        Err(err) => render_error(format, err),
    }
}

fn render_success(format: OutputFormat, result: &PluginResult) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope = Envelope::success(PLUGIN_SCHEMA_VERSION, result);
            print_json(&envelope)
        }
        OutputFormat::Text => {
            for message in &result.messages {
                println!("{message}");
            }
            exit::SUCCESS
        }
    }
}

fn render_error(format: OutputFormat, err: PluginError) -> i32 {
    match format {
        OutputFormat::Json => {
            let mut envelope_error = EnvelopeError::new(err.code, err.message);
            if let Some(details) = err.details {
                envelope_error = envelope_error.with_details(details);
            }
            let envelope: Envelope<()> =
                Envelope::failure(PLUGIN_ERROR_SCHEMA_VERSION, envelope_error);
            let _ = print_json(&envelope);
        }
        OutputFormat::Text => {
            let redacted = redacted(&err.message);
            let _ = writeln!(io::stderr(), "error: {redacted}");
        }
    }
    err.exit_code
}

fn print_json<T: Serialize>(envelope: &Envelope<T>) -> i32 {
    match serde_json::to_string(envelope) {
        Ok(serialized) => {
            println!("{serialized}");
            exit::SUCCESS
        }
        Err(_) => exit::SOFTWARE,
    }
}

fn redacted(value: &str) -> String {
    redact_text(value).value
}

#[derive(Debug)]
struct ParsedPluginEntry {
    plugin_id: String,
    git_url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct PluginResult {
    command: String,
    mutation_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    plugin_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plugins_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    interval_days: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_epoch: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    now_epoch: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    days_ago: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    days_left: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    update_due: Option<bool>,
    dry_run: bool,
    force: bool,
    updated: usize,
    unchanged: usize,
    skipped: usize,
    failed: usize,
    actions: Vec<PluginAction>,
    failures: Vec<PluginFailure>,
    messages: Vec<String>,
}

impl PluginResult {
    fn new(command: &str) -> Self {
        Self {
            command: command.to_string(),
            mutation_status: "not-started".to_string(),
            plugin_id: None,
            git_url: None,
            plugins_dir: None,
            timestamp_file: None,
            interval_days: None,
            last_epoch: None,
            now_epoch: None,
            days_ago: None,
            days_left: None,
            last_date: None,
            update_due: None,
            dry_run: false,
            force: false,
            updated: 0,
            unchanged: 0,
            skipped: 0,
            failed: 0,
            actions: Vec::new(),
            failures: Vec::new(),
            messages: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct PluginAction {
    kind: String,
    description: String,
    mutation: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    status: String,
}

impl PluginAction {
    fn new(
        kind: impl Into<String>,
        description: impl Into<String>,
        mutation: bool,
        path: Option<String>,
        command: Option<String>,
        status: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            description: description.into(),
            mutation,
            path,
            command,
            status: status.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct PluginFailure {
    plugin_id: String,
    message: String,
}

#[derive(Debug)]
struct PluginError {
    code: &'static str,
    message: String,
    exit_code: i32,
    details: Option<Value>,
}

impl PluginError {
    fn data(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: exit::DATA,
            details,
        }
    }

    fn runtime(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: exit::RUNTIME,
            details,
        }
    }
}
