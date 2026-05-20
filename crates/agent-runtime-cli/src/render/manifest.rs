//! Typed deserialization for the five Phase 1 manifests living under
//! `<source-root>/manifests/`. Every map visible to the Tera context layer
//! uses [`IndexMap`] or [`BTreeMap`] so render output stays deterministic
//! across processes (Resolved Decision #9 in
//! `inventory-target-architecture.md`).

use indexmap::IndexMap;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("missing manifest: {path} (source root: {root})")]
    Missing { path: PathBuf, root: PathBuf },
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
        source: serde_yml::Error,
    },
    #[error("io error reading {file}: {source}")]
    Io {
        file: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("source root could not be resolved ({path}): {source}")]
    SourceRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Canonicalised view of the directory passed via `--source-root`. The
/// canonical path is computed once at construction so every later resolve
/// walks the same tree.
#[derive(Debug, Clone)]
pub struct SourceRoot {
    path: PathBuf,
}

impl SourceRoot {
    /// Build a [`SourceRoot`] from an explicit `--source-root` argument, or
    /// fall back to the current working directory. The resulting path is
    /// canonicalised so symlinks resolve to a stable target.
    pub fn from_arg_or_cwd(arg: Option<&Path>) -> Result<Self, ManifestError> {
        let raw = match arg {
            Some(p) => p.to_path_buf(),
            None => std::env::current_dir().map_err(|source| ManifestError::SourceRoot {
                path: PathBuf::from("."),
                source,
            })?,
        };
        let canonical = raw
            .canonicalize()
            .map_err(|source| ManifestError::SourceRoot {
                path: raw.clone(),
                source,
            })?;
        Ok(Self { path: canonical })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn manifests_dir(&self) -> PathBuf {
        self.path.join("manifests")
    }
}

/// Bundle of every Phase 1 manifest. Constructed by [`load_all`].
#[derive(Debug)]
pub struct ManifestSet {
    pub skills: SkillsManifest,
    pub plugins: PluginsManifest,
    pub product_capabilities: ProductCapabilitiesManifest,
    pub runtime_roots: RuntimeRootsManifest,
    pub cli_tools: CliToolsManifest,
}

/// Read and validate every Phase 1 manifest under
/// `<source_root>/manifests/`. Each manifest's `schema_version` is
/// asserted to equal [`SCHEMA_VERSION`].
pub fn load_all(root: &SourceRoot) -> Result<ManifestSet, ManifestError> {
    let dir = root.manifests_dir();
    Ok(ManifestSet {
        skills: load::<SkillsManifest>(&dir.join("skills.yaml"), &root.path)?,
        plugins: load::<PluginsManifest>(&dir.join("plugins.yaml"), &root.path)?,
        product_capabilities: load::<ProductCapabilitiesManifest>(
            &dir.join("product-capabilities.yaml"),
            &root.path,
        )?,
        runtime_roots: load::<RuntimeRootsManifest>(&dir.join("runtime-roots.yaml"), &root.path)?,
        cli_tools: load::<CliToolsManifest>(&dir.join("cli-tools.yaml"), &root.path)?,
    })
}

trait WithSchemaVersion {
    fn schema_version(&self) -> u32;
}

fn load<T>(file: &Path, root: &Path) -> Result<T, ManifestError>
where
    T: for<'de> Deserialize<'de> + WithSchemaVersion,
{
    if !file.exists() {
        return Err(ManifestError::Missing {
            path: file.to_path_buf(),
            root: root.to_path_buf(),
        });
    }
    let raw = std::fs::read_to_string(file).map_err(|source| ManifestError::Io {
        file: file.to_path_buf(),
        source,
    })?;
    let parsed: T = serde_yml::from_str(&raw).map_err(|source| ManifestError::Parse {
        file: file.to_path_buf(),
        source,
    })?;
    if parsed.schema_version() != SCHEMA_VERSION {
        return Err(ManifestError::SchemaVersion {
            file: file.to_path_buf(),
            expected: SCHEMA_VERSION,
            found: parsed.schema_version(),
        });
    }
    Ok(parsed)
}

// ---------------------------------------------------------------------------
// skills.yaml
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsManifest {
    pub schema_version: u32,
    pub skills: Vec<Skill>,
}

impl WithSchemaVersion for SkillsManifest {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skill {
    pub id: String,
    pub domain: String,
    pub source: String,
    #[serde(default)]
    pub products: IndexMap<String, ProductRender>,
    pub required_clis: IndexMap<String, String>,
    #[serde(default)]
    pub state_out_mode: StateOutMode,
    #[serde(default)]
    pub aliases: IndexMap<String, String>,
    #[serde(default)]
    pub divergent: bool,
    #[serde(default)]
    pub portability_notes: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StateOutMode {
    #[default]
    Runtime,
    Literal,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductRender {
    #[serde(default)]
    pub name: Option<String>,
    pub render_to: String,
    #[serde(default)]
    pub path_override: Option<String>,
}

// ---------------------------------------------------------------------------
// plugins.yaml
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginsManifest {
    pub schema_version: u32,
    pub plugins: Vec<Plugin>,
}

impl WithSchemaVersion for PluginsManifest {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    pub id: String,
    pub domain: String,
    pub contained_skills: Vec<String>,
    pub product_manifests: IndexMap<String, String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub install_policy: InstallPolicy,
}

#[derive(Debug, Deserialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InstallPolicy {
    #[default]
    Always,
    OptIn,
    Experimental,
}

// ---------------------------------------------------------------------------
// product-capabilities.yaml
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductCapabilitiesManifest {
    pub schema_version: u32,
    pub products: ProductCapabilitiesProducts,
    #[serde(default)]
    pub plugin_manifest_diff: Option<PluginManifestDiff>,
}

impl WithSchemaVersion for ProductCapabilitiesManifest {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductCapabilitiesProducts {
    pub codex: ProductCapability,
    pub claude: ProductCapability,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductCapability {
    pub nested_skill_support: bool,
    pub plugin_manifest: PluginManifestSpec,
    pub hooks_model: HooksModel,
    pub config_activation: Vec<String>,
    pub runtime_state: RuntimeState,
    #[serde(default)]
    pub marketplace_concept: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifestSpec {
    pub path_pattern: String,
    pub loaded_at_runtime: bool,
    pub schema_ref: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HooksModel {
    pub config_surface: String,
    pub payload_shape: String,
    #[serde(default)]
    pub supports_inline_python: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeState {
    pub state_home_env: String,
    pub default_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifestDiff {
    #[serde(default)]
    pub shared_fields: Vec<String>,
    #[serde(default)]
    pub codex_only_fields: Vec<String>,
    #[serde(default)]
    pub claude_only_fields: Vec<String>,
}

// ---------------------------------------------------------------------------
// runtime-roots.yaml
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRootsManifest {
    pub schema_version: u32,
    pub products: RuntimeRootsProducts,
    #[serde(default)]
    pub host_profiles: IndexMap<String, IndexMap<String, serde_yml::Value>>,
}

impl WithSchemaVersion for RuntimeRootsManifest {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRootsProducts {
    pub codex: ProductRoot,
    pub claude: ProductRoot,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductRoot {
    pub live_home: String,
    pub docs_home: String,
    pub state_home: String,
    #[serde(default)]
    pub plugin_root: Option<String>,
    #[serde(default)]
    pub plugin_root_env: Option<String>,
    #[serde(default)]
    pub hook_config_strategy: Option<HookConfigStrategy>,
    pub min_version: String,
    pub recommended_version: String,
    pub min_version_effective_from: String,
    pub version_probe: String,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HookConfigStrategy {
    SettingsJson,
    ManagedBlock,
}

// ---------------------------------------------------------------------------
// cli-tools.yaml
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliToolsManifest {
    pub schema_version: u32,
    pub profiles: CliToolsProfiles,
    pub formulas: IndexMap<String, CliToolFormula>,
}

impl WithSchemaVersion for CliToolsManifest {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliToolsProfiles {
    pub core: Vec<String>,
    pub recommended: Vec<String>,
    pub full: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliToolFormula {
    pub brew: String,
    pub command: String,
    #[serde(default)]
    pub linux_only_alternative: Option<String>,
    pub categories: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    const VALID_SKILLS: &str = "schema_version: 1\nskills: []\n";
    const VALID_PLUGINS: &str = "schema_version: 1\nplugins: []\n";
    const VALID_PRODUCT_CAPABILITIES: &str = r#"
schema_version: 1
products:
  codex:
    nested_skill_support: true
    plugin_manifest:
      path_pattern: "$CODEX_HOME/plugins/<domain>/.codex-plugin/plugin.json"
      loaded_at_runtime: false
      schema_ref: "core/docs/schemas/codex-plugin.schema.json"
    hooks_model:
      config_surface: "$CODEX_HOME/config.toml"
      payload_shape: "codex-toml-managed-block"
      supports_inline_python: false
    config_activation:
      - "$CODEX_HOME/AGENTS.md"
    runtime_state:
      state_home_env: "CODEX_AGENT_STATE_HOME"
      default_path: "${XDG_STATE_HOME:-$HOME/.local/state}/agent-runtime-kit/codex"
    marketplace_concept: false
  claude:
    nested_skill_support: true
    plugin_manifest:
      path_pattern: "${CLAUDE_PLUGIN_ROOT}/<plugin>/.claude-plugin/plugin.json"
      loaded_at_runtime: true
      schema_ref: "core/docs/schemas/claude-plugin.schema.json"
    hooks_model:
      config_surface: "$HOME/.claude/settings.json"
      payload_shape: "claude-pretool-v1"
      supports_inline_python: true
    config_activation:
      - "$HOME/.claude/settings.json"
    runtime_state:
      state_home_env: "CLAUDE_KIT_STATE_HOME"
      default_path: "${XDG_STATE_HOME:-$HOME/.local/state}/agent-runtime-kit/claude"
    marketplace_concept: true
"#;
    const VALID_RUNTIME_ROOTS: &str = r#"
schema_version: 1
products:
  codex:
    live_home: "$CODEX_HOME"
    docs_home: "$CODEX_HOME"
    state_home: "${CODEX_AGENT_STATE_HOME:-$HOME/.local/state}"
    plugin_root: "$CODEX_HOME/plugins"
    hook_config_strategy: managed-block
    min_version: "<TBD: pin during Phase 1>"
    recommended_version: "<TBD: pin during Phase 1>"
    min_version_effective_from: "<TBD: pin during Phase 1>"
    version_probe: "codex --version"
  claude:
    live_home: "$HOME/.claude"
    docs_home: "$HOME/.claude"
    state_home: "${CLAUDE_KIT_STATE_HOME:-$HOME/.local/state}"
    plugin_root_env: "CLAUDE_PLUGIN_ROOT"
    hook_config_strategy: settings-json
    min_version: "<TBD: pin during Phase 1>"
    recommended_version: "<TBD: pin during Phase 1>"
    min_version_effective_from: "<TBD: pin during Phase 1>"
    version_probe: "claude --version"
"#;
    const VALID_CLI_TOOLS: &str = r#"
schema_version: 1
profiles:
  core: [ripgrep]
  recommended: [ripgrep]
  full: [ripgrep]
formulas:
  ripgrep:
    brew: ripgrep
    command: rg
    linux_only_alternative: null
    categories: [search]
"#;

    fn write_fixture(dir: &Path, with_overrides: &[(&str, &str)]) {
        let manifests = dir.join("manifests");
        fs::create_dir_all(&manifests).unwrap();
        let mut files: IndexMap<&str, &str> = IndexMap::new();
        files.insert("skills.yaml", VALID_SKILLS);
        files.insert("plugins.yaml", VALID_PLUGINS);
        files.insert("product-capabilities.yaml", VALID_PRODUCT_CAPABILITIES);
        files.insert("runtime-roots.yaml", VALID_RUNTIME_ROOTS);
        files.insert("cli-tools.yaml", VALID_CLI_TOOLS);
        for (name, body) in with_overrides {
            files.insert(*name, *body);
        }
        for (name, body) in files {
            fs::write(manifests.join(name), body).unwrap();
        }
    }

    #[test]
    fn load_all_round_trips_a_minimal_fixture() {
        let tmp = TempDir::new().unwrap();
        write_fixture(tmp.path(), &[]);
        let root = SourceRoot::from_arg_or_cwd(Some(tmp.path())).unwrap();
        let set = load_all(&root).unwrap();
        assert!(set.skills.skills.is_empty());
        assert!(set.plugins.plugins.is_empty());
        assert!(set.product_capabilities.products.codex.nested_skill_support);
        assert!(set.product_capabilities.products.claude.marketplace_concept);
        assert_eq!(
            set.runtime_roots.products.codex.version_probe,
            "codex --version"
        );
        assert_eq!(set.cli_tools.profiles.core, vec!["ripgrep".to_string()]);
        let ripgrep = set.cli_tools.formulas.get("ripgrep").unwrap();
        assert_eq!(ripgrep.brew, "ripgrep");
        assert_eq!(ripgrep.command, "rg");
        assert_eq!(ripgrep.categories, vec!["search".to_string()]);
        assert!(ripgrep.linux_only_alternative.is_none());
    }

    #[test]
    fn source_root_canonicalises_relative_paths() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("manifests")).unwrap();
        // Pass a path with a ./ prefix to force resolution.
        let dotted = tmp.path().join(".").join("manifests").join("..");
        let root = SourceRoot::from_arg_or_cwd(Some(&dotted)).unwrap();
        // The canonical path matches the canonical tmp path.
        assert_eq!(root.path(), tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn missing_manifest_reports_file_and_root() {
        let tmp = TempDir::new().unwrap();
        let manifests = tmp.path().join("manifests");
        fs::create_dir_all(&manifests).unwrap();
        // Only one manifest present; the rest are missing.
        fs::write(manifests.join("skills.yaml"), VALID_SKILLS).unwrap();
        let root = SourceRoot::from_arg_or_cwd(Some(tmp.path())).unwrap();
        let err = load_all(&root).unwrap_err();
        match err {
            ManifestError::Missing {
                path,
                root: root_path,
            } => {
                assert!(path.ends_with("plugins.yaml"));
                assert_eq!(root_path, tmp.path().canonicalize().unwrap());
            }
            other => panic!("expected ManifestError::Missing, got {other:?}"),
        }
    }

    #[test]
    fn unknown_field_in_skills_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let mutated = "schema_version: 1\nskills: []\nbogus: true\n";
        write_fixture(tmp.path(), &[("skills.yaml", mutated)]);
        let root = SourceRoot::from_arg_or_cwd(Some(tmp.path())).unwrap();
        let err = load_all(&root).unwrap_err();
        match err {
            ManifestError::Parse { file, .. } => {
                assert!(file.ends_with("skills.yaml"));
            }
            other => panic!("expected ManifestError::Parse, got {other:?}"),
        }
    }

    #[test]
    fn wrong_schema_version_is_rejected_with_file_path() {
        let tmp = TempDir::new().unwrap();
        let mutated = "schema_version: 2\nplugins: []\n";
        write_fixture(tmp.path(), &[("plugins.yaml", mutated)]);
        let root = SourceRoot::from_arg_or_cwd(Some(tmp.path())).unwrap();
        let err = load_all(&root).unwrap_err();
        match err {
            ManifestError::SchemaVersion {
                file,
                expected,
                found,
            } => {
                assert!(file.ends_with("plugins.yaml"));
                assert_eq!(expected, SCHEMA_VERSION);
                assert_eq!(found, 2);
            }
            other => panic!("expected ManifestError::SchemaVersion, got {other:?}"),
        }
    }

    #[test]
    fn state_out_mode_defaults_to_runtime() {
        let yaml = r#"
schema_version: 1
skills:
  - id: domain.skill
    domain: domain
    source: core/skills/domain/skill
    products: {}
    required_clis:
      agent-runtime: ">=0.13.0"
"#;
        let parsed: SkillsManifest = serde_yml::from_str(yaml).unwrap();
        let skill = parsed.skills.first().unwrap();
        assert_eq!(skill.state_out_mode, StateOutMode::Runtime);
        assert!(!skill.divergent);
    }

    #[test]
    fn install_policy_accepts_kebab_case() {
        let yaml = r#"
schema_version: 1
plugins:
  - id: domain
    domain: domain
    contained_skills: []
    product_manifests: {}
    install_policy: opt-in
"#;
        let parsed: PluginsManifest = serde_yml::from_str(yaml).unwrap();
        assert_eq!(
            parsed.plugins.first().unwrap().install_policy,
            InstallPolicy::OptIn,
        );
    }

    #[test]
    fn source_root_rejects_nonexistent_path() {
        let bogus = Path::new("/this/path/should/not/exist/agent-runtime-cli-test");
        let err = SourceRoot::from_arg_or_cwd(Some(bogus)).unwrap_err();
        match err {
            ManifestError::SourceRoot { path, .. } => assert_eq!(path, bogus),
            other => panic!("expected ManifestError::SourceRoot, got {other:?}"),
        }
    }
}
