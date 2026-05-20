//! Per-product render writer. Walks the skills declared for the active
//! product, renders each via Tera + helpers when its input hash differs
//! from the recorded cache entry, and writes the output under
//! `<source-root>/build/<product>/`. Skills with a cache hit are left
//! on disk verbatim — the determinism contract guarantees the cache-hit
//! path is byte-identical to a fresh render of the same source.
//!
//! Render only opens paths under `<source-root>/`. Every read is rooted
//! through [`source_path`](Self::source_path) and joined to the
//! canonical source root, so a malicious `skill.source` like
//! `../../etc` lands outside the source root and is rejected before any
//! I/O happens.

use crate::render::cache::{CACHE_FILE, CacheEntry, RenderCache};
use crate::render::helpers::{HelperContext, register_all};
use crate::render::manifest::{ManifestSet, Skill, SourceRoot};
use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tera::Tera;

pub const SKILL_TEMPLATE_FILE: &str = "SKILL.md.tera";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RenderReport {
    pub product: String,
    pub output_root: PathBuf,
    pub rendered: Vec<String>,
    pub cached: Vec<String>,
    pub skipped: Vec<String>,
}

/// Render every skill declared for `product` from manifests rooted at
/// `root`. Returns a summary describing which skills were rendered,
/// served from cache, or skipped.
pub fn write_product(
    root: &SourceRoot,
    manifests: Arc<ManifestSet>,
    product: &str,
) -> Result<RenderReport> {
    require_known_product(&manifests, product)?;
    let output_root = root.path().join("build").join(product);
    fs::create_dir_all(&output_root)
        .with_context(|| format!("create_dir_all {}", output_root.display()))?;

    let cache_path = output_root.join(CACHE_FILE);
    let prior_cache = RenderCache::load_or_empty(&cache_path);
    let manifest_bytes = read_manifest_bundle(root)?;
    let mut next_cache = RenderCache::empty();
    let mut report = RenderReport {
        product: product.to_string(),
        output_root: output_root.clone(),
        ..RenderReport::default()
    };

    for skill in &manifests.skills.skills {
        let Some(render) = skill.products.get(product) else {
            report.skipped.push(skill.id.clone());
            continue;
        };
        let template_path = sandboxed_join(root.path(), &skill.source)?.join(SKILL_TEMPLATE_FILE);
        let template_body = fs::read_to_string(&template_path).with_context(|| {
            format!(
                "read template {} for skill {}",
                template_path.display(),
                skill.id
            )
        })?;

        let input_hash = input_hash(
            product,
            skill,
            &render.render_to,
            &template_body,
            &manifest_bytes,
        );
        let entry = CacheEntry {
            hash: input_hash.clone(),
            output: render.render_to.clone(),
        };
        let output_path = sandboxed_join(&output_root, &render.render_to)?;
        let cache_hit = prior_cache
            .skills
            .get(&skill.id)
            .is_some_and(|prior| prior == &entry)
            && output_path.exists();

        if cache_hit {
            report.cached.push(skill.id.clone());
        } else {
            let rendered =
                render_template(root.path(), &manifests, product, skill, &template_body)?;
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create_dir_all {}", parent.display()))?;
            }
            fs::write(&output_path, rendered.as_bytes())
                .with_context(|| format!("write {}", output_path.display()))?;
            report.rendered.push(skill.id.clone());
        }
        next_cache.skills.insert(skill.id.clone(), entry);
    }

    next_cache
        .save(&cache_path)
        .with_context(|| format!("write {}", cache_path.display()))?;
    Ok(report)
}

fn require_known_product(manifests: &ManifestSet, product: &str) -> Result<()> {
    match product {
        "codex" | "claude" => Ok(()),
        other => Err(anyhow!(
            "unknown --product {other:?}; supported: codex, claude. \
             schema_version={}",
            manifests.product_capabilities.schema_version
        )),
    }
}

fn render_template(
    source_root: &Path,
    manifests: &Arc<ManifestSet>,
    product: &str,
    skill: &Skill,
    template_body: &str,
) -> Result<String> {
    let ctx = HelperContext {
        source_root: source_root.to_path_buf(),
        manifests: manifests.clone(),
        current_product: product.to_string(),
        current_skill_id: skill.id.clone(),
        current_skill_required_clis: skill.required_clis.clone(),
        current_skill_state_out_mode: skill.state_out_mode,
    };
    let mut tera = Tera::default();
    register_all(&mut tera, Arc::new(ctx));
    tera.render_str(template_body, &tera::Context::new())
        .with_context(|| format!("render skill {}", skill.id))
}

struct ManifestBytes {
    skills: Vec<u8>,
    plugins: Vec<u8>,
    product_capabilities: Vec<u8>,
    runtime_roots: Vec<u8>,
    cli_tools: Vec<u8>,
}

fn read_manifest_bundle(root: &SourceRoot) -> Result<ManifestBytes> {
    let dir = root.manifests_dir();
    let read = |name: &str| -> Result<Vec<u8>> {
        let path = dir.join(name);
        fs::read(&path).with_context(|| format!("hash-read {}", path.display()))
    };
    Ok(ManifestBytes {
        skills: read("skills.yaml")?,
        plugins: read("plugins.yaml")?,
        product_capabilities: read("product-capabilities.yaml")?,
        runtime_roots: read("runtime-roots.yaml")?,
        cli_tools: read("cli-tools.yaml")?,
    })
}

fn input_hash(
    product: &str,
    skill: &Skill,
    render_to: &str,
    template_body: &str,
    manifests: &ManifestBytes,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"agent-runtime-cli render v1\0");
    hasher.update(product.as_bytes());
    hasher.update(b"\0");
    hasher.update(skill.id.as_bytes());
    hasher.update(b"\0");
    hasher.update(render_to.as_bytes());
    hasher.update(b"\0");
    hasher.update(template_body.as_bytes());
    hasher.update(b"\0");
    // Whole-file manifest bytes — keeps the hash sensitive to any change
    // in a file the helpers might consume, at the cost of a coarse cache
    // invalidation when an unrelated manifest line shifts.
    hasher.update(&manifests.skills);
    hasher.update(b"\0");
    hasher.update(&manifests.plugins);
    hasher.update(b"\0");
    hasher.update(&manifests.product_capabilities);
    hasher.update(b"\0");
    hasher.update(&manifests.runtime_roots);
    hasher.update(b"\0");
    hasher.update(&manifests.cli_tools);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(7 + digest.len() * 2);
    out.push_str("sha256:");
    for byte in digest.iter() {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// Join `relative` onto `base` after rejecting any `..` segments. Used
/// for every render-time path so we cannot escape `<source-root>/` via
/// a hostile `skill.source`, `render_to`, or `--source-root` value.
pub(crate) fn sandboxed_join(base: &Path, relative: &str) -> Result<PathBuf> {
    let rel = PathBuf::from(relative);
    for component in rel.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(anyhow!(
                    "path {relative:?} contains a `..` segment; render must stay under {base}",
                    base = base.display(),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!(
                    "path {relative:?} is absolute; render must stay under {base}",
                    base = base.display(),
                ));
            }
        }
    }
    Ok(base.join(rel))
}

/// Map of the rendered output bytes keyed by path-under-output-root.
/// Test helper for cache-hit-vs-cache-miss byte equality assertions.
#[cfg(test)]
pub(crate) fn snapshot_outputs(output_root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    walk(output_root, output_root, &mut out);
    out
}

#[cfg(test)]
fn walk(base: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            walk(base, &path, out);
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some(CACHE_FILE) {
            continue;
        }
        let bytes = fs::read(&path).unwrap();
        let rel = path
            .strip_prefix(base)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        out.insert(rel, bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::cache::RenderCache;
    use tempfile::TempDir;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    const SKILLS_FIXTURE: &str = r#"
schema_version: 1
skills:
  - id: market.favorites
    domain: market
    source: core/skills/market/favorites
    products:
      codex:
        name: /market-favorites
        render_to: skills/market/favorites/SKILL.md
      claude:
        name: market:favorites
        render_to: plugins/market/skills/favorites/SKILL.md
    required_clis:
      agent-out: ">=0.5.0"
      market-cli: ">=0.4.0"
"#;

    /// Build a working source root with one skill that exercises every
    /// helper (script / skill_ref / state_out / cli_ref).
    fn fixture_source_root(tmp: &TempDir) -> SourceRoot {
        let root = tmp.path();
        write(&root.join("manifests/skills.yaml"), SKILLS_FIXTURE);
        write(
            &root.join("manifests/plugins.yaml"),
            "schema_version: 1\nplugins: []\n",
        );
        write(
            &root.join("manifests/product-capabilities.yaml"),
            PRODUCT_CAPS,
        );
        write(&root.join("manifests/runtime-roots.yaml"), RUNTIME_ROOTS);
        write(&root.join("manifests/cli-tools.yaml"), CLI_TOOLS);

        // Skill template exercises every helper. Trailing newline is
        // intentional so the rendered file ends with one.
        write(
            &root.join("core/skills/market/favorites/SKILL.md.tera"),
            r#"# {{ skill_ref(id="market.favorites") }}

state: {{ state_out(domain="market", topic="favorites") }}
script: {{ script(path="core/scripts/market.sh") }}
required: {{ cli_ref(name="agent-out") }} via {{ cli_ref(name="market-cli") }}
"#,
        );

        SourceRoot::from_arg_or_cwd(Some(root)).unwrap()
    }

    const PRODUCT_CAPS: &str = r#"
schema_version: 1
products:
  codex:
    nested_skill_support: true
    plugin_manifest:
      path_pattern: "ignored"
      loaded_at_runtime: false
      schema_ref: "ignored"
    hooks_model:
      config_surface: "ignored"
      payload_shape: "ignored"
      supports_inline_python: false
    config_activation:
      - "$CODEX_HOME/AGENTS.md"
    runtime_state:
      state_home_env: "STATE"
      default_path: "/tmp/state"
  claude:
    nested_skill_support: true
    plugin_manifest:
      path_pattern: "ignored"
      loaded_at_runtime: true
      schema_ref: "ignored"
    hooks_model:
      config_surface: "ignored"
      payload_shape: "ignored"
      supports_inline_python: true
    config_activation:
      - "$HOME/.claude/settings.json"
    runtime_state:
      state_home_env: "STATE"
      default_path: "/tmp/state"
"#;

    const RUNTIME_ROOTS: &str = r#"
schema_version: 1
products:
  codex:
    live_home: "$CODEX_HOME"
    docs_home: "$CODEX_HOME"
    state_home: "/tmp/state"
    plugin_root: "$CODEX_HOME/plugins"
    hook_config_strategy: managed-block
    min_version: "<TBD: pin during Phase 1>"
    recommended_version: "<TBD: pin during Phase 1>"
    min_version_effective_from: "<TBD: pin during Phase 1>"
    version_probe: "codex --version"
  claude:
    live_home: "$HOME/.claude"
    docs_home: "$HOME/.claude"
    state_home: "/tmp/state"
    plugin_root_env: "CLAUDE_PLUGIN_ROOT"
    hook_config_strategy: settings-json
    min_version: "<TBD: pin during Phase 1>"
    recommended_version: "<TBD: pin during Phase 1>"
    min_version_effective_from: "<TBD: pin during Phase 1>"
    version_probe: "claude --version"
"#;

    const CLI_TOOLS: &str = r#"
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

    fn load_set(root: &SourceRoot) -> Arc<ManifestSet> {
        Arc::new(crate::render::manifest::load_all(root).unwrap())
    }

    #[test]
    fn write_product_renders_codex_skill_into_build_tree() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        let set = load_set(&root);

        let report = write_product(&root, set, "codex").unwrap();

        assert_eq!(report.rendered, vec!["market.favorites".to_string()]);
        assert!(report.cached.is_empty());
        assert!(report.skipped.is_empty());
        let out = report.output_root.join("skills/market/favorites/SKILL.md");
        let body = fs::read_to_string(&out).unwrap();
        assert!(body.contains("# /market-favorites"), "{body}");
        assert!(
            body.contains("state: agent-out path-for --domain market --topic favorites"),
            "{body}",
        );
        assert!(
            body.contains("script: ") && body.contains("/core/scripts/market.sh"),
            "{body}",
        );
        assert!(
            body.contains("required: agent-out (>=0.5.0) via market-cli (>=0.4.0)"),
            "{body}",
        );

        // Cache file exists after the run.
        let cache = RenderCache::load_or_empty(&report.output_root.join(CACHE_FILE));
        assert!(cache.skills.contains_key("market.favorites"));
    }

    #[test]
    fn cache_hit_skips_render_and_keeps_existing_output_bytes() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        let set = load_set(&root);

        // First run populates output + cache.
        let first = write_product(&root, set.clone(), "codex").unwrap();
        let snapshot_first = snapshot_outputs(&first.output_root);
        assert_eq!(first.rendered, vec!["market.favorites".to_string()]);

        // Second run with no input change must hit the cache and leave
        // the output bytes identical.
        let second = write_product(&root, set.clone(), "codex").unwrap();
        assert!(second.rendered.is_empty(), "{:?}", second.rendered);
        assert_eq!(second.cached, vec!["market.favorites".to_string()]);
        let snapshot_second = snapshot_outputs(&second.output_root);
        assert_eq!(snapshot_first, snapshot_second);
    }

    #[test]
    fn cache_miss_after_cache_file_deletion_reproduces_identical_bytes() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        let set = load_set(&root);

        let first = write_product(&root, set.clone(), "codex").unwrap();
        let snapshot_first = snapshot_outputs(&first.output_root);

        // Delete the cache file → forces a re-render. The rendered
        // bytes must match the first run byte-for-byte.
        fs::remove_file(first.output_root.join(CACHE_FILE)).unwrap();
        let second = write_product(&root, set, "codex").unwrap();
        assert_eq!(second.rendered, vec!["market.favorites".to_string()]);
        let snapshot_second = snapshot_outputs(&second.output_root);
        assert_eq!(snapshot_first, snapshot_second);
    }

    #[test]
    fn template_change_invalidates_cache_and_re_renders() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        let set = load_set(&root);
        write_product(&root, set.clone(), "codex").unwrap();

        // Touch the template body.
        let tpl_path = root
            .path()
            .join("core/skills/market/favorites/SKILL.md.tera");
        let mut body = fs::read_to_string(&tpl_path).unwrap();
        body.push_str("\nextra line\n");
        fs::write(&tpl_path, body).unwrap();

        // Reload manifests (skill source bytes changed; manifest bytes
        // unchanged → cache key still differs because template body is
        // part of the hash input).
        let set = load_set(&root);
        let second = write_product(&root, set, "codex").unwrap();
        assert_eq!(second.rendered, vec!["market.favorites".to_string()]);
        let rendered =
            fs::read_to_string(second.output_root.join("skills/market/favorites/SKILL.md"))
                .unwrap();
        assert!(rendered.ends_with("extra line\n"), "{rendered}");
    }

    #[test]
    fn skill_without_product_entry_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(
            &root.join("manifests/skills.yaml"),
            r#"
schema_version: 1
skills:
  - id: codex.only
    domain: codex
    source: core/skills/codex/only
    products:
      codex:
        render_to: skills/codex-only/SKILL.md
    required_clis: {}
"#,
        );
        write(
            &root.join("manifests/plugins.yaml"),
            "schema_version: 1\nplugins: []\n",
        );
        write(
            &root.join("manifests/product-capabilities.yaml"),
            PRODUCT_CAPS,
        );
        write(&root.join("manifests/runtime-roots.yaml"), RUNTIME_ROOTS);
        write(&root.join("manifests/cli-tools.yaml"), CLI_TOOLS);
        write(
            &root.join("core/skills/codex/only/SKILL.md.tera"),
            "# codex-only\n",
        );
        let source_root = SourceRoot::from_arg_or_cwd(Some(root)).unwrap();
        let set = load_set(&source_root);
        let report = write_product(&source_root, set, "claude").unwrap();
        assert!(report.rendered.is_empty());
        assert_eq!(report.skipped, vec!["codex.only".to_string()]);
    }

    #[test]
    fn render_rejects_unknown_product() {
        let tmp = TempDir::new().unwrap();
        let root = fixture_source_root(&tmp);
        let set = load_set(&root);
        let err = write_product(&root, set, "unknown").unwrap_err();
        assert!(format!("{err:#}").contains("unknown --product"));
    }

    #[test]
    fn sandboxed_join_rejects_parent_segments_and_absolute_paths() {
        let base = Path::new("/tmp/source-root");
        sandboxed_join(base, "core/scripts/foo.sh").unwrap();
        sandboxed_join(base, "./core/scripts/foo.sh").unwrap();
        let err = sandboxed_join(base, "../etc/passwd").unwrap_err();
        assert!(format!("{err}").contains(".."));
        let err = sandboxed_join(base, "/etc/passwd").unwrap_err();
        assert!(format!("{err}").contains("absolute"));
    }

    #[test]
    fn write_product_runs_against_empty_skills_manifest() {
        // Real agent-runtime-kit ships skills.yaml empty in Plan 01.
        // Render must not blow up — it should produce an empty cache.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(
            &root.join("manifests/skills.yaml"),
            "schema_version: 1\nskills: []\n",
        );
        write(
            &root.join("manifests/plugins.yaml"),
            "schema_version: 1\nplugins: []\n",
        );
        write(
            &root.join("manifests/product-capabilities.yaml"),
            PRODUCT_CAPS,
        );
        write(&root.join("manifests/runtime-roots.yaml"), RUNTIME_ROOTS);
        write(&root.join("manifests/cli-tools.yaml"), CLI_TOOLS);
        let source_root = SourceRoot::from_arg_or_cwd(Some(root)).unwrap();
        let set = load_set(&source_root);
        let report = write_product(&source_root, set, "codex").unwrap();
        assert!(report.rendered.is_empty());
        assert!(report.cached.is_empty());
        assert!(report.skipped.is_empty());
        assert!(report.output_root.exists());
    }
}
