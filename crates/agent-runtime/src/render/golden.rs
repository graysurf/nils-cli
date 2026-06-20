//! `--update-golden` mode. After a normal render, copy every produced
//! file from `<source-root>/build/<product>/<skill-render-dir>/` into
//! `<source-root>/tests/golden/<product>/<skill-render-dir>/expected/`.
//!
//! Golden directory creation is best-effort: a missing
//! `tests/golden/<product>/` is created, never errored on, and we never
//! touch anything outside the active product's subtree.

use crate::render::manifest::ManifestSet;
use crate::render::writer::{HOME_PROMPT_FILE, HomePromptReport, RenderReport, sandboxed_join};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Copy every rendered file under `report.output_root` into
/// `<source-root>/tests/golden/<product>/<...>/expected/...`. Returns
/// the list of (source, destination) pairs that were actually copied.
pub fn update_golden(
    source_root: &Path,
    manifests: &ManifestSet,
    report: &RenderReport,
) -> Result<Vec<(PathBuf, PathBuf)>> {
    let golden_root = source_root
        .join("tests")
        .join("golden")
        .join(&report.product);
    fs::create_dir_all(&golden_root)
        .with_context(|| format!("create_dir_all {}", golden_root.display()))?;

    let mut copied = Vec::new();
    let touched = touched_skill_ids(report);
    for skill in &manifests.skills.skills {
        if !touched.contains(&skill.id) {
            continue;
        }
        let Some(render) = skill.products.get(&report.product) else {
            continue;
        };
        let render_to = Path::new(&render.render_to);
        let render_dir = render_to.parent().unwrap_or_else(|| Path::new(""));
        let src_dir = sandboxed_join(&report.output_root, &render_dir.to_string_lossy())?;
        let dst_dir = sandboxed_join(&golden_root, &format!("{}/expected", render_dir.display()))?;
        if !src_dir.exists() {
            continue;
        }
        fs::create_dir_all(&dst_dir)
            .with_context(|| format!("create_dir_all {}", dst_dir.display()))?;
        copy_tree(&src_dir, &dst_dir, &mut copied)?;
    }
    // Mirror the copy for the optional agents surface. Agent ids land in
    // the same merged `report.rendered`/`cached` set, so `touched` already
    // covers them; the render dir is the parent of `render_to` (e.g.
    // `agents/` for `agents/<id>.toml`).
    for agent in &manifests.agents.agents {
        if !touched.contains(&agent.id) {
            continue;
        }
        let Some(render) = agent.products.get(&report.product) else {
            continue;
        };
        let render_to = Path::new(&render.render_to);
        let render_dir = render_to.parent().unwrap_or_else(|| Path::new(""));
        let src_dir = sandboxed_join(&report.output_root, &render_dir.to_string_lossy())?;
        let dst_dir = sandboxed_join(&golden_root, &format!("{}/expected", render_dir.display()))?;
        if !src_dir.exists() {
            continue;
        }
        fs::create_dir_all(&dst_dir)
            .with_context(|| format!("create_dir_all {}", dst_dir.display()))?;
        copy_tree(&src_dir, &dst_dir, &mut copied)?;
    }
    let home_prompt = sandboxed_join(&report.output_root, HOME_PROMPT_FILE)?;
    if home_prompt.exists() {
        let dst = golden_root.join("AGENT_HOME.md");
        fs::copy(&home_prompt, &dst)
            .with_context(|| format!("copy {} -> {}", home_prompt.display(), dst.display()))?;
        copied.push((home_prompt, dst));
    }
    Ok(copied)
}

pub fn update_home_prompt(source_root: &Path, report: &HomePromptReport) -> Result<PathBuf> {
    let golden_root = source_root
        .join("tests")
        .join("golden")
        .join(&report.product);
    fs::create_dir_all(&golden_root)
        .with_context(|| format!("create_dir_all {}", golden_root.display()))?;
    let dst = golden_root.join(HOME_PROMPT_FILE);
    fs::copy(&report.output_path, &dst)
        .with_context(|| format!("copy {} -> {}", report.output_path.display(), dst.display()))?;
    Ok(dst)
}

fn touched_skill_ids(report: &RenderReport) -> std::collections::BTreeSet<String> {
    let mut ids = std::collections::BTreeSet::new();
    ids.extend(report.rendered.iter().cloned());
    ids.extend(report.cached.iter().cloned());
    ids
}

fn copy_tree(src: &Path, dst: &Path, copied: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(src)
        .with_context(|| format!("read_dir {}", src.display()))?
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let src_path = entry.path();
        let file_name = entry.file_name();
        let dst_path = dst.join(&file_name);
        if src_path.is_dir() {
            fs::create_dir_all(&dst_path)
                .with_context(|| format!("create_dir_all {}", dst_path.display()))?;
            copy_tree(&src_path, &dst_path, copied)?;
        } else {
            fs::copy(&src_path, &dst_path).with_context(|| {
                format!("copy {} -> {}", src_path.display(), dst_path.display())
            })?;
            copied.push((src_path, dst_path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::manifest::{self, SourceRoot};
    use crate::render::writer;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    fn fixture(tmp: &TempDir) -> SourceRoot {
        let root = tmp.path();
        write(
            &root.join("manifests/skills.yaml"),
            r#"
schema_version: 1
skills:
  - id: market.favorites
    domain: market
    source: core/skills/market/favorites
    products:
      codex:
        name: /market-favorites
        render_to: skills/market/favorites/SKILL.md
    required_clis:
      agent-out: ">=0.5.0"
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
            &root.join("core/skills/market/favorites/SKILL.md.tera"),
            r#"# {{ skill_ref(id="market.favorites") }}
required: {{ cli_ref(name="agent-out") }}
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

    #[test]
    fn flag_off_does_not_write_golden() {
        let tmp = TempDir::new().unwrap();
        let root = fixture(&tmp);
        let set = Arc::new(manifest::load_all(&root).unwrap());
        writer::write_product(&root, set, "codex").unwrap();
        // No golden update; ensure tests/golden/ never appeared.
        let golden = root.path().join("tests/golden");
        assert!(!golden.exists(), "{} should not exist", golden.display());
    }

    #[test]
    fn flag_on_creates_golden_tree_for_active_product() {
        let tmp = TempDir::new().unwrap();
        let root = fixture(&tmp);
        let set = Arc::new(manifest::load_all(&root).unwrap());
        let report = writer::write_product(&root, set.clone(), "codex").unwrap();
        let copied = update_golden(root.path(), &set, &report).unwrap();
        assert!(
            !copied.is_empty(),
            "golden update should copy at least one file"
        );

        let expected = root
            .path()
            .join("tests/golden/codex/skills/market/favorites/expected/SKILL.md");
        assert!(expected.exists(), "{} missing", expected.display());
        let body = fs::read_to_string(&expected).unwrap();
        assert!(body.contains("# /market-favorites"), "{body}");
    }

    #[test]
    fn flag_on_only_touches_active_product_subtree() {
        let tmp = TempDir::new().unwrap();
        let root = fixture(&tmp);
        let set = Arc::new(manifest::load_all(&root).unwrap());
        // Plant a sentinel file under tests/golden/claude/ to prove we
        // never write outside tests/golden/codex/.
        let sentinel_dir = root.path().join("tests/golden/claude/skills");
        fs::create_dir_all(&sentinel_dir).unwrap();
        let sentinel = sentinel_dir.join("DO_NOT_TOUCH.md");
        fs::write(&sentinel, "untouched\n").unwrap();
        let report = writer::write_product(&root, set.clone(), "codex").unwrap();
        update_golden(root.path(), &set, &report).unwrap();
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "untouched\n");
    }

    #[test]
    fn cached_skills_are_also_refreshed_into_golden() {
        let tmp = TempDir::new().unwrap();
        let root = fixture(&tmp);
        let set = Arc::new(manifest::load_all(&root).unwrap());
        // Prime cache.
        let first = writer::write_product(&root, set.clone(), "codex").unwrap();
        // Pre-create golden with a stale value.
        let expected = root
            .path()
            .join("tests/golden/codex/skills/market/favorites/expected/SKILL.md");
        fs::create_dir_all(expected.parent().unwrap()).unwrap();
        fs::write(&expected, "stale\n").unwrap();
        // Run a cached render then update golden — cached path must
        // still overwrite golden.
        let second = writer::write_product(&root, set.clone(), "codex").unwrap();
        assert!(
            !second.cached.is_empty(),
            "expected cache hit, got {second:?}"
        );
        let _ = first;
        update_golden(root.path(), &set, &second).unwrap();
        let body = fs::read_to_string(&expected).unwrap();
        assert!(body.contains("# /market-favorites"), "{body}");
    }

    #[test]
    fn update_golden_copies_agent_outputs() {
        let tmp = TempDir::new().unwrap();
        let root = fixture(&tmp);
        write(
            &root.path().join("manifests/agents.yaml"),
            "schema_version: 1\nagents:\n  - id: reviewer-quick\n    \
             source: core/agents/reviewer-quick\n    products:\n      codex:\n        \
             render_to: agents/reviewer-quick.toml\n",
        );
        write(
            &root.path().join("core/agents/reviewer-quick/AGENT.md.tera"),
            "name = \"reviewer-quick\"\n",
        );
        let set = Arc::new(manifest::load_all(&root).unwrap());
        let report = writer::write_product(&root, set.clone(), "codex").unwrap();
        let copied = update_golden(root.path(), &set, &report).unwrap();
        assert!(!copied.is_empty(), "golden update copied nothing");
        let expected = root
            .path()
            .join("tests/golden/codex/agents/expected/reviewer-quick.toml");
        assert!(expected.exists(), "{} missing", expected.display());
        let body = fs::read_to_string(&expected).unwrap();
        assert!(body.contains("name = \"reviewer-quick\""), "{body}");
    }
}
