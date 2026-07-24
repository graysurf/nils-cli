//! Governance-projected Codex supervisor runtime for `agent run`.
//!
//! The Execution Capsule supervisor needs repository instructions, lifecycle
//! hooks, command rules, authentication, and shell tooling. It does not need
//! arbitrary external MCP tools, whose startup adds latency, credential
//! refreshes, and failure modes unrelated to the prepared operation.
//!
//! This runtime is deliberately *not* [`super::isolated`]: that runtime removes
//! instructions, hooks, and rules, which the Execution Capsule contract must
//! preserve. Instead it builds a private child `CODEX_HOME` whose generated
//! `config.toml` is a fresh allowlist projection containing only the hook
//! feature and hook tables, and launches Codex with explicit plugin and app
//! capability disables so no direct, plugin-provided, or app-provided MCP
//! server can start.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::child_home::{self, ChildHome};

/// Codex features whose discovery can reintroduce an MCP server or an external
/// tool surface the prepared capsule did not declare.
pub const DISABLED_FEATURES: [&str; 4] =
    ["plugins", "remote_plugin", "apps", "workspace_dependencies"];
/// Codex `exec` flags the projected supervisor launch depends on.
const REQUIRED_FLAGS: [&str; 3] = ["--disable", "--ephemeral", "--skip-git-repo-check"];
/// Codex features the projected supervisor must be able to name explicitly.
/// `hooks` is required positively: the projection keeps lifecycle hooks active.
const REQUIRED_FEATURES: [&str; 5] = [
    "hooks",
    "plugins",
    "remote_plugin",
    "apps",
    "workspace_dependencies",
];
/// Top-level project-config tables that can introduce MCP, plugin, or app
/// authority the supervisor cannot prove disabled.
const REJECTED_PROJECT_TABLES: [&str; 5] = [
    "mcp_servers",
    "plugins",
    "apps",
    "connectors",
    "marketplace",
];
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_INSTRUCTION_BYTES: u64 = 256 * 1024;
const MAX_HOOKS_JSON_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
pub struct SupervisorError {
    pub code: &'static str,
    pub message: String,
    pub next_action: &'static str,
    pub retryable: &'static str,
}

impl SupervisorError {
    fn unsupported(message: String) -> Self {
        Self {
            code: "capsule-supervisor-unsupported",
            message,
            next_action: "upgrade Codex or rerun with --mcp-mode inherited",
            retryable: "no",
        }
    }

    fn home_failed(message: String) -> Self {
        Self {
            code: "capsule-supervisor-home-failed",
            message,
            next_action: "repair local runtime or temporary directory permissions",
            retryable: "usually",
        }
    }

    fn config_invalid(message: String) -> Self {
        Self {
            code: "capsule-supervisor-config-invalid",
            message,
            next_action: "repair the malformed hook or project configuration",
            retryable: "no",
        }
    }

    fn project_mcp(message: String) -> Self {
        Self {
            code: "capsule-project-mcp-undeclared",
            message,
            next_action: "remove project MCP for this run or rerun with --mcp-mode inherited",
            retryable: "no",
        }
    }
}

/// A prepared private supervisor home plus the project-config identities that
/// were checked during preflight.
pub struct SupervisorHome {
    home: ChildHome,
    project_configs: Vec<CheckedConfig>,
}

struct CheckedConfig {
    path: PathBuf,
    identity: (u64, u64, u64, i64, i64),
}

impl SupervisorHome {
    /// Probe Codex capabilities, reject project configuration that could start
    /// MCP, then build the private governance projection.
    pub fn prepare(cwd: &Path) -> Result<Self, SupervisorError> {
        probe_capabilities()?;
        let source_home = child_home::original_codex_home();
        let source_config = source_home.join("config.toml");
        let project_configs = check_project_configs(cwd, &source_config)?;

        let home = ChildHome::create("codex-cli-capsule-supervisor-")
            .map_err(SupervisorError::home_failed)?;
        home.bridge_auth(child_home::auth_source(&source_home).as_deref())
            .map_err(SupervisorError::home_failed)?;
        project_instructions(&source_home, home.path())?;
        project_config(&source_config, home.path())?;
        project_hooks_json(&source_home, home.path())?;
        Ok(Self {
            home,
            project_configs,
        })
    }

    pub fn path(&self) -> &Path {
        self.home.path()
    }

    /// Re-verify that no checked project config was swapped between preflight
    /// and launch.
    pub fn verify_project_configs(&self) -> Result<(), SupervisorError> {
        for checked in &self.project_configs {
            let file = open_no_follow(&checked.path).map_err(|error| {
                SupervisorError::config_invalid(format!(
                    "project configuration {} became unreadable after preflight: {error}",
                    checked.path.display()
                ))
            })?;
            let metadata = file.metadata().map_err(|error| {
                SupervisorError::config_invalid(format!(
                    "cannot inspect project configuration {}: {error}",
                    checked.path.display()
                ))
            })?;
            if config_identity(&metadata) != checked.identity {
                return Err(SupervisorError::config_invalid(format!(
                    "project configuration {} changed after preflight",
                    checked.path.display()
                )));
            }
        }
        Ok(())
    }

    /// Point the child at the private home and drop control-plane environment.
    pub fn apply(&self, command: &mut Command) {
        command.env("CODEX_HOME", self.home.path());
        child_home::remove_control_environment(command);
    }

    pub fn warn_if_auth_replaced(&self, stderr: &mut impl Write) {
        child_home::warn_if_auth_replaced(self.home.path(), stderr);
    }
}

fn probe_capabilities() -> Result<(), SupervisorError> {
    let help_words = child_home::codex_exec_help_words();
    let feature_names = child_home::codex_feature_names();
    let missing = REQUIRED_FLAGS
        .iter()
        .filter(|flag| !help_words.contains(**flag))
        .chain(
            REQUIRED_FEATURES
                .iter()
                .filter(|feature| !feature_names.contains(**feature)),
        )
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(SupervisorError::unsupported(format!(
        "installed Codex cannot enforce the no-MCP supervisor contract; missing: {}",
        missing.join(", ")
    )))
}

fn project_instructions(source_home: &Path, child_home: &Path) -> Result<(), SupervisorError> {
    let source = source_home.join("AGENTS.md");
    // Follows a symlinked instruction source on purpose: the active home
    // instructions are the governance input. Only bounded bytes are copied, so
    // the child never holds a writable handle on the operator's source.
    let Ok(metadata) = fs::metadata(&source) else {
        return Ok(());
    };
    if !metadata.is_file() {
        return Ok(());
    }
    if metadata.len() > MAX_INSTRUCTION_BYTES {
        return Err(SupervisorError::config_invalid(format!(
            "home instructions {} exceed {MAX_INSTRUCTION_BYTES} bytes",
            source.display()
        )));
    }
    let bytes = fs::read(&source).map_err(|error| {
        SupervisorError::config_invalid(format!(
            "cannot read home instructions {}: {error}",
            source.display()
        ))
    })?;
    write_private_file(child_home, "AGENTS.md", &bytes)
}

fn project_config(source_config: &Path, child_home: &Path) -> Result<(), SupervisorError> {
    let child_config = child_home.join("config.toml");
    let source = match read_bounded_config(source_config, MAX_CONFIG_BYTES)? {
        Some(text) => text,
        None => return write_private_file(child_home, "config.toml", b""),
    };
    let parsed = source.parse::<toml::Table>().map_err(|error| {
        SupervisorError::config_invalid(format!(
            "cannot parse {}: {error}",
            source_config.display()
        ))
    })?;
    let projection = project_governance_table(&parsed, source_config, &child_config);
    let rendered = toml::to_string(&projection).map_err(|error| {
        SupervisorError::config_invalid(format!(
            "cannot render the projected supervisor configuration: {error}"
        ))
    })?;
    write_private_file(child_home, "config.toml", rendered.as_bytes())
}

/// Build a fresh governance projection.
///
/// Only the hook feature switch and the hook table are carried over. Hook trust
/// state keys embed the declaring config path, so they are rekeyed onto the
/// child config path; otherwise the projected hooks would load untrusted.
/// Nothing else is preserved: no `mcp_servers`, plugin, app, connector, skill,
/// memory, goal, subagent, or notification registration, and no unrelated user
/// interface configuration.
fn project_governance_table(
    source: &toml::Table,
    source_config: &Path,
    child_config: &Path,
) -> toml::Table {
    let mut output = toml::Table::new();
    if let Some(hooks) = source
        .get("features")
        .and_then(toml::Value::as_table)
        .and_then(|features| features.get("hooks"))
    {
        let mut features = toml::Table::new();
        features.insert("hooks".to_string(), hooks.clone());
        output.insert("features".to_string(), toml::Value::Table(features));
    }
    if let Some(hooks) = source.get("hooks").and_then(toml::Value::as_table) {
        let mut projected = hooks.clone();
        if let Some(state) = projected.get("state").and_then(toml::Value::as_table) {
            let source_prefix = format!("{}:", source_config.display());
            let child_prefix = format!("{}:", child_config.display());
            let rekeyed = state
                .iter()
                .map(|(key, value)| match key.strip_prefix(&source_prefix) {
                    Some(rest) => (format!("{child_prefix}{rest}"), value.clone()),
                    None => (key.clone(), value.clone()),
                })
                .collect::<toml::Table>();
            projected.insert("state".to_string(), toml::Value::Table(rekeyed));
        }
        output.insert("hooks".to_string(), toml::Value::Table(projected));
    }
    output
}

fn project_hooks_json(source_home: &Path, child_home: &Path) -> Result<(), SupervisorError> {
    let source = source_home.join("hooks.json");
    let Ok(metadata) = fs::symlink_metadata(&source) else {
        return Ok(());
    };
    if !metadata.is_file() {
        return Ok(());
    }
    if metadata.len() > MAX_HOOKS_JSON_BYTES {
        return Err(SupervisorError::config_invalid(format!(
            "provider hook file {} exceeds {MAX_HOOKS_JSON_BYTES} bytes",
            source.display()
        )));
    }
    let bytes = fs::read(&source).map_err(|error| {
        SupervisorError::config_invalid(format!(
            "cannot read provider hook file {}: {error}",
            source.display()
        ))
    })?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| {
        SupervisorError::config_invalid(format!(
            "cannot parse provider hook file {}: {error}",
            source.display()
        ))
    })?;
    if !value.is_object() {
        return Err(SupervisorError::config_invalid(format!(
            "provider hook file {} must contain a JSON object",
            source.display()
        )));
    }
    write_private_file(child_home, "hooks.json", &bytes)
}

/// Validate every project config Codex could apply for this run.
///
/// The check is fail-closed: a project config that declares MCP, plugin, or app
/// authority is rejected instead of assumed harmless, because the explicit CLI
/// feature disables have not been proven to stop every applicable project MCP.
fn check_project_configs(
    cwd: &Path,
    source_config: &Path,
) -> Result<Vec<CheckedConfig>, SupervisorError> {
    let canonical_source = fs::canonicalize(source_config).ok();
    let mut checked = Vec::new();
    for candidate in project_config_candidates(cwd) {
        let Ok(canonical) = fs::canonicalize(&candidate) else {
            continue;
        };
        if canonical_source.as_deref() == Some(canonical.as_path()) {
            continue;
        }
        let file = open_no_follow(&candidate).map_err(|error| {
            SupervisorError::config_invalid(format!(
                "cannot securely read project configuration {}: {error}",
                candidate.display()
            ))
        })?;
        let metadata = file.metadata().map_err(|error| {
            SupervisorError::config_invalid(format!(
                "cannot inspect project configuration {}: {error}",
                candidate.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(SupervisorError::config_invalid(format!(
                "project configuration {} is not a regular file",
                candidate.display()
            )));
        }
        if metadata.len() > MAX_CONFIG_BYTES {
            return Err(SupervisorError::config_invalid(format!(
                "project configuration {} exceeds {MAX_CONFIG_BYTES} bytes",
                candidate.display()
            )));
        }
        let mut text = String::new();
        let mut file = file;
        file.read_to_string(&mut text).map_err(|error| {
            SupervisorError::config_invalid(format!(
                "cannot read project configuration {}: {error}",
                candidate.display()
            ))
        })?;
        let parsed = text.parse::<toml::Table>().map_err(|error| {
            SupervisorError::config_invalid(format!(
                "cannot parse project configuration {}: {error}",
                candidate.display()
            ))
        })?;
        if let Some(declaration) = undeclared_authority(&parsed) {
            return Err(SupervisorError::project_mcp(format!(
                "project configuration {} declares {declaration}, which the no-MCP supervisor cannot prove disabled",
                candidate.display()
            )));
        }
        checked.push(CheckedConfig {
            path: candidate,
            identity: config_identity(&metadata),
        });
    }
    Ok(checked)
}

/// Name the first rejected declaration, never its value.
fn undeclared_authority(config: &toml::Table) -> Option<String> {
    for table in REJECTED_PROJECT_TABLES {
        if config.contains_key(table) {
            return Some(table.to_string());
        }
    }
    let features = config.get("features").and_then(toml::Value::as_table)?;
    DISABLED_FEATURES
        .iter()
        .find(|feature| features.get(**feature).and_then(toml::Value::as_bool) == Some(true))
        .map(|feature| format!("features.{feature}"))
}

/// Project config paths Codex could apply, bounded by the runner's accepted
/// working directory and its enclosing repository.
fn project_config_candidates(cwd: &Path) -> Vec<PathBuf> {
    let boundary = git_toplevel(cwd);
    let mut candidates = vec![cwd.join(".codex").join("config.toml")];
    if let Some(boundary) = boundary {
        let mut current = cwd.parent();
        while let Some(directory) = current {
            if !directory.starts_with(&boundary) {
                break;
            }
            candidates.push(directory.join(".codex").join("config.toml"));
            if directory == boundary {
                break;
            }
            current = directory.parent();
        }
    }
    candidates
}

fn git_toplevel(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
    fs::canonicalize(path).ok()
}

fn read_bounded_config(path: &Path, limit: u64) -> Result<Option<String>, SupervisorError> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(None);
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    if metadata.len() > limit {
        return Err(SupervisorError::config_invalid(format!(
            "{} exceeds {limit} bytes",
            path.display()
        )));
    }
    let text = fs::read_to_string(path).map_err(|error| {
        SupervisorError::config_invalid(format!("cannot read {}: {error}", path.display()))
    })?;
    Ok(Some(text))
}

fn open_no_follow(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

fn config_identity(metadata: &fs::Metadata) -> (u64, u64, u64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.size(),
        metadata.mtime(),
        metadata.mtime_nsec(),
    )
}

/// Write owner-only bytes into the private child home through a temporary file
/// and rename, so a partially written projection is never launched.
fn write_private_file(home: &Path, name: &str, bytes: &[u8]) -> Result<(), SupervisorError> {
    let target = home.join(name);
    let temporary = home.join(format!(".{name}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| {
            SupervisorError::home_failed(format!(
                "cannot create the projected {name} in the private supervisor home: {error}"
            ))
        })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            SupervisorError::home_failed(format!("cannot write the projected {name}: {error}"))
        })?;
    drop(file);
    fs::rename(&temporary, &target).map_err(|error| {
        SupervisorError::home_failed(format!("cannot publish the projected {name}: {error}"))
    })?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).map_err(|error| {
        SupervisorError::home_failed(format!(
            "cannot restrict the projected {name} to its owner: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn table(text: &str) -> toml::Table {
        text.parse::<toml::Table>().expect("parse toml")
    }

    #[test]
    fn governance_projection_keeps_only_hook_governance() {
        let source = table(
            r#"
model = "gpt-5"
notify = ["notify-send"]

[features]
hooks = true
memories = true

[tools]
web_search = true

[mcp_servers.atlassian-rovo]
url = "https://example.invalid/mcp"
bearer_token = "SECRET-SENTINEL"

[plugins."vendor.plugin".mcp_servers.rovo]
command = "rovo"

[apps.connector]
token = "SECRET-SENTINEL"

[[hooks.PreToolUse]]

[[hooks.PreToolUse.hooks]]
type = "command"
command = "agent-hook dispatch --product codex"

[hooks.state."/source/config.toml:pre_tool_use:0:0"]
trusted_hash = "abc"
enabled = true
"#,
        );

        let projected = project_governance_table(
            &source,
            Path::new("/source/config.toml"),
            Path::new("/child/config.toml"),
        );

        assert_eq!(
            projected.keys().collect::<Vec<_>>(),
            vec!["features", "hooks"]
        );
        assert_eq!(projected["features"]["hooks"].as_bool(), Some(true));
        assert!(projected["features"].as_table().expect("features").len() == 1);
        assert!(projected["hooks"]["PreToolUse"].is_array());
        let state = projected["hooks"]["state"].as_table().expect("state");
        assert_eq!(
            state.keys().collect::<Vec<_>>(),
            vec!["/child/config.toml:pre_tool_use:0:0"]
        );

        let rendered = toml::to_string(&projected).expect("render projection");
        for excluded in [
            "mcp_servers",
            "plugins",
            "apps",
            "SECRET-SENTINEL",
            "web_search",
            "notify",
            "memories",
            "gpt-5",
        ] {
            assert!(
                !rendered.contains(excluded),
                "projected config must not contain {excluded}: {rendered}"
            );
        }
    }

    #[test]
    fn governance_projection_renders_scalars_beside_tables() {
        let source = table(
            r#"
[features]
hooks = true

[[hooks.PreToolUse]]

[[hooks.PreToolUse.hooks]]
type = "command"
command = "true"

[hooks]
timeout = 5
"#,
        );

        let projected = project_governance_table(
            &source,
            Path::new("/source/config.toml"),
            Path::new("/child/config.toml"),
        );
        let rendered = toml::to_string(&projected).expect("render projection with scalar");
        assert!(rendered.contains("timeout = 5"), "{rendered}");
        let reparsed = table(&rendered);
        assert_eq!(reparsed["hooks"]["timeout"].as_integer(), Some(5));
    }

    #[test]
    fn governance_projection_is_empty_without_hook_governance() {
        let source = table("model = \"gpt-5\"\n\n[mcp_servers.example]\ncommand = \"x\"\n");
        let projected = project_governance_table(
            &source,
            Path::new("/source/config.toml"),
            Path::new("/child/config.toml"),
        );
        assert!(projected.is_empty());
    }

    #[test]
    fn undeclared_authority_names_rejected_declarations_only() {
        assert_eq!(
            undeclared_authority(&table("[mcp_servers.example]\ncommand = \"x\"\n")),
            Some("mcp_servers".to_string())
        );
        assert_eq!(
            undeclared_authority(&table("[plugins.\"vendor.plugin\"]\nenabled = true\n")),
            Some("plugins".to_string())
        );
        assert_eq!(
            undeclared_authority(&table("[features]\nplugins = true\n")),
            Some("features.plugins".to_string())
        );
        assert_eq!(
            undeclared_authority(&table("[features]\nplugins = false\nhooks = true\n")),
            None
        );
    }

    #[test]
    fn undeclared_authority_ignores_benign_lookalike_keys() {
        let benign = table(
            r#"
mcp_servers_notes = "documented elsewhere"
apps_review_reminder = true

[tools]
mcp_servers = "this is a nested key, not a server table"

[hooks.state."/repo/.codex/config.toml:pre_tool_use:0:0"]
trusted_hash = "abc"

[profiles.plugins]
model = "gpt-5"
"#,
        );
        assert_eq!(undeclared_authority(&benign), None);
    }
}
