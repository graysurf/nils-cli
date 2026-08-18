use std::fmt;
use std::path::PathBuf;

use clap::ValueEnum;
use serde::Serialize;

/// A free-form intent identifier (for example `project-dev` or `task-tools`).
///
/// Contexts are declared by the catalog, not compiled into the binary, so the
/// engine treats them as opaque kebab-case-ish identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Context(String);

impl Context {
    /// Validate and construct a context identifier. Allowed characters are
    /// ASCII alphanumerics, `-`, `_`, `.`, and `/` so catalog authors can use
    /// readable intent names without surprising the parser.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("context cannot be empty".to_string());
        }
        if let Some(bad) = trimmed
            .chars()
            .find(|ch| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/')))
        {
            return Err(format!(
                "context `{trimmed}` contains unsupported character `{bad}`; allowed: a-z A-Z 0-9 - _ . /"
            ));
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A free-form workflow-phase identifier (for example `edit` or `delivery`).
///
/// Phases are declared by the catalog, not compiled into the binary, so a
/// consumer defines its own phase vocabulary and adding a phase never needs a
/// new release. Validated on the same charset as [`Context`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Phase(String);

impl Phase {
    /// Validate and construct a phase identifier. Allowed characters mirror
    /// [`Context`]: ASCII alphanumerics, `-`, `_`, `.`, and `/`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("phase cannot be empty".to_string());
        }
        if let Some(bad) = trimmed
            .chars()
            .find(|ch| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/')))
        {
            return Err(format!(
                "phase `{trimmed}` contains unsupported character `{bad}`; allowed: a-z A-Z 0-9 - _ . /"
            ));
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, ValueEnum)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Product {
    Codex,
    Claude,
    Hermes,
    Dsh,
}

impl Product {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Hermes => "hermes",
            Self::Dsh => "dsh",
        }
    }

    pub const fn supported_values() -> &'static [&'static str] {
        &["codex", "claude", "hermes", "dsh"]
    }

    pub fn from_config_value(value: &str) -> Option<Self> {
        match value {
            "codex" => Some(Self::Codex),
            "claude" => Some(Self::Claude),
            "hermes" => Some(Self::Hermes),
            "dsh" => Some(Self::Dsh),
            _ => None,
        }
    }
}

impl fmt::Display for Product {
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
pub enum AuditTarget {
    Home,
    Project,
    #[default]
    All,
}

impl AuditTarget {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Project => "project",
            Self::All => "all",
        }
    }

    pub fn includes_scope(self, scope: Scope) -> bool {
        match self {
            Self::Home => matches!(scope, Scope::Home | Scope::Global),
            Self::Project => scope == Scope::Project,
            Self::All => true,
        }
    }
}

impl fmt::Display for AuditTarget {
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

/// Which catalog layer a resolved document originated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentSource {
    Home,
    Project,
}

impl DocumentSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Project => "project",
        }
    }

    pub fn from_scope(scope: Scope) -> Self {
        match scope {
            Scope::Home | Scope::Global => Self::Home,
            Scope::Project => Self::Project,
        }
    }
}

impl fmt::Display for DocumentSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `when` predicate: `path-exists:<glob>` atoms composed with `||` and `&&`.
///
/// The empty / `always` predicate is unconditionally true. `&&` binds tighter
/// than `||`, so the predicate is an OR of AND-clauses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum When {
    Always,
    /// OR of AND-clauses; the predicate holds when any clause holds, and a
    /// clause holds when every atom in it holds.
    Any(Vec<Vec<WhenAtom>>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum WhenAtom {
    /// True when at least one filesystem path matching `glob` exists under the
    /// resolved project root.
    PathExists { glob: String },
    /// An explicit always-true atom (the `always` keyword).
    Always,
}

/// Result of validating a resolved document's content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocumentValidation {
    pub exists: bool,
    pub non_empty: bool,
    /// `None` when no marker is declared; otherwise whether the marker string
    /// appears in the document content.
    pub marker_present: Option<bool>,
    pub freshness: FreshnessCheck,
    /// Overall verdict: a declared, required document is satisfied only when
    /// `valid` is true.
    pub valid: bool,
}

impl DocumentValidation {
    pub fn missing() -> Self {
        Self {
            exists: false,
            non_empty: false,
            marker_present: None,
            freshness: FreshnessCheck::NotDeclared,
            valid: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FreshnessCheck {
    /// No `last-reviewed-within-days` declared for this entry.
    NotDeclared,
    /// Declared and the document carries a recent enough `last-reviewed` date.
    Fresh,
    /// Declared but the document's `last-reviewed` date is too old.
    Stale,
    /// Declared but no parseable `last-reviewed: YYYY-MM-DD` line was found.
    Unknown,
}

impl FreshnessCheck {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotDeclared => "not-declared",
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Unknown => "unknown",
        }
    }

    /// A freshness verdict passes unless it is explicitly stale or unknown for a
    /// declared freshness requirement.
    pub const fn passes(self) -> bool {
        matches!(self, Self::NotDeclared | Self::Fresh)
    }
}

impl fmt::Display for FreshnessCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A document required (or conditionally required) for a given intent, after
/// `when` evaluation and content validation.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedDocument {
    pub context: Context,
    pub scope: Scope,
    pub path: PathBuf,
    pub products: Vec<Product>,
    /// The catalog phases this document is scoped to. Empty means the document
    /// applies to every phase; omitted from JSON when empty so no-phase catalog
    /// output stays byte-identical.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<Phase>,
    /// Whether the catalog marked the entry `required = true`.
    pub declared_required: bool,
    /// Whether the entry is required for this run (`declared_required` AND the
    /// `when` predicate evaluated true).
    pub required: bool,
    pub when: String,
    pub when_satisfied: bool,
    pub status: DocumentStatus,
    pub validation: DocumentValidation,
    pub source: DocumentSource,
    pub why: String,
    /// Document content, populated by `preflight` content emission only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

impl ResolvedDocument {
    /// A required document is satisfied when it exists and passes validation.
    pub fn satisfied(&self) -> bool {
        !self.required || (self.status == DocumentStatus::Present && self.validation.valid)
    }
}

/// The per-repo validation contract for an intent, resolved from `[[validation]]`
/// catalog entries.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationContract {
    pub context: Context,
    pub declared: bool,
    pub commands: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolveSummary {
    pub required_total: usize,
    pub satisfied_required: usize,
    pub missing_required: usize,
    pub invalid_required: usize,
}

impl ResolveSummary {
    pub fn from_documents(documents: &[ResolvedDocument]) -> Self {
        let required: Vec<&ResolvedDocument> =
            documents.iter().filter(|doc| doc.required).collect();
        let required_total = required.len();
        let satisfied_required = required.iter().filter(|doc| doc.satisfied()).count();
        let missing_required = required
            .iter()
            .filter(|doc| doc.status == DocumentStatus::Missing)
            .count();
        let invalid_required = required
            .iter()
            .filter(|doc| doc.status == DocumentStatus::Present && !doc.validation.valid)
            .count();

        Self {
            required_total,
            satisfied_required,
            missing_required,
            invalid_required,
        }
    }

    pub fn all_satisfied(&self) -> bool {
        self.required_total == self.satisfied_required
    }
}

/// The output of `preflight --intent X`: the resolved doc set plus the
/// validation contract for that intent.
#[derive(Debug, Clone, Serialize)]
pub struct PreflightReport {
    pub schema_version: &'static str,
    pub intent: Context,
    pub product: Option<Product>,
    /// The requested phase filter, or `None` when no `--phase` was supplied.
    /// Omitted from JSON when absent so no-phase output stays byte-identical.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
    pub strict: bool,
    pub docs_home: PathBuf,
    pub project_path: PathBuf,
    pub is_linked_worktree: bool,
    pub documents: Vec<ResolvedDocument>,
    pub validation: ValidationContract,
    pub summary: ResolveSummary,
}

impl PreflightReport {
    pub const SCHEMA_VERSION: &'static str = "agent-docs.preflight.v2";

    pub fn has_unsatisfied_required(&self) -> bool {
        !self.summary.all_satisfied()
    }
}

/// A single wiring check performed by `audit` (for example, "is the
/// `~/.claude/CLAUDE.md` symlink intact and pointing at the docs-home?").
#[derive(Debug, Clone, Serialize)]
pub struct WiringCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// A single skill-name check performed by `audit` when the project catalog
/// opts in via `[skills] enforce_name_prefix`. Each immediate subdirectory of
/// the configured skills directory is checked against the required prefixes.
#[derive(Debug, Clone, Serialize)]
pub struct SkillCheck {
    /// The skill directory name (relative to the skills directory).
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditReport {
    pub schema_version: &'static str,
    pub target: AuditTarget,
    pub product: Option<Product>,
    pub strict: bool,
    pub docs_home: PathBuf,
    pub project_path: PathBuf,
    pub wiring: Vec<WiringCheck>,
    /// Skill-name prefix checks; empty unless the project catalog opts in.
    pub skills: Vec<SkillCheck>,
    pub documents: Vec<ResolvedDocument>,
    pub problems: usize,
    pub suggested_actions: Vec<String>,
}

impl AuditReport {
    pub const SCHEMA_VERSION: &'static str = "agent-docs.audit.v2";

    pub fn has_problems(&self) -> bool {
        self.problems > 0
    }
}

// ---------------------------------------------------------------------------
// Catalog (parsed AGENT_DOCS.toml) types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocumentEntry {
    pub context: Context,
    pub scope: Scope,
    pub path: PathBuf,
    pub products: Vec<Product>,
    /// The phases this document is scoped to. Empty means every phase. Omitted
    /// from JSON when empty so no-phase catalogs serialize byte-identically
    /// (which keeps the session fingerprint stable across the upgrade).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<Phase>,
    pub required: bool,
    pub when: When,
    /// The raw `when` string as written in the catalog (for display / audit).
    pub when_raw: String,
    pub marker: Option<String>,
    pub freshness_days: Option<u64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationEntry {
    pub context: Context,
    pub products: Vec<Product>,
    pub commands: Vec<String>,
    pub marker: Option<String>,
    pub description: Option<String>,
}

/// Opt-in skill-name policy declared by a project catalog's `[skills]` table.
///
/// When `enforce_name_prefix` is true, `audit` flags every immediate
/// subdirectory of `dir` whose name is not lowercase kebab-case starting with
/// one of `allowed_prefixes` (followed by a hyphen and at least one more
/// character). This mirrors the `create-project-skill` creation-time rule
/// (`^(project|private)-[a-z0-9-]+`) so renamed repos stay compliant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillPolicy {
    pub enforce_name_prefix: bool,
    /// Allowed name prefixes (each matched as `<prefix>-`). Defaults to
    /// `["project", "private"]`.
    pub allowed_prefixes: Vec<String>,
    /// Skills directory relative to the project root. Defaults to
    /// `.agents/skills`.
    pub dir: String,
}

impl SkillPolicy {
    pub const DEFAULT_DIR: &'static str = ".agents/skills";

    pub fn default_prefixes() -> Vec<String> {
        vec!["project".to_string(), "private".to_string()]
    }

    /// Whether a skill directory `name` satisfies the policy: lowercase
    /// kebab-case (`[a-z0-9-]+`) starting with one of the allowed prefixes
    /// followed by `-` and at least one further character.
    pub fn name_is_valid(&self, name: &str) -> bool {
        let kebab = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !kebab {
            return false;
        }
        self.allowed_prefixes.iter().any(|prefix| {
            let needle = format!("{prefix}-");
            name.starts_with(&needle) && name.len() > needle.len()
        })
    }

    /// Human-readable description of the required prefixes (for audit detail).
    pub fn prefix_hint(&self) -> String {
        self.allowed_prefixes
            .iter()
            .map(|prefix| format!("{prefix}-"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogOrigin {
    Home,
    Repository,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeCatalog {
    pub source_scope: Scope,
    pub root: PathBuf,
    pub file_path: PathBuf,
    pub documents: Vec<DocumentEntry>,
    pub validations: Vec<ValidationEntry>,
    /// Opt-in skill-name policy; `None` when no `[skills]` table is declared.
    pub skill_policy: Option<SkillPolicy>,
    /// Optional repository path classification contract. Only project-scope
    /// catalogs may declare it; consumers treat absence as `not-configured`.
    pub path_classes: Option<crate::path_classes::PathClassContract>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct LoadedCatalog {
    pub home: Option<ScopeCatalog>,
    pub project: Option<ScopeCatalog>,
}

impl LoadedCatalog {
    pub fn in_load_order(&self) -> Vec<&ScopeCatalog> {
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

// ---------------------------------------------------------------------------
// Config-load errors
// ---------------------------------------------------------------------------

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
    /// The catalog array this error belongs to (for example `document` or
    /// `validation`), when applicable.
    pub section: Option<&'static str>,
    pub entry_index: Option<usize>,
    pub field: Option<String>,
    // Boxed to keep the error variant small (clippy::result_large_err).
    pub location: Option<Box<ConfigErrorLocation>>,
    pub message: String,
}

impl ConfigLoadError {
    pub fn io(file_path: PathBuf, message: impl Into<String>) -> Self {
        Self {
            kind: ConfigErrorKind::Io,
            file_path,
            section: None,
            entry_index: None,
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
            section: None,
            entry_index: None,
            field: None,
            location: location.map(Box::new),
            message: message.into(),
        }
    }

    pub fn validation(
        file_path: PathBuf,
        section: &'static str,
        entry_index: usize,
        field: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: ConfigErrorKind::Validation,
            file_path,
            section: Some(section),
            entry_index: Some(entry_index),
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
            section: None,
            entry_index: None,
            field: Some(field.into()),
            location: None,
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.file_path.display())?;
        if let Some(location) = &self.location {
            write!(f, ":{}:{}", location.line, location.column)?;
        }
        write!(f, " [{}]", self.kind)?;
        match (self.section, self.entry_index, self.field.as_deref()) {
            (Some(section), Some(index), Some(field)) => {
                write!(f, " {section}[{index}].{field}")?;
            }
            (Some(section), Some(index), None) => {
                write!(f, " {section}[{index}]")?;
            }
            (_, _, Some(field)) => {
                write!(f, " {field}")?;
            }
            _ => {}
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for ConfigLoadError {}

// ---------------------------------------------------------------------------
// init / list / remove report types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InitMode {
    Print,
    DryRun,
    Write,
}

impl InitMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Print => "print",
            Self::DryRun => "dry-run",
            Self::Write => "write",
        }
    }
}

impl fmt::Display for InitMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InitReport {
    pub mode: InitMode,
    pub target_path: PathBuf,
    pub wrote: bool,
    pub stub: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListReport {
    pub docs_home: PathBuf,
    pub project_path: PathBuf,
    pub intents: Vec<String>,
    pub documents: Vec<ResolvedDocument>,
    pub validations: Vec<ValidationContract>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoveOutcome {
    Removed,
    NotFound,
}

impl RemoveOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Removed => "removed",
            Self::NotFound => "not-found",
        }
    }
}

impl fmt::Display for RemoveOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoveReport {
    pub config_path: PathBuf,
    pub outcome: RemoveOutcome,
    pub context: String,
    pub scope: Scope,
    pub path: PathBuf,
    pub remaining_documents: usize,
}
