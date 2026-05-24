use std::fmt;
use std::path::PathBuf;

use clap::ValueEnum;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Context {
    Startup,
    SkillDev,
    TaskTools,
    ProjectDev,
}

impl Context {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::SkillDev => "skill-dev",
            Self::TaskTools => "task-tools",
            Self::ProjectDev => "project-dev",
        }
    }

    pub const fn supported_values() -> &'static [&'static str] {
        &["startup", "skill-dev", "task-tools", "project-dev"]
    }

    pub fn from_config_value(value: &str) -> Option<Self> {
        match value {
            "startup" => Some(Self::Startup),
            "skill-dev" => Some(Self::SkillDev),
            "task-tools" => Some(Self::TaskTools),
            "project-dev" => Some(Self::ProjectDev),
            _ => None,
        }
    }
}

impl fmt::Display for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub const SUPPORTED_CONTEXTS: [Context; 4] = [
    Context::Startup,
    Context::SkillDev,
    Context::TaskTools,
    Context::ProjectDev,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    Home,
    Project,
    Global,
}

impl Scope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Project => "project",
            Self::Global => "global",
        }
    }

    pub const fn supported_values() -> &'static [&'static str] {
        &["home", "project", "global"]
    }

    pub fn from_config_value(value: &str) -> Option<Self> {
        match value {
            "home" => Some(Self::Home),
            "project" => Some(Self::Project),
            "global" => Some(Self::Global),
            _ => None,
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

impl OutputFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FallbackMode {
    #[default]
    Auto,
    LocalOnly,
}

impl FallbackMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::LocalOnly => "local-only",
        }
    }
}

impl fmt::Display for FallbackMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ResolveFormat {
    #[default]
    Text,
    Json,
    Checklist,
}

impl ResolveFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
            Self::Checklist => "checklist",
        }
    }
}

impl fmt::Display for ResolveFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum, Default)]
#[serde(rename_all = "kebab-case")]
pub enum BaselineTarget {
    Home,
    Project,
    #[default]
    All,
}

impl BaselineTarget {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Project => "project",
            Self::All => "all",
        }
    }
}

impl fmt::Display for BaselineTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentStatus {
    Present,
    Missing,
}

impl DocumentStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Missing => "missing",
        }
    }
}

impl fmt::Display for DocumentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentSource {
    Builtin,
    BuiltinFallback,
    ExtensionHome,
    ExtensionProject,
}

impl DocumentSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::BuiltinFallback => "builtin-fallback",
            Self::ExtensionHome => "extension-home",
            Self::ExtensionProject => "extension-project",
        }
    }
}

impl fmt::Display for DocumentSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedDocument {
    pub context: Context,
    pub scope: Scope,
    pub path: PathBuf,
    pub required: bool,
    pub status: DocumentStatus,
    pub source: DocumentSource,
    pub why: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolveSummary {
    pub required_total: usize,
    pub present_required: usize,
    pub missing_required: usize,
}

impl ResolveSummary {
    pub fn from_documents(documents: &[ResolvedDocument]) -> Self {
        let required_total = documents.iter().filter(|doc| doc.required).count();
        let present_required = documents
            .iter()
            .filter(|doc| doc.required && doc.status == DocumentStatus::Present)
            .count();
        let missing_required = required_total.saturating_sub(present_required);

        Self {
            required_total,
            present_required,
            missing_required,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolveReport {
    pub context: Context,
    pub strict: bool,
    pub docs_home: PathBuf,
    pub project_path: PathBuf,
    pub is_linked_worktree: bool,
    pub git_common_dir: Option<PathBuf>,
    pub primary_worktree_path: Option<PathBuf>,
    pub documents: Vec<ResolvedDocument>,
    pub summary: ResolveSummary,
}

impl ResolveReport {
    pub fn has_missing_required(&self) -> bool {
        self.summary.missing_required > 0
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BaselineCheckItem {
    pub scope: Scope,
    pub context: Context,
    pub label: String,
    pub path: PathBuf,
    pub required: bool,
    pub status: DocumentStatus,
    pub source: DocumentSource,
    pub why: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BaselineCheckReport {
    pub target: BaselineTarget,
    pub strict: bool,
    pub docs_home: PathBuf,
    pub project_path: PathBuf,
    pub items: Vec<BaselineCheckItem>,
    pub missing_required: usize,
    pub missing_optional: usize,
    pub suggested_actions: Vec<String>,
}

impl BaselineCheckReport {
    pub fn from_items(
        target: BaselineTarget,
        strict: bool,
        docs_home: PathBuf,
        project_path: PathBuf,
        items: Vec<BaselineCheckItem>,
        suggested_actions: Vec<String>,
    ) -> Self {
        let missing_required = items
            .iter()
            .filter(|item| item.required && item.status == DocumentStatus::Missing)
            .count();
        let missing_optional = items
            .iter()
            .filter(|item| !item.required && item.status == DocumentStatus::Missing)
            .count();

        Self {
            target,
            strict,
            docs_home,
            project_path,
            items,
            missing_required,
            missing_optional,
            suggested_actions,
        }
    }

    pub fn has_missing_required(&self) -> bool {
        self.missing_required > 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentWhen {
    Always,
}

impl DocumentWhen {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
        }
    }

    pub const fn supported_values() -> &'static [&'static str] {
        &["always"]
    }

    pub fn from_config_value(value: &str) -> Option<Self> {
        match value {
            "always" => Some(Self::Always),
            _ => None,
        }
    }
}

impl fmt::Display for DocumentWhen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigDocumentEntry {
    pub context: Context,
    pub scope: Scope,
    pub path: PathBuf,
    pub required: bool,
    pub when: DocumentWhen,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigScopeFile {
    pub source_scope: Scope,
    pub root: PathBuf,
    pub file_path: PathBuf,
    pub documents: Vec<ConfigDocumentEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct LoadedConfigs {
    pub home: Option<ConfigScopeFile>,
    pub project: Option<ConfigScopeFile>,
}

impl LoadedConfigs {
    pub fn in_load_order(&self) -> Vec<&ConfigScopeFile> {
        let mut ordered = Vec::new();
        if let Some(home) = self.home.as_ref() {
            ordered.push(home);
        }
        if let Some(project) = self.project.as_ref() {
            ordered.push(project);
        }
        ordered
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigErrorKind {
    Io,
    Parse,
    Validation,
}

impl ConfigErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Parse => "parse",
            Self::Validation => "validation",
        }
    }
}

impl fmt::Display for ConfigErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ConfigErrorLocation {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigLoadError {
    pub kind: ConfigErrorKind,
    pub file_path: PathBuf,
    pub document_index: Option<usize>,
    pub field: Option<String>,
    pub location: Option<ConfigErrorLocation>,
    pub message: String,
}

impl ConfigLoadError {
    pub fn io(file_path: PathBuf, message: impl Into<String>) -> Self {
        Self {
            kind: ConfigErrorKind::Io,
            file_path,
            document_index: None,
            field: None,
            location: None,
            message: message.into(),
        }
    }

    pub fn parse(
        file_path: PathBuf,
        message: impl Into<String>,
        location: Option<ConfigErrorLocation>,
    ) -> Self {
        Self {
            kind: ConfigErrorKind::Parse,
            file_path,
            document_index: None,
            field: None,
            location,
            message: message.into(),
        }
    }

    pub fn validation(
        file_path: PathBuf,
        document_index: usize,
        field: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: ConfigErrorKind::Validation,
            file_path,
            document_index: Some(document_index),
            field: Some(field.into()),
            location: None,
            message: message.into(),
        }
    }

    pub fn validation_root(
        file_path: PathBuf,
        field: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: ConfigErrorKind::Validation,
            file_path,
            document_index: None,
            field: Some(field.into()),
            location: None,
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.document_index, self.field.as_deref(), self.location) {
            (Some(index), Some(field), Some(location)) => write!(
                f,
                "{}:{}:{} [{}] document[{index}].{field}: {}",
                self.file_path.display(),
                location.line,
                location.column,
                self.kind,
                self.message
            ),
            (Some(index), Some(field), None) => write!(
                f,
                "{} [{}] document[{index}].{field}: {}",
                self.file_path.display(),
                self.kind,
                self.message
            ),
            (None, None, Some(location)) => write!(
                f,
                "{}:{}:{} [{}]: {}",
                self.file_path.display(),
                location.line,
                location.column,
                self.kind,
                self.message
            ),
            (None, Some(field), Some(location)) => write!(
                f,
                "{}:{}:{} [{}] {field}: {}",
                self.file_path.display(),
                location.line,
                location.column,
                self.kind,
                self.message
            ),
            (None, Some(field), None) => write!(
                f,
                "{} [{}] {field}: {}",
                self.file_path.display(),
                self.kind,
                self.message
            ),
            _ => write!(
                f,
                "{} [{}]: {}",
                self.file_path.display(),
                self.kind,
                self.message
            ),
        }
    }
}

impl std::error::Error for ConfigLoadError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AddDocumentAction {
    Inserted,
    Updated,
}

impl AddDocumentAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inserted => "inserted",
            Self::Updated => "updated",
        }
    }
}

impl fmt::Display for AddDocumentAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AddDocumentReport {
    pub target: Scope,
    pub target_root: PathBuf,
    pub config_path: PathBuf,
    pub created_config: bool,
    pub action: AddDocumentAction,
    pub entry: ConfigDocumentEntry,
    pub document_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct StubReport {
    pub command: String,
    pub implemented: bool,
    pub message: String,
}
