use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use nils_common::cli_contract::{Envelope, EnvelopeError, schema_version_for};
use serde::{Deserialize, Serialize};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::cli::{
    ConfigArgs, ConfigCommand, ConfigEnrollArgs, ConfigFormatArgs, ConfigRemoveArgs, ConfigRuleArgs,
};
use crate::env::{PathOverrides, ProjectIdentity, resolve_project_identity};
use crate::model::OutputFormat;

const CONFIG_SCHEMA_VERSION: u32 = 1;
const MAX_REASON_BYTES: usize = 500;
const MAX_CONFIG_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_PRIVATE_CATALOG_BYTES: usize = 1024 * 1024;
const MAX_PRIVATE_CATALOG_ENTRIES: usize = 1024;
const MAX_SELECTED_RULES: usize = 2;
const CONFIG_DIR_MODE: u32 = 0o700;
const CONFIG_FILE_MODE: u32 = 0o600;
const TEMP_CREATE_ATTEMPTS: u64 = 128;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectorKind {
    ProjectPath,
    GitCommonDir,
}

impl SelectorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProjectPath => "project-path",
            Self::GitCommonDir => "git-common-dir",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleMode {
    Enroll,
    Exclude,
}

impl RuleMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enroll => "enroll",
            Self::Exclude => "exclude",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRule {
    #[serde(rename = "match")]
    pub selector: SelectorKind,
    pub path: PathBuf,
    pub mode: RuleMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub project: Vec<ProjectRule>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigState {
    Missing,
    Valid,
    Invalid,
    Insecure,
    Unreadable,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigDiagnostic {
    pub code: String,
    pub message: String,
}

#[derive(Debug)]
pub enum ConfigRead {
    Missing,
    Valid(UserConfig),
    Fault {
        state: ConfigState,
        diagnostic: ConfigDiagnostic,
    },
}

#[derive(Debug)]
pub(crate) struct SecureFile {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigOperationErrorKind {
    Correctable,
    Runtime,
}

#[derive(Debug)]
struct ConfigOperationError {
    kind: ConfigOperationErrorKind,
    error: anyhow::Error,
}

impl ConfigOperationError {
    fn correctable(error: impl Into<anyhow::Error>) -> Self {
        Self {
            kind: ConfigOperationErrorKind::Correctable,
            error: error.into(),
        }
    }

    fn runtime(error: impl Into<anyhow::Error>) -> Self {
        Self {
            kind: ConfigOperationErrorKind::Runtime,
            error: error.into(),
        }
    }
}

impl fmt::Display for ConfigOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:#}", self.error)
    }
}

impl std::error::Error for ConfigOperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

type ConfigResult<T> = std::result::Result<T, ConfigOperationError>;

#[derive(Debug, Clone, Serialize)]
pub struct ConfigEntryView {
    #[serde(rename = "match")]
    pub selector: SelectorKind,
    pub path: PathBuf,
    pub mode: RuleMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl From<&ProjectRule> for ConfigEntryView {
    fn from(rule: &ProjectRule) -> Self {
        Self {
            selector: rule.selector,
            path: rule.path.clone(),
            mode: rule.mode,
            catalog: rule.catalog.clone(),
            reason: rule.reason.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ConfigReport {
    config_path: PathBuf,
    applied: bool,
    action: &'static str,
    entries: Vec<ConfigEntryView>,
}

pub fn run(args: ConfigArgs, overrides: &PathOverrides) -> i32 {
    let (command, format) = config_command_output(&args);
    match run_inner(args, overrides) {
        Ok(()) => 0,
        Err(err) => render_config_error(command, format, &err),
    }
}

fn config_command_output(args: &ConfigArgs) -> (&'static str, OutputFormat) {
    match &args.command {
        ConfigCommand::Enroll(args) => ("enroll", args.format),
        ConfigCommand::Exclude(args) => ("exclude", args.format),
        ConfigCommand::Show(args) => ("show", args.format),
        ConfigCommand::List(args) => ("list", args.format),
        ConfigCommand::Remove(args) => ("remove", args.format),
    }
}

fn render_config_error(command: &str, format: OutputFormat, err: &ConfigOperationError) -> i32 {
    let message = err.to_string().replace('\n', " ");
    match format {
        OutputFormat::Json => {
            let envelope: Envelope<()> = Envelope::failure(
                schema_version_for("agent-docs", &format!("config.{command}"), 1),
                EnvelopeError::new("config-operation-failed", message),
            );
            match serde_json::to_string(&envelope) {
                Ok(serialized) => println!("{serialized}"),
                Err(serialize_err) => eprintln!("error: {serialize_err:#}"),
            }
        }
        OutputFormat::Text => eprintln!("error: {message}"),
    }
    match err.kind {
        ConfigOperationErrorKind::Correctable => 3,
        ConfigOperationErrorKind::Runtime => 4,
    }
}

fn run_inner(args: ConfigArgs, overrides: &PathOverrides) -> ConfigResult<()> {
    match args.command {
        ConfigCommand::List(args) => {
            let identity =
                resolve_identity(overrides).map_err(ConfigOperationError::correctable)?;
            list(args, &identity)
        }
        ConfigCommand::Show(args) => {
            let identity =
                resolve_identity(overrides).map_err(ConfigOperationError::correctable)?;
            show(args, &identity)
        }
        ConfigCommand::Enroll(args) => {
            let identity =
                resolve_identity(overrides).map_err(ConfigOperationError::correctable)?;
            enroll(args, &identity)
        }
        ConfigCommand::Exclude(args) => {
            let identity =
                resolve_identity(overrides).map_err(ConfigOperationError::correctable)?;
            mutate_rule(args, &identity, RuleMode::Exclude, None, "exclude")
        }
        ConfigCommand::Remove(args) => {
            let identity =
                resolve_identity(overrides).map_err(ConfigOperationError::correctable)?;
            remove(args, &identity)
        }
    }
}

fn resolve_identity(overrides: &PathOverrides) -> Result<ProjectIdentity> {
    resolve_project_identity(overrides.project_path.as_deref())
}

fn list(args: ConfigFormatArgs, identity: &ProjectIdentity) -> ConfigResult<()> {
    let path = config_path().map_err(ConfigOperationError::correctable)?;
    let entries = match read_config_at(&path, identity) {
        ConfigRead::Missing => Vec::new(),
        ConfigRead::Valid(config) => config.project.iter().map(ConfigEntryView::from).collect(),
        ConfigRead::Fault { diagnostic, .. } => {
            return Err(ConfigOperationError::correctable(anyhow!(
                diagnostic.message
            )));
        }
    };
    render_config(
        args.format,
        "list",
        ConfigReport {
            config_path: path,
            applied: false,
            action: "list",
            entries,
        },
    )
    .map_err(ConfigOperationError::correctable)
}

fn show(args: ConfigFormatArgs, identity: &ProjectIdentity) -> ConfigResult<()> {
    let path = config_path().map_err(ConfigOperationError::correctable)?;
    let entries = match read_config_at(&path, identity) {
        ConfigRead::Missing => Vec::new(),
        ConfigRead::Valid(config) => matching_rules(&config, identity)
            .into_iter()
            .map(ConfigEntryView::from)
            .collect(),
        ConfigRead::Fault { diagnostic, .. } => {
            return Err(ConfigOperationError::correctable(anyhow!(
                diagnostic.message
            )));
        }
    };
    render_config(
        args.format,
        "show",
        ConfigReport {
            config_path: path,
            applied: false,
            action: "show",
            entries,
        },
    )
    .map_err(ConfigOperationError::correctable)
}

fn enroll(args: ConfigEnrollArgs, identity: &ProjectIdentity) -> ConfigResult<()> {
    let catalog = validate_catalog_for_enrollment(&args.catalog, identity)
        .map_err(ConfigOperationError::correctable)?;
    mutate_rule(
        ConfigRuleArgs {
            all_worktrees: args.all_worktrees,
            reason: args.reason,
            apply: args.apply,
            format: args.format,
        },
        identity,
        RuleMode::Enroll,
        Some(catalog),
        "enroll",
    )
}

fn mutate_rule(
    args: ConfigRuleArgs,
    identity: &ProjectIdentity,
    mode: RuleMode,
    catalog: Option<PathBuf>,
    action: &'static str,
) -> ConfigResult<()> {
    let path = config_path().map_err(ConfigOperationError::correctable)?;
    validate_config_destination(&path, identity).map_err(ConfigOperationError::correctable)?;
    let selector =
        selector_for(identity, args.all_worktrees).map_err(ConfigOperationError::correctable)?;
    let reason = validate_reason(args.reason).map_err(ConfigOperationError::correctable)?;
    let rule = ProjectRule {
        selector: selector.0,
        path: selector.1,
        mode,
        catalog,
        reason,
    };

    let _ = update_document(&path, &rule).map_err(ConfigOperationError::correctable)?;
    if args.apply {
        let _lock = ConfigLock::acquire(&path, identity).map_err(ConfigOperationError::runtime)?;
        let proposed = update_document(&path, &rule).map_err(ConfigOperationError::correctable)?;
        write_config(&path, proposed.as_bytes(), identity)
            .map_err(ConfigOperationError::runtime)?;
    }

    render_config(
        args.format,
        action,
        ConfigReport {
            config_path: path,
            applied: args.apply,
            action,
            entries: vec![ConfigEntryView::from(&rule)],
        },
    )
    .map_err(ConfigOperationError::correctable)
}

fn remove(args: ConfigRemoveArgs, identity: &ProjectIdentity) -> ConfigResult<()> {
    let path = config_path().map_err(ConfigOperationError::correctable)?;
    validate_config_destination(&path, identity).map_err(ConfigOperationError::correctable)?;
    let selector =
        selector_for(identity, args.all_worktrees).map_err(ConfigOperationError::correctable)?;
    let _ = remove_from_document(&path, selector.0, &selector.1)
        .map_err(ConfigOperationError::correctable)?;
    if args.apply {
        let _lock = ConfigLock::acquire(&path, identity).map_err(ConfigOperationError::runtime)?;
        let proposed = remove_from_document(&path, selector.0, &selector.1)
            .map_err(ConfigOperationError::correctable)?;
        write_config(&path, proposed.as_bytes(), identity)
            .map_err(ConfigOperationError::runtime)?;
    }
    render_config(
        args.format,
        "remove",
        ConfigReport {
            config_path: path,
            applied: args.apply,
            action: "remove",
            entries: Vec::new(),
        },
    )
    .map_err(ConfigOperationError::correctable)
}

fn render_config(format: OutputFormat, command: &str, report: ConfigReport) -> Result<()> {
    match format {
        OutputFormat::Json => {
            let envelope = Envelope::success(
                schema_version_for("agent-docs", &format!("config.{command}"), 1),
                report,
            );
            println!("{}", serde_json::to_string(&envelope)?);
            Ok(())
        }
        OutputFormat::Text => {
            println!(
                "CONFIG: action={} applied={} path={} entries={}",
                report.action,
                report.applied,
                report.config_path.display(),
                report.entries.len()
            );
            Ok(())
        }
    }
}

pub fn config_path() -> Result<PathBuf> {
    let path =
        if let Some(raw) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
            let root = PathBuf::from(raw);
            if !root.is_absolute() {
                bail!("XDG_CONFIG_HOME must be an absolute path");
            }
            root.join("agent-docs/config.toml")
        } else {
            let home = crate::env::home_dir().ok_or_else(|| anyhow!("HOME is unset"))?;
            if !home.is_absolute() {
                bail!("HOME must be an absolute path when XDG_CONFIG_HOME is unset");
            }
            home.join(".config/agent-docs/config.toml")
        };
    require_utf8_path(&path, "user config path")?;
    Ok(path)
}

pub fn read_config_for_identity(identity: &ProjectIdentity) -> Result<ConfigRead> {
    Ok(read_config_at(&config_path()?, identity))
}

fn read_config_at(path: &Path, identity: &ProjectIdentity) -> ConfigRead {
    let snapshot = match secure_read_file(path, MAX_CONFIG_BYTES) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return ConfigRead::Missing,
        Err(err) => {
            return ConfigRead::Fault {
                state: if err.kind() == io::ErrorKind::PermissionDenied {
                    ConfigState::Insecure
                } else {
                    ConfigState::Unreadable
                },
                diagnostic: ConfigDiagnostic {
                    code: "insecure-user-config".to_string(),
                    message: err.to_string(),
                },
            };
        }
    };

    if let Err(err) = ensure_outside_protected_git_roots(&snapshot.path, identity, "user config") {
        return ConfigRead::Fault {
            state: ConfigState::Insecure,
            diagnostic: ConfigDiagnostic {
                code: "insecure-user-config".to_string(),
                message: err.to_string(),
            },
        };
    }

    let raw = match String::from_utf8(snapshot.bytes) {
        Ok(raw) => raw,
        Err(err) => {
            return ConfigRead::Fault {
                state: ConfigState::Invalid,
                diagnostic: ConfigDiagnostic {
                    code: "invalid-user-config".to_string(),
                    message: format!("user config is not valid UTF-8: {err}"),
                },
            };
        }
    };
    match parse_config(path, &raw) {
        Ok(config) => ConfigRead::Valid(config),
        Err(message) => ConfigRead::Fault {
            state: ConfigState::Invalid,
            diagnostic: ConfigDiagnostic {
                code: "invalid-user-config".to_string(),
                message,
            },
        },
    }
}

fn parse_config(path: &Path, raw: &str) -> std::result::Result<UserConfig, String> {
    let config: UserConfig = toml::from_str(raw).map_err(|err| {
        let location = err
            .span()
            .map(|span| sanitized_line_column(raw, span.start))
            .map(|(line, column)| format!(" at line {line}, column {column}"))
            .unwrap_or_default();
        format!("invalid user config TOML {}{location}", path.display())
    })?;
    if config.schema_version != CONFIG_SCHEMA_VERSION {
        return Err(format!(
            "unsupported user config schema_version {}; expected {CONFIG_SCHEMA_VERSION}",
            config.schema_version
        ));
    }
    for (index, rule) in config.project.iter().enumerate() {
        validate_rule(rule).map_err(|message| format!("project rule {}: {message}", index + 1))?;
    }
    Ok(config)
}

fn validate_rule(rule: &ProjectRule) -> std::result::Result<(), String> {
    if !rule.path.is_absolute() {
        return Err("path must be absolute".to_string());
    }
    if path_has_forbidden_components(&rule.path) {
        return Err("path must use a normalized absolute spelling without `.` or `..`".to_string());
    }
    match rule.mode {
        RuleMode::Enroll if rule.catalog.is_none() => {
            return Err("enrollment requires catalog".to_string());
        }
        RuleMode::Exclude if rule.catalog.is_some() => {
            return Err("exclusion must not declare catalog".to_string());
        }
        _ => {}
    }
    if let Some(catalog) = &rule.catalog {
        if !catalog.is_absolute() {
            return Err("catalog must be absolute".to_string());
        }
        if path_has_forbidden_components(catalog) {
            return Err(
                "catalog must use a normalized absolute spelling without `.` or `..`".to_string(),
            );
        }
    }
    if let Some(reason) = &rule.reason {
        validate_reason(Some(reason.clone())).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn path_has_forbidden_components(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}

fn validate_reason(reason: Option<String>) -> Result<Option<String>> {
    let Some(reason) = reason else {
        return Ok(None);
    };
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        bail!("reason must not be empty");
    }
    if trimmed.len() > MAX_REASON_BYTES {
        bail!("reason must be at most {MAX_REASON_BYTES} bytes");
    }
    Ok(Some(trimmed.to_string()))
}

pub fn matching_rules<'a>(
    config: &'a UserConfig,
    identity: &'a ProjectIdentity,
) -> Vec<&'a ProjectRule> {
    matching_rules_iter(config, identity).collect()
}

pub(crate) fn matching_rules_for_selection<'a>(
    config: &'a UserConfig,
    identity: &'a ProjectIdentity,
) -> Vec<&'a ProjectRule> {
    matching_rules_iter(config, identity)
        .take(MAX_SELECTED_RULES)
        .collect()
}

fn matching_rules_iter<'a>(
    config: &'a UserConfig,
    identity: &'a ProjectIdentity,
) -> impl Iterator<Item = &'a ProjectRule> + 'a {
    config.project.iter().filter(|rule| match rule.selector {
        SelectorKind::ProjectPath => rule.path == identity.project_path,
        SelectorKind::GitCommonDir => identity
            .git_common_dir
            .as_ref()
            .is_some_and(|path| path == &rule.path),
    })
}

fn selector_for(
    identity: &ProjectIdentity,
    all_worktrees: bool,
) -> Result<(SelectorKind, PathBuf)> {
    if all_worktrees {
        let common = identity
            .git_common_dir
            .as_ref()
            .ok_or_else(|| anyhow!("--all-worktrees requires a Git repository"))?;
        let canonical = fs::canonicalize(common).with_context(|| {
            format!("failed to canonicalize Git common dir {}", common.display())
        })?;
        require_utf8_path(&canonical, "Git common-dir selector")?;
        return Ok((SelectorKind::GitCommonDir, canonical));
    }
    require_utf8_path(&identity.project_path, "project-path selector")?;
    Ok((SelectorKind::ProjectPath, identity.project_path.clone()))
}

pub(crate) fn read_selected_catalog(path: &Path, identity: &ProjectIdentity) -> Result<SecureFile> {
    if !path.is_absolute() {
        bail!("catalog path must be absolute");
    }
    if path_has_forbidden_components(path) {
        bail!("catalog path must use a normalized absolute spelling without `.` or `..`");
    }
    require_utf8_path(path, "private catalog path")?;
    let snapshot = secure_read_file(path, MAX_PRIVATE_CATALOG_BYTES)
        .with_context(|| format!("catalog {} is not secure", path.display()))?
        .ok_or_else(|| anyhow!("catalog {} does not exist", path.display()))?;
    require_utf8_path(&snapshot.path, "private catalog identity")?;
    ensure_outside_protected_git_roots(&snapshot.path, identity, "private catalog")?;
    Ok(snapshot)
}

pub(crate) fn parse_selected_catalog(
    snapshot: &SecureFile,
    identity: &ProjectIdentity,
) -> Result<crate::model::ScopeCatalog> {
    let raw = std::str::from_utf8(&snapshot.bytes).context("private catalog is not valid UTF-8")?;
    let catalog = crate::config::load_scope_catalog_from_str(
        crate::model::Scope::Project,
        crate::model::CatalogOrigin::User,
        &identity.project_path,
        &snapshot.path,
        raw,
    )
    .map_err(sanitized_private_catalog_error)?;
    let entry_count = catalog
        .documents
        .len()
        .checked_add(catalog.validations.len())
        .ok_or_else(|| anyhow!("private catalog entry count overflow"))?;
    if entry_count > MAX_PRIVATE_CATALOG_ENTRIES {
        bail!("private catalog exceeds the {MAX_PRIVATE_CATALOG_ENTRIES}-entry selection limit");
    }
    Ok(catalog)
}

fn sanitized_private_catalog_error(err: crate::model::ConfigLoadError) -> anyhow::Error {
    use crate::model::ConfigErrorKind;

    match err.kind {
        ConfigErrorKind::Parse => anyhow!("invalid private catalog TOML"),
        ConfigErrorKind::Validation
            if err.message == "private/user catalog documents require project scope" =>
        {
            anyhow!("private/user catalog documents require project scope")
        }
        ConfigErrorKind::Validation => anyhow!("private catalog validation failed"),
        ConfigErrorKind::Io => anyhow!("private catalog could not be loaded"),
    }
}

fn validate_catalog_for_enrollment(path: &Path, identity: &ProjectIdentity) -> Result<PathBuf> {
    let snapshot = read_selected_catalog(path, identity)?;
    let _ = parse_selected_catalog(&snapshot, identity)?;
    Ok(snapshot.path)
}

pub(crate) fn secure_read_file(path: &Path, max_bytes: usize) -> io::Result<Option<SecureFile>> {
    reject_unresolved_symlink_ancestors(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} final path must not be a symlink", path.display()),
            ));
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} must be a regular file", path.display()),
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(not(unix))]
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "secure config and private-catalog reads are unsupported on this platform",
        ));
    }
    #[cfg(unix)]
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    #[cfg(unix)]
    let opened_metadata = file.metadata()?;
    #[cfg(unix)]
    validate_secure_file_metadata(path, &opened_metadata)?;
    #[cfg(unix)]
    let canonical = fs::canonicalize(path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let path_metadata = fs::metadata(&canonical)?;
        if opened_metadata.dev() != path_metadata.dev()
            || opened_metadata.ino() != path_metadata.ino()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} changed while it was opened", path.display()),
            ));
        }
    }

    #[cfg(unix)]
    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    #[cfg(unix)]
    let mut bytes = Vec::new();
    #[cfg(unix)]
    std::io::Read::by_ref(&mut file)
        .take(limit)
        .read_to_end(&mut bytes)?;
    #[cfg(unix)]
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} exceeds the {max_bytes}-byte size limit", path.display()),
        ));
    }
    #[cfg(unix)]
    Ok(Some(SecureFile {
        path: canonical,
        bytes,
    }))
}

fn validate_secure_file_metadata(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} must be a regular file and not a symlink",
                path.display()
            ),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} must be owned by the current user", path.display()),
            ));
        }
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} must not be group/world writable", path.display()),
            ));
        }
    }
    Ok(())
}

fn reject_unresolved_symlink_ancestors(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let resolved = fs::canonicalize(&current).map_err(|err| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "config parent contains an unresolved symlink at {}: {err}",
                            current.display()
                        ),
                    )
                })?;
                if !resolved.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        format!("{} does not resolve to a directory", current.display()),
                    ));
                }
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!("{} is not a directory", current.display()),
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => break,
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn update_document(path: &Path, rule: &ProjectRule) -> Result<String> {
    let mut doc = load_document(path)?;
    let tables = project_tables_mut(&mut doc)?;
    retain_other_rules(tables, rule.selector, &rule.path);
    tables.push(rule_table(rule)?);
    bounded_rendered_config(doc)
}

fn remove_from_document(path: &Path, selector: SelectorKind, rule_path: &Path) -> Result<String> {
    let mut doc = load_document(path)?;
    let tables = project_tables_mut(&mut doc)?;
    retain_other_rules(tables, selector, rule_path);
    bounded_rendered_config(doc)
}

fn bounded_rendered_config(doc: DocumentMut) -> Result<String> {
    let rendered = doc.to_string();
    if rendered.len() > MAX_CONFIG_BYTES {
        bail!("user config exceeds the {MAX_CONFIG_BYTES}-byte rendered-config limit");
    }
    Ok(rendered)
}

fn load_document(path: &Path) -> Result<DocumentMut> {
    let Some(snapshot) = secure_read_file(path, MAX_CONFIG_BYTES)
        .with_context(|| format!("failed to read {}", path.display()))?
    else {
        return "schema_version = 1\n"
            .parse::<DocumentMut>()
            .context("failed to initialize user config");
    };
    let raw = String::from_utf8(snapshot.bytes).context("user config is not valid UTF-8")?;
    parse_config(path, &raw).map_err(anyhow::Error::msg)?;
    raw.parse::<DocumentMut>()
        .map_err(|_| anyhow!("failed to edit user config TOML {}", path.display()))
}

fn project_tables_mut(doc: &mut DocumentMut) -> Result<&mut ArrayOfTables> {
    if doc.get("project").is_none() {
        doc["project"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    doc["project"]
        .as_array_of_tables_mut()
        .ok_or_else(|| anyhow!("project must be an array of tables"))
}

fn retain_other_rules(tables: &mut ArrayOfTables, selector: SelectorKind, path: &Path) {
    let mut index = tables.len();
    while index > 0 {
        index -= 1;
        let table = tables.get(index).expect("array-of-tables index");
        let same_selector = table
            .get("match")
            .and_then(Item::as_str)
            .is_some_and(|value| value == selector.as_str());
        let same_path = table
            .get("path")
            .and_then(Item::as_str)
            .is_some_and(|value| Path::new(value) == path);
        if same_selector && same_path {
            tables.remove(index);
        }
    }
}

fn rule_table(rule: &ProjectRule) -> Result<Table> {
    let mut table = Table::new();
    table["match"] = value(rule.selector.as_str());
    table["path"] = value(
        rule.path
            .to_str()
            .ok_or_else(|| anyhow!("selector path must be valid UTF-8"))?,
    );
    table["mode"] = value(rule.mode.as_str());
    if let Some(catalog) = &rule.catalog {
        table["catalog"] = value(
            catalog
                .to_str()
                .ok_or_else(|| anyhow!("catalog identity must be valid UTF-8"))?,
        );
    }
    if let Some(reason) = &rule.reason {
        table["reason"] = value(reason);
    }
    Ok(table)
}

fn validate_config_destination(path: &Path, identity: &ProjectIdentity) -> Result<()> {
    if !path.is_absolute() || path_has_forbidden_components(path) {
        bail!("user config path must use a normalized absolute spelling");
    }
    require_utf8_path(path, "user config path")?;
    reject_unresolved_symlink_ancestors(path)
        .with_context(|| format!("user config path {} is not secure", path.display()))?;
    reject_final_symlink_or_nonregular(path, "user config")?;
    let canonical = canonicalize_intended_path(path)?;
    ensure_outside_protected_git_roots(&canonical, identity, "user config")
}

fn require_utf8_path(path: &Path, description: &str) -> Result<()> {
    if path.to_str().is_none() {
        bail!("{description} must be valid UTF-8");
    }
    Ok(())
}

fn sanitized_line_column(raw: &str, byte_offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;
    for (index, character) in raw.char_indices() {
        if index >= byte_offset.min(raw.len()) {
            break;
        }
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

fn protected_git_roots(identity: &ProjectIdentity) -> Vec<PathBuf> {
    let mut roots = vec![identity.project_path.clone()];
    if let Some(common) = &identity.git_common_dir {
        roots.push(common.clone());
    }
    roots.extend(identity.worktree_roots.iter().cloned());
    roots.sort();
    roots.dedup();
    roots
}

fn ensure_outside_protected_git_roots(
    canonical_path: &Path,
    identity: &ProjectIdentity,
    description: &str,
) -> Result<()> {
    if protected_git_roots(identity)
        .iter()
        .any(|root| canonical_path.starts_with(root))
    {
        bail!("{description} must remain outside every target Git worktree and common dir");
    }
    Ok(())
}

fn canonicalize_intended_path(path: &Path) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(canonical) => return Ok(canonical),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to canonicalize path {}", path.display()));
        }
    }
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("path has no final component"))?;
    let parent = path.parent().ok_or_else(|| anyhow!("path has no parent"))?;
    let mut unresolved = Vec::new();
    let mut cursor = parent;
    let canonical_parent = loop {
        match fs::canonicalize(cursor) {
            Ok(canonical) => break canonical,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let name = cursor.file_name().ok_or_else(|| {
                    anyhow!("failed to resolve path parent {}: {err}", parent.display())
                })?;
                unresolved.push(name.to_os_string());
                cursor = cursor.parent().ok_or_else(|| {
                    anyhow!("failed to resolve path parent {}: {err}", parent.display())
                })?;
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to resolve path parent {}", parent.display())
                });
            }
        }
    };
    let mut canonical = canonical_parent;
    for component in unresolved.iter().rev() {
        canonical.push(component);
    }
    canonical.push(file_name);
    Ok(canonical)
}

fn reject_final_symlink_or_nonregular(path: &Path, description: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("{description} final path must not be a symlink")
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            bail!("{description} must be a regular file")
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

struct PreparedConfigParent {
    canonical_path: PathBuf,
    destination: PathBuf,
    directory: File,
}

fn prepare_config_parent(path: &Path, identity: &ProjectIdentity) -> Result<PreparedConfigParent> {
    #[cfg(not(unix))]
    bail!("durable user-config mutation is unsupported on this platform");

    #[cfg(unix)]
    {
        reject_unresolved_symlink_ancestors(path)
            .with_context(|| format!("user config path {} is not secure", path.display()))?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("config path has no parent"))?;
        let parent_existed = fs::metadata(parent).is_ok();
        if !parent_existed {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory {}", parent.display())
            })?;
            set_mode(parent, CONFIG_DIR_MODE)?;
        }
        let canonical_path = fs::canonicalize(parent)
            .with_context(|| format!("failed to resolve config directory {}", parent.display()))?;
        validate_config_directory(&canonical_path)?;
        let file_name = path
            .file_name()
            .ok_or_else(|| anyhow!("config path has no final component"))?;
        let destination = canonical_path.join(file_name);
        require_utf8_path(&destination, "canonical user config path")?;
        ensure_outside_protected_git_roots(&destination, identity, "user config")?;
        reject_final_symlink_or_nonregular(&destination, "user config")?;
        let directory = File::open(&canonical_path).with_context(|| {
            format!(
                "failed to pin config directory {}",
                canonical_path.display()
            )
        })?;
        Ok(PreparedConfigParent {
            canonical_path,
            destination,
            directory,
        })
    }
}

fn validate_config_directory(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_dir() {
        bail!("config directory {} must be a directory", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!(
                "config directory {} must be owned by the current user",
                path.display()
            );
        }
        if metadata.permissions().mode() & 0o777 != CONFIG_DIR_MODE {
            bail!("config directory {} must have mode 0700", path.display());
        }
    }
    Ok(())
}

fn write_config(path: &Path, bytes: &[u8], identity: &ProjectIdentity) -> Result<()> {
    if bytes.len() > MAX_CONFIG_BYTES {
        bail!("user config exceeds the {MAX_CONFIG_BYTES}-byte rendered-config limit");
    }
    let parent = prepare_config_parent(path, identity)?;
    let (mut temporary, temporary_path) = create_config_temporary(&parent.canonical_path)?;
    let result = (|| -> Result<()> {
        set_file_mode(&temporary, CONFIG_FILE_MODE)?;
        validate_temporary_metadata(&temporary_path, &temporary.metadata()?)?;
        temporary.write_all(bytes).with_context(|| {
            format!(
                "failed to write temporary config {}",
                temporary_path.display()
            )
        })?;
        temporary.sync_all().with_context(|| {
            format!(
                "failed to sync temporary config {}",
                temporary_path.display()
            )
        })?;
        reject_final_symlink_or_nonregular(&parent.destination, "user config")?;
        fs::rename(&temporary_path, &parent.destination).with_context(|| {
            format!(
                "failed to atomically replace user config {}",
                parent.destination.display()
            )
        })?;
        parent.directory.sync_all().with_context(|| {
            format!(
                "failed to sync pinned config directory {}",
                parent.canonical_path.display()
            )
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn create_config_temporary(parent: &Path) -> Result<(File, PathBuf)> {
    for _ in 0..TEMP_CREATE_ATTEMPTS {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".config.toml.tmp.{}.{}",
            std::process::id(),
            sequence
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(CONFIG_FILE_MODE)
                .custom_flags(libc::O_NOFOLLOW);
        }
        match options.open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to create temporary config in {}", parent.display())
                });
            }
        }
    }
    bail!("failed to allocate a unique same-directory config temporary file")
}

#[cfg(unix)]
fn validate_temporary_metadata(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        bail!(
            "temporary config {} must be a single-link regular file",
            path.display()
        );
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        bail!(
            "temporary config {} must be owned by the current user",
            path.display()
        );
    }
    if metadata.permissions().mode() & 0o777 != CONFIG_FILE_MODE {
        bail!("temporary config {} must have mode 0600", path.display());
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_temporary_metadata(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    bail!("secure temporary config files are unsupported on this platform")
}

struct ConfigLock {
    file: File,
}

impl ConfigLock {
    fn acquire(config_path: &Path, identity: &ProjectIdentity) -> Result<Self> {
        #[cfg(not(unix))]
        bail!("descriptor-held user-config locking is unsupported on this platform");

        #[cfg(unix)]
        {
            let parent = prepare_config_parent(config_path, identity)?;
            let path = parent.canonical_path.join("config.lock");
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true);
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(CONFIG_FILE_MODE)
                .custom_flags(libc::O_NOFOLLOW);
            let file = options
                .open(&path)
                .with_context(|| format!("failed to open config lock {}", path.display()))?;
            validate_lock_metadata_before_chmod(&path, &file.metadata()?)?;
            set_file_mode(&file, CONFIG_FILE_MODE)?;
            validate_secure_file_metadata(&path, &file.metadata()?)?;
            use std::os::fd::AsRawFd;
            // SAFETY: `flock` observes the valid descriptor retained by this guard.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(io::Error::last_os_error())
                    .with_context(|| format!("failed to acquire config lock {}", path.display()));
            }
            Ok(Self { file })
        }
    }
}

#[cfg(unix)]
fn validate_lock_metadata_before_chmod(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if !metadata.file_type().is_file() {
        bail!("config lock {} must be a regular file", path.display());
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        bail!(
            "config lock {} must be owned by the current user",
            path.display()
        );
    }
    if metadata.nlink() != 1 {
        bail!("config lock {} must not have hardlinks", path.display());
    }
    Ok(())
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: `flock` observes the valid descriptor retained by this guard.
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

#[cfg(unix)]
fn set_file_mode(file: &File, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_file_mode(_file: &File, _mode: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure file modes are unsupported on this platform",
    ))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure directory modes are unsupported on this platform",
    ))
}
