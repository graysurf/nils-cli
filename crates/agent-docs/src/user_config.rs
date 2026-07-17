use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

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
const CONFIG_DIR_MODE: u32 = 0o700;
const CONFIG_FILE_MODE: u32 = 0o600;

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

fn render_config_error(command: &str, format: OutputFormat, err: &anyhow::Error) -> i32 {
    let message = format!("{err:#}").replace('\n', " ");
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
    4
}

fn run_inner(args: ConfigArgs, overrides: &PathOverrides) -> Result<()> {
    match args.command {
        ConfigCommand::List(args) => list(args),
        ConfigCommand::Show(args) => {
            let identity = resolve_identity(overrides)?;
            show(args, &identity)
        }
        ConfigCommand::Enroll(args) => {
            let identity = resolve_identity(overrides)?;
            enroll(args, &identity)
        }
        ConfigCommand::Exclude(args) => {
            let identity = resolve_identity(overrides)?;
            mutate_rule(args, &identity, RuleMode::Exclude, None, "exclude")
        }
        ConfigCommand::Remove(args) => {
            let identity = resolve_identity(overrides)?;
            remove(args, &identity)
        }
    }
}

fn resolve_identity(overrides: &PathOverrides) -> Result<ProjectIdentity> {
    resolve_project_identity(overrides.project_path.as_deref())
}

fn list(args: ConfigFormatArgs) -> Result<()> {
    let path = config_path()?;
    let entries = match read_config_at(&path) {
        ConfigRead::Missing => Vec::new(),
        ConfigRead::Valid(config) => config.project.iter().map(ConfigEntryView::from).collect(),
        ConfigRead::Fault { diagnostic, .. } => bail!(diagnostic.message),
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
}

fn show(args: ConfigFormatArgs, identity: &ProjectIdentity) -> Result<()> {
    let path = config_path()?;
    let entries = match read_config_at(&path) {
        ConfigRead::Missing => Vec::new(),
        ConfigRead::Valid(config) => matching_rules(&config, identity)
            .into_iter()
            .map(ConfigEntryView::from)
            .collect(),
        ConfigRead::Fault { diagnostic, .. } => bail!(diagnostic.message),
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
}

fn enroll(args: ConfigEnrollArgs, identity: &ProjectIdentity) -> Result<()> {
    let catalog = validate_catalog_for_enrollment(&args.catalog, identity)?;
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
) -> Result<()> {
    let path = config_path()?;
    validate_config_destination(&path, identity)?;
    let selector = selector_for(identity, args.all_worktrees)?;
    let reason = validate_reason(args.reason)?;
    let rule = ProjectRule {
        selector: selector.0,
        path: selector.1,
        mode,
        catalog,
        reason,
    };

    if args.apply {
        let _lock = ConfigLock::acquire(&path)?;
        let proposed = update_document(&path, &rule)?;
        write_config(&path, proposed.as_bytes())?;
    } else {
        let _ = update_document(&path, &rule)?;
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
}

fn remove(args: ConfigRemoveArgs, identity: &ProjectIdentity) -> Result<()> {
    let path = config_path()?;
    validate_config_destination(&path, identity)?;
    let selector = selector_for(identity, args.all_worktrees)?;
    if args.apply {
        let _lock = ConfigLock::acquire(&path)?;
        let proposed = remove_from_document(&path, selector.0, &selector.1)?;
        write_config(&path, proposed.as_bytes())?;
    } else {
        let _ = remove_from_document(&path, selector.0, &selector.1)?;
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
    if let Some(raw) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        let root = PathBuf::from(raw);
        if !root.is_absolute() {
            bail!("XDG_CONFIG_HOME must be an absolute path");
        }
        return Ok(root.join("agent-docs/config.toml"));
    }
    let home = crate::env::home_dir().ok_or_else(|| anyhow!("HOME is unset"))?;
    if !home.is_absolute() {
        bail!("HOME must be an absolute path when XDG_CONFIG_HOME is unset");
    }
    Ok(home.join(".config/agent-docs/config.toml"))
}

pub fn read_config() -> Result<ConfigRead> {
    Ok(read_config_at(&config_path()?))
}

fn read_config_at(path: &Path) -> ConfigRead {
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
    let config: UserConfig = toml::from_str(raw)
        .map_err(|err| format!("invalid user config {}: {err}", path.display()))?;
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
    if let Some(catalog) = &rule.catalog
        && !catalog.is_absolute()
    {
        return Err("catalog must be absolute".to_string());
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
    identity: &ProjectIdentity,
) -> Vec<&'a ProjectRule> {
    config
        .project
        .iter()
        .filter(|rule| match rule.selector {
            SelectorKind::ProjectPath => rule.path == identity.project_path,
            SelectorKind::GitCommonDir => identity
                .git_common_dir
                .as_ref()
                .is_some_and(|path| path == &rule.path),
        })
        .collect()
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
        return Ok((
            SelectorKind::GitCommonDir,
            fs::canonicalize(common).with_context(|| {
                format!("failed to canonicalize Git common dir {}", common.display())
            })?,
        ));
    }
    Ok((SelectorKind::ProjectPath, identity.project_path.clone()))
}

pub(crate) fn read_selected_catalog(path: &Path, identity: &ProjectIdentity) -> Result<SecureFile> {
    if !path.is_absolute() {
        bail!("catalog path must be absolute");
    }
    if path_has_forbidden_components(path) {
        bail!("catalog path must use a normalized absolute spelling without `.` or `..`");
    }
    let snapshot = secure_read_file(path, MAX_PRIVATE_CATALOG_BYTES)
        .with_context(|| format!("catalog {} is not secure", path.display()))?
        .ok_or_else(|| anyhow!("catalog {} does not exist", path.display()))?;

    let mut forbidden = vec![identity.project_path.clone()];
    if let Some(common) = &identity.git_common_dir {
        forbidden.push(fs::canonicalize(common).unwrap_or_else(|_| common.clone()));
        forbidden.extend(crate::env::git_worktree_roots(&identity.project_path)?);
    }
    if forbidden.iter().any(|root| snapshot.path.starts_with(root)) {
        bail!("private catalog must remain outside every target Git worktree and common dir");
    }
    Ok(snapshot)
}

fn validate_catalog_for_enrollment(path: &Path, identity: &ProjectIdentity) -> Result<PathBuf> {
    let snapshot = read_selected_catalog(path, identity)?;
    let raw = std::str::from_utf8(&snapshot.bytes).context("private catalog is not valid UTF-8")?;
    crate::config::load_scope_catalog_from_str(
        crate::model::Scope::Project,
        crate::model::CatalogOrigin::User,
        &identity.project_path,
        &snapshot.path,
        raw,
    )
    .map_err(|err| anyhow!(err.to_string()))?;
    Ok(snapshot.path)
}

pub(crate) fn secure_read_file(path: &Path, max_bytes: usize) -> io::Result<Option<SecureFile>> {
    reject_symlink_ancestors(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} must not be a symlink", path.display()),
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
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let opened_metadata = file.metadata()?;
    validate_secure_file_metadata(path, &opened_metadata)?;
    let canonical = fs::canonicalize(path)?;
    reject_symlink_ancestors(path)?;

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

    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.by_ref().take(limit).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} exceeds the {max_bytes}-byte size limit", path.display()),
        ));
    }
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

fn reject_symlink_ancestors(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "{} must not contain symlinked parent directories",
                        path.display()
                    ),
                ));
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
    tables.push(rule_table(rule));
    Ok(doc.to_string())
}

fn remove_from_document(path: &Path, selector: SelectorKind, rule_path: &Path) -> Result<String> {
    let mut doc = load_document(path)?;
    let tables = project_tables_mut(&mut doc)?;
    retain_other_rules(tables, selector, rule_path);
    Ok(doc.to_string())
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
        .with_context(|| format!("failed to edit {}", path.display()))
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

fn rule_table(rule: &ProjectRule) -> Table {
    let mut table = Table::new();
    table["match"] = value(rule.selector.as_str());
    table["path"] = value(rule.path.to_string_lossy().as_ref());
    table["mode"] = value(rule.mode.as_str());
    if let Some(catalog) = &rule.catalog {
        table["catalog"] = value(catalog.to_string_lossy().as_ref());
    }
    if let Some(reason) = &rule.reason {
        table["reason"] = value(reason);
    }
    table
}

fn validate_config_destination(path: &Path, identity: &ProjectIdentity) -> Result<()> {
    if !path.is_absolute() || path_has_forbidden_components(path) {
        bail!("user config path must use a normalized absolute spelling");
    }
    reject_symlink_ancestors(path)
        .with_context(|| format!("user config path {} is not secure", path.display()))?;

    let mut forbidden = vec![identity.project_path.clone()];
    if let Some(common) = &identity.git_common_dir {
        forbidden.push(common.clone());
        forbidden.extend(crate::env::git_worktree_roots(&identity.project_path)?);
    }
    if forbidden.iter().any(|root| path.starts_with(root)) {
        bail!("user config must remain outside every target Git worktree and common dir");
    }
    Ok(())
}

fn prepare_config_parent(path: &Path) -> Result<()> {
    reject_symlink_ancestors(path)
        .with_context(|| format!("user config path {} is not secure", path.display()))?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("config path has no parent"))?;
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "config directory {} must not be a symlink",
                    current.display()
                );
            }
            Ok(metadata) if !metadata.is_dir() => {
                bail!("config parent {} is not a directory", current.display());
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current).with_context(|| {
                    format!("failed to create config directory {}", current.display())
                })?;
                set_mode(&current, CONFIG_DIR_MODE)?;
            }
            Err(err) => return Err(err.into()),
        }
    }
    validate_config_directory(parent)?;
    Ok(())
}

fn validate_config_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("config directory {} must not be a symlink", path.display());
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
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("config directory {} must have mode 0700", path.display());
        }
    }
    Ok(())
}

fn write_config(path: &Path, bytes: &[u8]) -> Result<()> {
    prepare_config_parent(path)?;
    nils_common::fs::write_atomic(path, bytes, CONFIG_FILE_MODE)
        .with_context(|| format!("failed to atomically write {}", path.display()))?;
    File::open(path)?.sync_all()?;
    File::open(path.parent().expect("validated config parent"))?.sync_all()?;
    Ok(())
}

struct ConfigLock {
    file: File,
    #[cfg(not(unix))]
    path: PathBuf,
}

impl ConfigLock {
    fn acquire(config_path: &Path) -> Result<Self> {
        prepare_config_parent(config_path)?;
        let parent = config_path
            .parent()
            .ok_or_else(|| anyhow!("config path has no parent"))?;
        let path = parent.join("config.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(CONFIG_FILE_MODE)
                .custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(not(unix))]
        options.create_new(true);
        let file = options
            .open(&path)
            .with_context(|| format!("failed to open config lock {}", path.display()))?;
        set_file_mode(&file, CONFIG_FILE_MODE)?;
        validate_secure_file_metadata(&path, &file.metadata()?)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: `flock` observes the valid descriptor owned by `file`.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(io::Error::last_os_error())
                    .with_context(|| format!("failed to acquire config lock {}", path.display()));
            }
        }
        file.sync_all()?;
        Ok(Self {
            file,
            #[cfg(not(unix))]
            path,
        })
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: `flock` observes the valid descriptor owned by this guard.
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
        #[cfg(not(unix))]
        {
            let _ = fs::remove_file(&self.path);
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
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}
