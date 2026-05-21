//! Read-only `agent-runtime doctor` probes. Plan 04 Sprint 3 Task 3.1.
//!
//! This sprint covers filesystem posture only: link-map symlinks,
//! managed-block marker pairing, and runtime-roots path readability.
//! Version probes and upgrade suggestions land in later Sprint 3 tasks.

pub mod probes;

use crate::install::link_map::{LinkMap, LinkMapError};
use crate::install::overlay::{self, LinkMapOverlay, OverlaySummary};
use crate::install::plan::{InstallPlan, PlanError};
use crate::render::manifest::{ProductRoot, RuntimeRootsManifest, SCHEMA_VERSION};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct DoctorOptions {
    pub overlay_enabled: bool,
    pub overlay_path: Option<PathBuf>,
}

impl Default for DoctorOptions {
    fn default() -> Self {
        Self {
            overlay_enabled: true,
            overlay_path: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum DoctorError {
    #[error("runtime-roots: {0}")]
    RuntimeRoots(#[from] RuntimeRootsError),
    #[error("link-map: {0}")]
    LinkMap(#[from] LinkMapError),
    #[error("plan: {0}")]
    Plan(#[from] PlanError),
    #[error("unknown product `{product}`; expected `codex` or `claude`")]
    UnknownProduct { product: String },
}

#[derive(Debug, Error)]
pub enum RuntimeRootsError {
    #[error("missing runtime-roots manifest: {path}")]
    Missing { path: PathBuf },
    #[error("schema_version mismatch in {file}: expected {expected}, got {found}")]
    SchemaVersion {
        file: PathBuf,
        expected: u32,
        found: u32,
    },
    #[error("parse error in {file}: {source}")]
    Parse {
        file: PathBuf,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("io error reading {file}: {source}")]
    Io {
        file: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorSeverity {
    Ok,
    Warn,
    Block,
}

impl DoctorSeverity {
    pub fn exit_code(self) -> u8 {
        match self {
            DoctorSeverity::Ok => 0,
            DoctorSeverity::Warn => 1,
            DoctorSeverity::Block => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorFinding {
    pub product: String,
    pub check: &'static str,
    pub severity: DoctorSeverity,
    pub entry_id: Option<String>,
    pub path: Option<PathBuf>,
    pub message: String,
}

impl DoctorFinding {
    pub fn warn(
        product: &str,
        check: &'static str,
        entry_id: Option<String>,
        path: Option<PathBuf>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            product: product.to_string(),
            check,
            severity: DoctorSeverity::Warn,
            entry_id,
            path,
            message: message.into(),
        }
    }

    pub fn block(
        product: &str,
        check: &'static str,
        entry_id: Option<String>,
        path: Option<PathBuf>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            product: product.to_string(),
            check,
            severity: DoctorSeverity::Block,
            entry_id,
            path,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedRuntimeRoots {
    pub product: String,
    pub live_home: PathBuf,
    pub docs_home: PathBuf,
    pub state_home: PathBuf,
    pub plugin_root: Option<PathBuf>,
}

#[derive(Debug)]
pub struct DoctorOutcome {
    pub product: String,
    pub findings: Vec<DoctorFinding>,
    pub ok: usize,
    pub warn: usize,
    pub block: usize,
    pub overlay: Option<OverlaySummary>,
}

impl DoctorOutcome {
    pub fn total_checks(&self) -> usize {
        self.ok + self.warn + self.block
    }

    pub fn exit_code(&self) -> u8 {
        if self.block > 0 {
            DoctorSeverity::Block.exit_code()
        } else if self.warn > 0 {
            DoctorSeverity::Warn.exit_code()
        } else {
            DoctorSeverity::Ok.exit_code()
        }
    }
}

pub fn run(
    product: &str,
    source_root: &Path,
    live_home_override: Option<&Path>,
    state_home_override: Option<&Path>,
    options: &DoctorOptions,
) -> Result<DoctorOutcome, DoctorError> {
    let runtime_roots = load_runtime_roots(source_root)?;
    let product_root = product_root(&runtime_roots, product)?;
    let resolved_roots = resolve_runtime_roots(
        product,
        product_root,
        live_home_override,
        state_home_override,
    );

    let mut link_map = LinkMap::load(source_root, product)?;
    let mut overlay_summary = None;
    if options.overlay_enabled {
        let overlay_opt = match options.overlay_path.as_deref() {
            Some(path) => LinkMapOverlay::load_from(path)?,
            None => LinkMapOverlay::load_optional(source_root)?,
        };
        if let Some(overlay) = overlay_opt {
            let summary = overlay::apply(&mut link_map, &overlay)?;
            overlay_summary = Some(summary);
        }
    }

    let plan = InstallPlan::build(
        product,
        source_root,
        &resolved_roots.live_home,
        &resolved_roots.state_home,
        &link_map,
    )?;

    let mut report = probes::ProbeReport::default();
    report.extend(probes::runtime_roots(&resolved_roots));
    report.extend(probes::install_plan(product, &plan));

    let warn = report
        .findings
        .iter()
        .filter(|f| f.severity == DoctorSeverity::Warn)
        .count();
    let block = report
        .findings
        .iter()
        .filter(|f| f.severity == DoctorSeverity::Block)
        .count();

    Ok(DoctorOutcome {
        product: product.to_string(),
        findings: report.findings,
        ok: report.ok,
        warn,
        block,
        overlay: overlay_summary,
    })
}

fn load_runtime_roots(source_root: &Path) -> Result<RuntimeRootsManifest, RuntimeRootsError> {
    let file = source_root.join("manifests").join("runtime-roots.yaml");
    if !file.exists() {
        return Err(RuntimeRootsError::Missing { path: file });
    }
    let raw = std::fs::read_to_string(&file).map_err(|source| RuntimeRootsError::Io {
        file: file.clone(),
        source,
    })?;
    let parsed: RuntimeRootsManifest =
        serde_yaml_ng::from_str(&raw).map_err(|source| RuntimeRootsError::Parse {
            file: file.clone(),
            source,
        })?;
    if parsed.schema_version != SCHEMA_VERSION {
        return Err(RuntimeRootsError::SchemaVersion {
            file,
            expected: SCHEMA_VERSION,
            found: parsed.schema_version,
        });
    }
    Ok(parsed)
}

fn product_root<'a>(
    runtime_roots: &'a RuntimeRootsManifest,
    product: &str,
) -> Result<&'a ProductRoot, DoctorError> {
    match product {
        "codex" => Ok(&runtime_roots.products.codex),
        "claude" => Ok(&runtime_roots.products.claude),
        other => Err(DoctorError::UnknownProduct {
            product: other.to_string(),
        }),
    }
}

fn resolve_runtime_roots(
    product: &str,
    root: &ProductRoot,
    live_home_override: Option<&Path>,
    state_home_override: Option<&Path>,
) -> ResolvedRuntimeRoots {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    resolve_runtime_roots_with_env(product, root, live_home_override, state_home_override, env)
}

fn resolve_runtime_roots_with_env(
    product: &str,
    root: &ProductRoot,
    live_home_override: Option<&Path>,
    state_home_override: Option<&Path>,
    mut env: BTreeMap<String, String>,
) -> ResolvedRuntimeRoots {
    if let Some(live_home) = live_home_override
        && product == "codex"
    {
        env.insert(
            "CODEX_HOME".to_string(),
            live_home.to_string_lossy().into_owned(),
        );
    }

    let live_home = live_home_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(expand_env_vars(&root.live_home, &env)));
    let docs_home = resolve_product_path(product, &root.docs_home, live_home_override, &env);
    let state_home = state_home_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(expand_env_vars(&root.state_home, &env)));
    let plugin_root = resolve_plugin_root(product, root, live_home_override, &env);

    ResolvedRuntimeRoots {
        product: product.to_string(),
        live_home,
        docs_home,
        state_home,
        plugin_root,
    }
}

fn resolve_plugin_root(
    product: &str,
    root: &ProductRoot,
    live_home_override: Option<&Path>,
    env: &BTreeMap<String, String>,
) -> Option<PathBuf> {
    if let Some(raw) = root.plugin_root.as_deref() {
        return Some(resolve_product_path(product, raw, live_home_override, env));
    }
    let name = root.plugin_root_env.as_deref()?;
    let value = env.get(name)?;
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

fn resolve_product_path(
    product: &str,
    raw: &str,
    live_home_override: Option<&Path>,
    env: &BTreeMap<String, String>,
) -> PathBuf {
    if let Some(live_home) = live_home_override {
        if product == "claude" {
            if raw == "$HOME/.claude" {
                return live_home.to_path_buf();
            }
            if let Some(rest) = raw.strip_prefix("$HOME/.claude/") {
                return live_home.join(rest);
            }
        }
        if product == "codex" {
            if raw == "$CODEX_HOME" {
                return live_home.to_path_buf();
            }
            if let Some(rest) = raw.strip_prefix("$CODEX_HOME/") {
                return live_home.join(rest);
            }
        }
    }
    PathBuf::from(expand_env_vars(raw, env))
}

fn expand_env_vars(raw: &str, env: &BTreeMap<String, String>) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        if chars.get(i + 1) == Some(&'{')
            && let Some(end) = find_matching_brace(&chars, i + 1)
        {
            let expr: String = chars[i + 2..end].iter().collect();
            out.push_str(&expand_braced_expr(&expr, env));
            i = end + 1;
            continue;
        }
        let mut end = i + 1;
        while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        if end == i + 1 {
            out.push('$');
            i += 1;
            continue;
        }
        let name: String = chars[i + 1..end].iter().collect();
        out.push_str(env.get(&name).map(String::as_str).unwrap_or(""));
        i = end;
    }
    out
}

fn find_matching_brace(chars: &[char], open_brace: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open_brace + 1;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
            depth += 1;
            i += 2;
            continue;
        }
        if chars[i] == '}' {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
        i += 1;
    }
    None
}

fn expand_braced_expr(expr: &str, env: &BTreeMap<String, String>) -> String {
    if let Some((name, fallback)) = expr.split_once(":-") {
        if let Some(value) = env.get(name)
            && !value.is_empty()
        {
            return value.clone();
        }
        expand_env_vars(fallback, env)
    } else {
        env.get(expr).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_expansion_supports_nested_default_values() {
        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), "/tmp/home".to_string());
        assert_eq!(
            expand_env_vars("${CODEX_AGENT_STATE_HOME:-$HOME/.local/state}", &env),
            "/tmp/home/.local/state"
        );
    }

    #[test]
    fn env_expansion_supports_nested_braced_default_values() {
        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), "/tmp/home".to_string());
        assert_eq!(
            expand_env_vars(
                "${CODEX_AGENT_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/agent-runtime-kit/codex}",
                &env
            ),
            "/tmp/home/.local/state/agent-runtime-kit/codex"
        );

        env.insert("XDG_STATE_HOME".to_string(), "/tmp/state".to_string());
        assert_eq!(
            expand_env_vars(
                "${CODEX_AGENT_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/agent-runtime-kit/codex}",
                &env
            ),
            "/tmp/state/agent-runtime-kit/codex"
        );

        env.insert(
            "CODEX_AGENT_STATE_HOME".to_string(),
            "/tmp/codex-state".to_string(),
        );
        assert_eq!(
            expand_env_vars(
                "${CODEX_AGENT_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/agent-runtime-kit/codex}",
                &env
            ),
            "/tmp/codex-state"
        );
    }

    #[test]
    fn runtime_root_resolution_uses_plugin_root_env_when_set() {
        let root = ProductRoot {
            live_home: "$HOME/.claude".to_string(),
            docs_home: "$HOME/.claude".to_string(),
            state_home: "$HOME/.local/state/agent-runtime-kit/claude".to_string(),
            plugin_root: None,
            plugin_root_env: Some("CLAUDE_PLUGIN_ROOT".to_string()),
            hook_config_strategy: None,
            min_version: "0.0.0".to_string(),
            recommended_version: "0.0.0".to_string(),
            min_version_effective_from: "2099-01-01".to_string(),
            version_probe: "claude --version".to_string(),
        };
        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), "/tmp/home".to_string());
        env.insert(
            "CLAUDE_PLUGIN_ROOT".to_string(),
            "/tmp/claude-plugin".to_string(),
        );

        let resolved = resolve_runtime_roots_with_env("claude", &root, None, None, env);

        assert_eq!(
            resolved.plugin_root,
            Some(PathBuf::from("/tmp/claude-plugin"))
        );
    }
}
