use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use nils_common::cli_contract::{Envelope, EnvelopeError, schema_version_for};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::cli::{IntegrationArgs, IntegrationCommand, IntegrationResolveArgs};
use crate::config::{config_path_for_root, load_scope_catalog_from_str};
use crate::env::{PathOverrides, ProjectIdentity, ResolvedRoots, resolve_roots};
use crate::model::{
    CatalogOrigin, FallbackMode, LoadedCatalog, OutputFormat, Product, Scope, ScopeCatalog,
};
use crate::user_config::{
    ConfigDiagnostic, ConfigRead, ConfigState, ProjectRule, RuleMode, SelectorKind, config_path,
    matching_rules, read_config, read_selected_catalog,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrationAction {
    Integrate,
    Exclude,
    Unmanaged,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundCatalogErrorKind {
    Config,
    Data,
    Runtime,
}

#[derive(Debug)]
pub(crate) enum BoundCatalogError {
    Resolution(anyhow::Error),
    StaleFingerprint {
        expected: String,
        current: String,
    },
    CatalogNotSelected {
        action: IntegrationAction,
        reason_code: String,
    },
    MissingCatalog,
}

impl BoundCatalogError {
    pub(crate) const fn kind(&self) -> BoundCatalogErrorKind {
        match self {
            Self::StaleFingerprint { .. } => BoundCatalogErrorKind::Data,
            Self::CatalogNotSelected { .. } => BoundCatalogErrorKind::Config,
            Self::Resolution(_) | Self::MissingCatalog => BoundCatalogErrorKind::Runtime,
        }
    }

    pub(crate) const fn code(&self) -> &'static str {
        match self {
            Self::Resolution(_) => "integration-resolution-failed",
            Self::StaleFingerprint { .. } => "stale-integration-decision",
            Self::CatalogNotSelected { .. } => "integration-catalog-not-selected",
            Self::MissingCatalog => "integration-catalog-invariant-failed",
        }
    }
}

impl fmt::Display for BoundCatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resolution(err) => write!(f, "failed to resolve integration decision: {err:#}"),
            Self::StaleFingerprint { expected, current } => write!(
                f,
                "integration fingerprint is stale: expected {expected}, current {current}"
            ),
            Self::CatalogNotSelected {
                action,
                reason_code,
            } => write!(
                f,
                "integration decision does not select a catalog: action={action:?} reason={reason_code}"
            ),
            Self::MissingCatalog => f.write_str("integrate decision omitted loaded catalog"),
        }
    }
}

impl std::error::Error for BoundCatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Resolution(err) => Some(err.as_ref()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct IntegrationIdentity {
    pub project_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_common_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchedSelector {
    pub kind: SelectorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectedCatalogOrigin {
    Repository,
    User,
}

#[derive(Debug, Clone, Serialize)]
pub struct SelectedCatalog {
    pub origin: SelectedCatalogOrigin,
    pub path: PathBuf,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntegrationResult {
    pub action: IntegrationAction,
    pub reason_code: String,
    pub product: Product,
    pub config_path: PathBuf,
    pub config_state: ConfigState,
    pub identity: IntegrationIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_selector: Option<MatchedSelector>,
    pub selected_catalog: Option<SelectedCatalog>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<ConfigDiagnostic>,
    pub decision_fingerprint: String,
}

#[derive(Debug, Serialize)]
struct FingerprintInput<'a> {
    schema_version: u32,
    product: Product,
    fallback_mode: FallbackMode,
    project_path: &'a Path,
    git_common_dir: Option<&'a Path>,
    config_state: Option<ConfigState>,
    selector: Option<SelectorKind>,
    selector_path: Option<&'a Path>,
    mode: Option<RuleMode>,
    reason: Option<&'a str>,
    action: IntegrationAction,
    reason_code: &'a str,
    selected_origin: Option<SelectedCatalogOrigin>,
    selected_path: Option<&'a Path>,
    selected_digest: Option<&'a str>,
    docs_home_digest: &'a str,
}

struct ResolutionSnapshot {
    result: IntegrationResult,
    catalog: Option<EffectiveCatalog>,
}

pub(crate) struct EffectiveCatalog {
    pub(crate) catalog: LoadedCatalog,
    pub(crate) private_project_catalog: bool,
}

struct DecisionCatalog {
    selected: SelectedCatalog,
    catalog: LoadedCatalog,
    docs_home_digest: String,
    private_project_catalog: bool,
}

struct CatalogFile {
    path: PathBuf,
    raw: String,
    digest: String,
}

pub fn run(args: IntegrationArgs, overrides: &PathOverrides, fallback_mode: FallbackMode) -> i32 {
    let format = match &args.command {
        IntegrationCommand::Resolve(args) => args.format,
    };
    let result = match args.command {
        IntegrationCommand::Resolve(args) => run_resolve(args, overrides, fallback_mode),
    };
    match result {
        Ok(()) => 0,
        Err(err) => {
            let message = format!("{err:#}").replace('\n', " ");
            match format {
                OutputFormat::Json => {
                    let envelope: Envelope<()> = Envelope::failure(
                        schema_version_for("agent-docs", "integration.resolve", 1),
                        EnvelopeError::new("integration-resolve-failed", message),
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
    }
}

fn run_resolve(
    args: IntegrationResolveArgs,
    overrides: &PathOverrides,
    fallback_mode: FallbackMode,
) -> Result<()> {
    let roots = resolve_roots(overrides)?;
    let result = resolve_decision(&roots, args.product, fallback_mode)?;
    match args.format {
        OutputFormat::Json => {
            let envelope = Envelope::success(
                schema_version_for("agent-docs", "integration.resolve", 1),
                result,
            );
            println!("{}", serde_json::to_string(&envelope)?);
            Ok(())
        }
        OutputFormat::Text => {
            println!(
                "INTEGRATION: action={:?} reason={} fingerprint={}",
                result.action, result.reason_code, result.decision_fingerprint
            );
            Ok(())
        }
    }
}

pub fn resolve_decision(
    roots: &ResolvedRoots,
    product: Product,
    fallback_mode: FallbackMode,
) -> Result<IntegrationResult> {
    Ok(resolve_snapshot(roots, product, fallback_mode)?.result)
}

fn resolve_snapshot(
    roots: &ResolvedRoots,
    product: Product,
    fallback_mode: FallbackMode,
) -> Result<ResolutionSnapshot> {
    let config_path = config_path()?;
    let repository_path = config_path_for_root(&roots.project_path);
    let repository_present = fs::symlink_metadata(&repository_path).is_ok();
    let identity = project_identity(roots);
    let read = read_config()?;

    let (config_state, config, mut diagnostics) = match read {
        ConfigRead::Missing => (ConfigState::Missing, None, Vec::new()),
        ConfigRead::Valid(config) => (ConfigState::Valid, Some(config), Vec::new()),
        ConfigRead::Fault { state, diagnostic } => (state, None, vec![diagnostic]),
    };
    let matches = config
        .as_ref()
        .map(|config| matching_rules(config, &identity))
        .unwrap_or_default();

    let (action, reason_code, rule, decision_catalog) = if matches.len() > 1 {
        (
            IntegrationAction::Block,
            "ambiguous-user-rule".to_string(),
            None,
            None,
        )
    } else if let Some(rule) = matches.first().copied() {
        match rule.mode {
            RuleMode::Exclude => (
                IntegrationAction::Exclude,
                "user-exclusion".to_string(),
                Some(rule),
                None,
            ),
            RuleMode::Enroll if repository_present => (
                IntegrationAction::Block,
                "catalog-conflict".to_string(),
                Some(rule),
                None,
            ),
            RuleMode::Enroll => match selected_user_catalog(rule, &identity, roots) {
                Ok(selected) => (
                    IntegrationAction::Integrate,
                    "user-enrollment".to_string(),
                    Some(rule),
                    Some(selected),
                ),
                Err((code, diagnostic)) => {
                    diagnostics.push(diagnostic);
                    (IntegrationAction::Block, code.to_string(), Some(rule), None)
                }
            },
        }
    } else if repository_present {
        match selected_repository_catalog(roots) {
            Ok(selected) => (
                IntegrationAction::Integrate,
                "repository-catalog".to_string(),
                None,
                Some(selected),
            ),
            Err(diagnostic) => {
                diagnostics.push(diagnostic);
                (
                    IntegrationAction::Block,
                    "repository-catalog-invalid".to_string(),
                    None,
                    None,
                )
            }
        }
    } else {
        (
            IntegrationAction::Unmanaged,
            "no-catalog".to_string(),
            None,
            None,
        )
    };

    let selected = decision_catalog
        .as_ref()
        .map(|decision| decision.selected.clone());
    let docs_home_digest = decision_catalog
        .as_ref()
        .map(|decision| decision.docs_home_digest.as_str())
        .unwrap_or("not-selected");
    let fingerprint_config_state = if !matches.is_empty()
        || !matches!(config_state, ConfigState::Missing | ConfigState::Valid)
    {
        Some(config_state)
    } else {
        None
    };
    let fingerprint_input = FingerprintInput {
        schema_version: 1,
        product,
        fallback_mode,
        project_path: &roots.project_path,
        git_common_dir: roots.git_common_dir.as_deref(),
        config_state: fingerprint_config_state,
        selector: rule.map(|rule| rule.selector),
        selector_path: rule.map(|rule| rule.path.as_path()),
        mode: rule.map(|rule| rule.mode),
        reason: rule.and_then(|rule| rule.reason.as_deref()),
        action,
        reason_code: &reason_code,
        selected_origin: selected.as_ref().map(|selected| selected.origin),
        selected_path: selected.as_ref().map(|selected| selected.path.as_path()),
        selected_digest: selected.as_ref().map(|selected| selected.digest.as_str()),
        docs_home_digest,
    };
    let fingerprint = decision_fingerprint(&fingerprint_input)?;
    let catalog = decision_catalog.map(|decision| EffectiveCatalog {
        catalog: decision.catalog,
        private_project_catalog: decision.private_project_catalog,
    });

    Ok(ResolutionSnapshot {
        result: IntegrationResult {
            action,
            reason_code,
            product,
            config_path,
            config_state,
            identity: IntegrationIdentity {
                project_path: roots.project_path.clone(),
                git_common_dir: roots.git_common_dir.clone(),
            },
            matched_selector: rule.map(|rule| MatchedSelector {
                kind: rule.selector,
            }),
            selected_catalog: selected,
            diagnostics,
            decision_fingerprint: fingerprint,
        },
        catalog,
    })
}

pub(crate) fn load_bound_catalog(
    roots: &ResolvedRoots,
    product: Product,
    fallback_mode: FallbackMode,
    expected_fingerprint: Option<&str>,
) -> std::result::Result<EffectiveCatalog, BoundCatalogError> {
    load_bound_catalog_with_fingerprint(roots, product, fallback_mode, expected_fingerprint)
        .map(|(catalog, _)| catalog)
}

pub(crate) fn load_bound_catalog_with_fingerprint(
    roots: &ResolvedRoots,
    product: Product,
    fallback_mode: FallbackMode,
    expected_fingerprint: Option<&str>,
) -> std::result::Result<(EffectiveCatalog, String), BoundCatalogError> {
    let snapshot =
        resolve_snapshot(roots, product, fallback_mode).map_err(BoundCatalogError::Resolution)?;
    if let Some(expected) = expected_fingerprint
        && expected != snapshot.result.decision_fingerprint
    {
        return Err(BoundCatalogError::StaleFingerprint {
            expected: expected.to_string(),
            current: snapshot.result.decision_fingerprint,
        });
    }
    if snapshot.result.action != IntegrationAction::Integrate {
        return Err(BoundCatalogError::CatalogNotSelected {
            action: snapshot.result.action,
            reason_code: snapshot.result.reason_code,
        });
    }
    let fingerprint = snapshot.result.decision_fingerprint;
    let catalog = snapshot.catalog.ok_or(BoundCatalogError::MissingCatalog)?;
    Ok((catalog, fingerprint))
}

fn selected_user_catalog(
    rule: &ProjectRule,
    identity: &ProjectIdentity,
    roots: &ResolvedRoots,
) -> std::result::Result<DecisionCatalog, (&'static str, ConfigDiagnostic)> {
    let path = rule.catalog.as_ref().expect("validated enrollment catalog");
    let snapshot = read_selected_catalog(path, identity).map_err(|err| {
        (
            "selected-catalog-unavailable",
            ConfigDiagnostic {
                code: "selected-catalog-unavailable".to_string(),
                message: err.to_string(),
            },
        )
    })?;
    let raw = std::str::from_utf8(&snapshot.bytes).map_err(|err| {
        (
            "selected-catalog-invalid",
            ConfigDiagnostic {
                code: "selected-catalog-invalid".to_string(),
                message: format!("private catalog is not valid UTF-8: {err}"),
            },
        )
    })?;
    let project = load_scope_catalog_from_str(
        Scope::Project,
        CatalogOrigin::User,
        &identity.project_path,
        &snapshot.path,
        raw,
    )
    .map_err(|err| {
        (
            "selected-catalog-invalid",
            ConfigDiagnostic {
                code: "selected-catalog-invalid".to_string(),
                message: err.to_string(),
            },
        )
    })?;
    let (home, docs_home_digest) = load_home_catalog(roots).map_err(|err| {
        (
            "effective-catalog-invalid",
            ConfigDiagnostic {
                code: "docs-home-catalog-invalid".to_string(),
                message: err.to_string(),
            },
        )
    })?;
    let digest = digest_bytes(&snapshot.bytes);
    Ok(DecisionCatalog {
        selected: SelectedCatalog {
            origin: SelectedCatalogOrigin::User,
            path: snapshot.path,
            digest,
        },
        catalog: LoadedCatalog {
            home,
            project: Some(project),
        },
        docs_home_digest,
        private_project_catalog: true,
    })
}

fn selected_repository_catalog(
    roots: &ResolvedRoots,
) -> std::result::Result<DecisionCatalog, ConfigDiagnostic> {
    load_repository_catalog(roots).map_err(|err| ConfigDiagnostic {
        code: "repository-catalog-invalid".to_string(),
        message: err.to_string(),
    })
}

fn load_repository_catalog(roots: &ResolvedRoots) -> Result<DecisionCatalog> {
    let project_path = config_path_for_root(&roots.project_path);
    let project_file = read_catalog_file(&project_path)?
        .ok_or_else(|| anyhow!("repository catalog {} disappeared", project_path.display()))?;
    let home_path = config_path_for_root(&roots.docs_home);
    let same_file = fs::canonicalize(&home_path)
        .ok()
        .is_some_and(|path| path == project_file.path);

    let (home, project, docs_home_digest) = if same_file {
        let home = parse_catalog_file(
            Scope::Home,
            CatalogOrigin::Home,
            &roots.docs_home,
            &project_file,
        )?;
        (Some(home), None, project_file.digest.clone())
    } else {
        let (home, digest) = load_home_catalog(roots)?;
        let project = parse_catalog_file(
            Scope::Project,
            CatalogOrigin::Repository,
            &roots.project_path,
            &project_file,
        )?;
        (home, Some(project), digest)
    };

    Ok(DecisionCatalog {
        selected: SelectedCatalog {
            origin: SelectedCatalogOrigin::Repository,
            path: project_file.path,
            digest: project_file.digest,
        },
        catalog: LoadedCatalog { home, project },
        docs_home_digest,
        private_project_catalog: false,
    })
}

fn load_home_catalog(roots: &ResolvedRoots) -> Result<(Option<ScopeCatalog>, String)> {
    let path = config_path_for_root(&roots.docs_home);
    let Some(file) = read_catalog_file(&path)? else {
        return Ok((None, "missing".to_string()));
    };
    let catalog = parse_catalog_file(Scope::Home, CatalogOrigin::Home, &roots.docs_home, &file)?;
    Ok((Some(catalog), file.digest))
}

fn read_catalog_file(path: &Path) -> Result<Option<CatalogFile>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    if bytes.len() > crate::user_config::MAX_PRIVATE_CATALOG_BYTES {
        bail!(
            "catalog {} exceeds the {}-byte size limit",
            path.display(),
            crate::user_config::MAX_PRIVATE_CATALOG_BYTES
        );
    }
    let raw = String::from_utf8(bytes.clone())
        .with_context(|| format!("catalog {} is not valid UTF-8", path.display()))?;
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("failed to canonicalize catalog {}", path.display()))?;
    Ok(Some(CatalogFile {
        path: canonical,
        raw,
        digest: digest_bytes(&bytes),
    }))
}

fn parse_catalog_file(
    scope: Scope,
    origin: CatalogOrigin,
    root: &Path,
    file: &CatalogFile,
) -> Result<ScopeCatalog> {
    load_scope_catalog_from_str(scope, origin, root, &file.path, &file.raw)
        .map_err(|err| anyhow!(err.to_string()))
}

fn digest_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decision_fingerprint(input: &FingerprintInput<'_>) -> Result<String> {
    let bytes = serde_json::to_vec(input).context("failed to encode integration decision")?;
    Ok(digest_bytes(&bytes))
}

fn project_identity(roots: &ResolvedRoots) -> ProjectIdentity {
    ProjectIdentity {
        project_path: roots.project_path.clone(),
        is_linked_worktree: roots.is_linked_worktree,
        git_common_dir: roots.git_common_dir.clone(),
        primary_worktree_path: roots.primary_worktree_path.clone(),
    }
}
