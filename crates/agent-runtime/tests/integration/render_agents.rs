//! End-to-end coverage for the optional `agents` render surface. The
//! library tests under `src/render/` cover the writer / manifest / golden
//! modules in isolation; this file spawns the actual binary so the clap
//! layer, `lib::run()`, and the on-disk agent output stay verifiable
//! together. Mirrors `render.rs` but drives `manifests/agents.yaml` +
//! `core/agents/<id>/AGENT.md.tera`.

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn agent_runtime_bin() -> PathBuf {
    bin::resolve("agent-runtime")
}

fn run(args: &[&str]) -> CmdOutput {
    cmd::run(&agent_runtime_bin(), args, &[], None)
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

const VALID_PRODUCT_CAPABILITIES: &str = r#"
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

const VALID_RUNTIME_ROOTS: &str = r#"
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

const VALID_PLUGINS: &str = "schema_version: 1\nplugins: []\n";
const EMPTY_SKILLS: &str = "schema_version: 1\nskills: []\n";

const AGENTS_MANIFEST: &str = r#"
schema_version: 1
agents:
  - id: reviewer-quick
    source: core/agents/reviewer-quick
    products:
      codex:
        render_to: agents/reviewer-quick.toml
      claude:
        render_to: agents/reviewer-quick.md
"#;

// One canonical source; the `product` Tera variable selects the Codex
// TOML arm or the Claude Markdown arm.
const AGENT_TEMPLATE: &str = "{% if product == \"codex\" %}name = \"reviewer-quick\"\n\
     description = \"quick read-only reviewer\"\n\
     {% else %}---\nname: reviewer-quick\ndescription: quick read-only reviewer\n---\n{% endif %}";

fn fixture(tmp: &TempDir) -> PathBuf {
    let root = tmp.path().to_path_buf();
    write(&root.join("manifests/skills.yaml"), EMPTY_SKILLS);
    write(&root.join("manifests/plugins.yaml"), VALID_PLUGINS);
    write(
        &root.join("manifests/product-capabilities.yaml"),
        VALID_PRODUCT_CAPABILITIES,
    );
    write(
        &root.join("manifests/runtime-roots.yaml"),
        VALID_RUNTIME_ROOTS,
    );
    write(&root.join("manifests/cli-tools.yaml"), VALID_CLI_TOOLS);
    write(&root.join("manifests/agents.yaml"), AGENTS_MANIFEST);
    write(
        &root.join("core/agents/reviewer-quick/AGENT.md.tera"),
        AGENT_TEMPLATE,
    );
    root
}

#[test]
fn render_codex_writes_agent_toml_and_separate_agents_cache() {
    let tmp = TempDir::new().unwrap();
    let root = fixture(&tmp);
    let root_str = root.to_str().unwrap();

    let out = run(&["render", "--source-root", root_str, "--product", "codex"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let rendered = fs::read_to_string(root.join("build/codex/agents/reviewer-quick.toml"))
        .expect("rendered agent TOML missing");
    assert!(rendered.contains("name = \"reviewer-quick\""), "{rendered}");

    // The agents surface keeps its own cache file, distinct from the
    // skills `.render-cache.json`.
    let cache = fs::read_to_string(root.join("build/codex/.render-cache-agents.json")).unwrap();
    assert!(cache.contains("reviewer-quick"), "{cache}");
}

#[test]
fn render_claude_branches_one_source_to_markdown() {
    let tmp = TempDir::new().unwrap();
    let root = fixture(&tmp);
    let root_str = root.to_str().unwrap();

    let out = run(&["render", "--source-root", root_str, "--product", "claude"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let rendered = fs::read_to_string(root.join("build/claude/agents/reviewer-quick.md"))
        .expect("rendered agent Markdown missing");
    assert!(rendered.contains("---\nname: reviewer-quick"), "{rendered}");
    assert!(
        !rendered.contains("name = \"reviewer-quick\""),
        "{rendered}"
    );
}

#[test]
fn render_agents_is_byte_identical_across_processes() {
    let tmp = TempDir::new().unwrap();
    let root = fixture(&tmp);
    let root_str = root.to_str().unwrap();
    let target = root.join("build/codex/agents/reviewer-quick.toml");

    let first = run(&["render", "--source-root", root_str, "--product", "codex"]);
    assert_eq!(first.code, 0, "stderr: {}", first.stderr_text());
    let body_first = fs::read_to_string(&target).unwrap();

    // Second cold process: the cache-hit path must reproduce the
    // cache-miss bytes exactly.
    let second = run(&["render", "--source-root", root_str, "--product", "codex"]);
    assert_eq!(second.code, 0, "stderr: {}", second.stderr_text());
    let body_second = fs::read_to_string(&target).unwrap();
    assert_eq!(body_first, body_second);
}
