//! Tera helper functions registered against a per-skill render pass.
//!
//! Each helper is constructed from a shared [`HelperContext`] that captures
//! everything required for resolution: the source root, the parsed manifest
//! bundle, the active product, and the current skill's `required_clis` /
//! `state_out_mode`. Helpers are stateless beyond this snapshot, so two
//! cold processes registering the same context produce identical output.
//!
//! Tera's `Function` trait signature forces `&HashMap<String, Value>` on
//! us — that's the only sanctioned `HashMap` import inside `src/render/`
//! and it stays scoped to this module (the context fed to Tera lives in
//! [`IndexMap`] / `BTreeMap` per Resolved Decision #9). The crate-wide
//! `clippy::disallowed_types` gate on `HashMap` is silenced exactly here
//! and nowhere else.
#![allow(clippy::disallowed_types)]

use crate::render::manifest::{ManifestSet, StateOutMode};
use indexmap::IndexMap;
use nils_markdown::Engine;
use std::path::PathBuf;
use std::sync::Arc;

pub mod cli_ref;
pub mod script;
pub mod skill_ref;
pub mod state_out;

/// Snapshot of the resolution context that all four helpers share for a
/// single skill's render. Cloned via [`Arc`] into each helper closure.
#[derive(Debug, Clone)]
pub struct HelperContext {
    pub source_root: PathBuf,
    pub manifests: Arc<ManifestSet>,
    pub current_product: String,
    pub current_skill_id: String,
    pub current_skill_required_clis: IndexMap<String, String>,
    pub current_skill_state_out_mode: StateOutMode,
}

/// Register every renderer helper on `engine` with the shared `ctx`.
///
/// `agent-runtime-cli` keeps the four helper bodies in this crate
/// because they bind to its manifest domain (`ManifestSet`,
/// `StateOutMode`, `CliToolsManifest`); `nils-markdown` exposes a
/// generic [`Engine::register_helper`] extension point that this
/// function plugs into.
pub fn register_all(engine: &mut Engine, ctx: Arc<HelperContext>) {
    engine.register_helper("script", script::make(ctx.clone()));
    engine.register_helper("skill_ref", skill_ref::make(ctx.clone()));
    engine.register_helper("state_out", state_out::make(ctx.clone()));
    engine.register_helper("cli_ref", cli_ref::make(ctx));
}

#[cfg(test)]
mod test_support {
    use super::*;
    use crate::render::manifest::{
        CliToolFormula, CliToolsManifest, CliToolsProfiles, HooksModel, PluginManifestSpec,
        PluginsManifest, ProductCapabilitiesManifest, ProductCapabilitiesProducts,
        ProductCapability, ProductRender, ProductRoot, RuntimeRootsManifest, RuntimeRootsProducts,
        RuntimeState, Skill, SkillProducts, SkillsManifest, StateOutMode,
    };

    fn product_capability() -> ProductCapability {
        ProductCapability {
            nested_skill_support: true,
            plugin_manifest: PluginManifestSpec {
                path_pattern: "ignored".to_string(),
                loaded_at_runtime: false,
                schema_ref: "ignored".to_string(),
            },
            hooks_model: HooksModel {
                config_surface: "ignored".to_string(),
                payload_shape: "ignored".to_string(),
                supports_inline_python: false,
            },
            config_activation: vec!["ignored".to_string()],
            runtime_state: RuntimeState {
                state_home_env: "STATE".to_string(),
                default_path: "/tmp/state".to_string(),
            },
            marketplace_concept: false,
        }
    }

    fn product_root() -> ProductRoot {
        ProductRoot {
            live_home: "/tmp/live".to_string(),
            docs_home: "/tmp/docs".to_string(),
            state_home: "/tmp/state".to_string(),
            plugin_root: None,
            plugin_root_env: None,
            hook_config_strategy: None,
            min_version: "<TBD: pin during Phase 1>".to_string(),
            recommended_version: "<TBD: pin during Phase 1>".to_string(),
            min_version_effective_from: "<TBD: pin during Phase 1>".to_string(),
            version_probe: "probe".to_string(),
        }
    }

    pub fn fixture_manifests() -> ManifestSet {
        let products = SkillProducts {
            codex: Some(ProductRender {
                name: Some("/codex-name".to_string()),
                render_to: "skills/sample/SKILL.md".to_string(),
                path_override: None,
            }),
            claude: Some(ProductRender {
                name: Some("market:favorites".to_string()),
                render_to: "plugins/market/skills/favorites/SKILL.md".to_string(),
                path_override: None,
            }),
        };
        let mut required_clis = IndexMap::new();
        required_clis.insert("agent-out".to_string(), ">=0.5.0".to_string());
        required_clis.insert("market-cli".to_string(), ">=0.4.0".to_string());
        let skill = Skill {
            id: "market.favorites".to_string(),
            domain: "market".to_string(),
            source: "core/skills/market/favorites".to_string(),
            products,
            required_clis,
            state_out_mode: StateOutMode::Runtime,
            aliases: IndexMap::new(),
            divergent: false,
            portability_notes: None,
        };
        let mut formulas: IndexMap<String, CliToolFormula> = IndexMap::new();
        formulas.insert(
            "ripgrep".to_string(),
            CliToolFormula {
                brew: "ripgrep".to_string(),
                command: "rg".to_string(),
                linux_only_alternative: None,
                categories: vec!["search".to_string()],
                notes: None,
            },
        );
        ManifestSet {
            skills: SkillsManifest {
                schema_version: 1,
                skills: vec![skill],
            },
            plugins: PluginsManifest {
                schema_version: 1,
                plugins: vec![],
            },
            product_capabilities: ProductCapabilitiesManifest {
                schema_version: 1,
                products: ProductCapabilitiesProducts {
                    codex: product_capability(),
                    claude: product_capability(),
                },
                plugin_manifest_diff: None,
            },
            runtime_roots: RuntimeRootsManifest {
                schema_version: 1,
                products: RuntimeRootsProducts {
                    codex: product_root(),
                    claude: product_root(),
                },
                host_profiles: IndexMap::new(),
            },
            cli_tools: CliToolsManifest {
                schema_version: 1,
                profiles: CliToolsProfiles {
                    core: vec!["ripgrep".to_string()],
                    recommended: vec!["ripgrep".to_string()],
                    full: vec!["ripgrep".to_string()],
                },
                formulas,
            },
        }
    }

    pub fn fixture_context(product: &str) -> HelperContext {
        let manifests = Arc::new(fixture_manifests());
        let skill = &manifests.skills.skills[0];
        HelperContext {
            source_root: PathBuf::from("/tmp/source-root"),
            current_product: product.to_string(),
            current_skill_id: skill.id.clone(),
            current_skill_required_clis: skill.required_clis.clone(),
            current_skill_state_out_mode: skill.state_out_mode,
            manifests,
        }
    }

    pub fn render(template: &str, ctx: HelperContext) -> tera::Result<String> {
        let mut engine = Engine::builder().build();
        register_all(&mut engine, Arc::new(ctx));
        engine
            .render_str(template, &serde_json::Value::Null)
            .map_err(|err| match err {
                nils_markdown::RenderError::Render { source, .. } => source,
                other => tera::Error::msg(format!("{other}")),
            })
    }

    /// Flatten a Tera error and its source chain into a single string so
    /// rejection tests can assert against the originating helper message,
    /// not Tera's outer `Failed to render '__tera_one_off'` wrapper.
    pub fn format_err(err: &tera::Error) -> String {
        let mut out = err.to_string();
        let mut next: Option<&dyn std::error::Error> = std::error::Error::source(err);
        while let Some(cause) = next {
            out.push_str("\n  caused by: ");
            out.push_str(&cause.to_string());
            next = cause.source();
        }
        out
    }
}
