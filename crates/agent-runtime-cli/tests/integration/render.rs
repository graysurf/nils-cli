//! End-to-end coverage for `agent-runtime render`. The library-level
//! tests under `src/render/` cover the writer / cache / helper modules
//! in isolation; this file spawns the actual binary so the clap layer,
//! the `lib::run()` exit-code mapping, and the on-disk output all stay
//! verifiable together.

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

fn fixture(tmp: &TempDir) -> PathBuf {
    let root = tmp.path().to_path_buf();
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
    write(
        &root.join("core/skills/market/favorites/SKILL.md.tera"),
        r#"# {{ skill_ref(id="market.favorites") }}
required: {{ cli_ref(name="agent-out") }}
state: {{ state_out(domain="market", topic="favorites") }}
"#,
    );
    root
}

#[test]
fn render_against_fixture_writes_expected_skill_md_and_cache() {
    let tmp = TempDir::new().unwrap();
    let root = fixture(&tmp);
    let root_str = root.to_str().unwrap();

    let out = run(&["render", "--source-root", root_str, "--product", "codex"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let rendered = fs::read_to_string(root.join("build/codex/skills/market/favorites/SKILL.md"))
        .expect("rendered SKILL.md missing");
    assert!(rendered.contains("# /market-favorites"), "{rendered}");
    assert!(
        rendered.contains("required: agent-out (>=0.5.0)"),
        "{rendered}",
    );
    assert!(
        rendered.contains("state: agent-out path-for --domain market --topic favorites"),
        "{rendered}",
    );

    let cache = fs::read_to_string(root.join("build/codex/.render-cache.json")).unwrap();
    assert!(cache.contains("market.favorites"), "{cache}");
}

#[test]
fn render_re_run_is_cache_hit_and_produces_identical_output() {
    let tmp = TempDir::new().unwrap();
    let root = fixture(&tmp);
    let root_str = root.to_str().unwrap();

    let first = run(&["render", "--source-root", root_str, "--product", "codex"]);
    assert_eq!(first.code, 0);
    let first_body =
        fs::read_to_string(root.join("build/codex/skills/market/favorites/SKILL.md")).unwrap();

    let second = run(&["render", "--source-root", root_str, "--product", "codex"]);
    assert_eq!(second.code, 0);
    assert!(
        second.stderr_text().contains("cached=1"),
        "expected cache hit on second run, got stderr: {}",
        second.stderr_text(),
    );
    let second_body =
        fs::read_to_string(root.join("build/codex/skills/market/favorites/SKILL.md")).unwrap();
    assert_eq!(first_body, second_body);
}

#[test]
fn render_against_missing_source_root_exits_two() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("no-manifests");
    fs::create_dir_all(&root).unwrap();
    let out = run(&[
        "render",
        "--source-root",
        root.to_str().unwrap(),
        "--product",
        "codex",
    ]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr_text());
    assert!(
        out.stderr_text().contains("missing manifest") || out.stderr_text().contains("manifests"),
        "{}",
        out.stderr_text(),
    );
}

#[test]
fn render_with_update_golden_copies_rendered_files_into_tests_golden() {
    let tmp = TempDir::new().unwrap();
    let root = fixture(&tmp);
    let root_str = root.to_str().unwrap();
    let out = run(&[
        "render",
        "--source-root",
        root_str,
        "--product",
        "codex",
        "--update-golden",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let golden = root.join("tests/golden/codex/skills/market/favorites/expected/SKILL.md");
    let body = fs::read_to_string(&golden).expect("golden SKILL.md missing");
    assert!(body.contains("# /market-favorites"), "{body}");
}
