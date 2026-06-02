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

const VALID_SURFACES: &str = r#"
schema_version: 1
render:
  target: support-matrix
  root_view: SUPPORT_MATRIX.md
  output: build/shared/SUPPORT_MATRIX.md
surfaces:
  - id: sample-surface
    ordinal: 1
    name: "sample surface"
    products:
      codex:
        state: shipped
        mechanism: "codex mechanism"
        source_artifacts:
          - AGENTS.md
        min_product: "0.130.0"
        min_nils_cli: "v0.20.0"
        acceptance:
          - kind: ci
            command: "cargo test"
            success:
              exit_status: 0
          - kind: live
            note: "manual session"
            descriptive_only: true
        source_manifest:
          - "manifests/product-capabilities.yaml:1"
      claude:
        state: not-applicable
        mechanism: "claude does not use this sample"
        source_artifacts: []
        min_product: "n/a"
        min_nils_cli: "n/a"
        acceptance:
          - kind: ci
            note: "—"
            descriptive_only: true
          - kind: live
            note: "—"
            descriptive_only: true
        source_manifest:
          - "docs/source/sample.md:1"
"#;

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
    write(&root.join("manifests/surfaces.yaml"), VALID_SURFACES);
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

#[test]
fn render_help_lists_support_matrix_target() {
    let out = run(&["render", "--help"]);
    assert_eq!(out.code, 0);
    let stdout = out.stdout_text();
    assert!(stdout.contains("--target"), "{stdout}");
    assert!(stdout.contains("support-matrix"), "{stdout}");
}

#[test]
fn render_support_matrix_writes_shared_output() {
    let tmp = TempDir::new().unwrap();
    let root = fixture(&tmp);
    let root_str = root.to_str().unwrap();

    let out = run(&[
        "render",
        "--source-root",
        root_str,
        "--target",
        "support-matrix",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let rendered = fs::read_to_string(root.join("build/shared/SUPPORT_MATRIX.md"))
        .expect("rendered support matrix missing");
    assert!(rendered.contains("Generated by `agent-runtime render --target support-matrix`"));
    assert!(rendered.contains("| 1. sample surface | codex | shipped | codex mechanism |"));
    assert!(rendered.contains("`cargo test` (exit 0)"));
    assert!(rendered.contains("| 1. sample surface | claude | not-applicable |"));
    assert!(out.stderr_text().contains("target=support-matrix"));
}

#[test]
fn render_support_matrix_rejects_invalid_acceptance() {
    let tmp = TempDir::new().unwrap();
    let root = fixture(&tmp);
    write(
        &root.join("manifests/surfaces.yaml"),
        r#"
schema_version: 1
render:
  target: support-matrix
  root_view: SUPPORT_MATRIX.md
  output: build/shared/SUPPORT_MATRIX.md
surfaces:
  - id: bad
    ordinal: 1
    name: bad
    products:
      codex:
        state: shipped
        mechanism: bad
        source_artifacts: []
        min_product: "0.130.0"
        min_nils_cli: "v0.20.0"
        acceptance:
          - kind: ci
            note: note
            command: "true"
            descriptive_only: true
        source_manifest: ["x:1"]
      claude:
        state: shipped
        mechanism: bad
        source_artifacts: []
        min_product: "2.1.145"
        min_nils_cli: "v0.20.0"
        acceptance:
          - kind: ci
            note: note
            descriptive_only: true
        source_manifest: ["x:1"]
"#,
    );
    let out = run(&[
        "render",
        "--source-root",
        root.to_str().unwrap(),
        "--target",
        "support-matrix",
    ]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr_text());
    assert!(
        out.stderr_text().contains("acceptance"),
        "{}",
        out.stderr_text()
    );
}

#[test]
fn render_support_matrix_covers_state_variants() {
    let tmp = TempDir::new().unwrap();
    let root = fixture(&tmp);
    write(
        &root.join("manifests/surfaces.yaml"),
        r#"
schema_version: 1
render:
  target: support-matrix
  root_view: SUPPORT_MATRIX.md
  output: build/shared/SUPPORT_MATRIX.md
surfaces:
  - id: variant-one
    ordinal: 1
    name: variant one
    products:
      codex:
        state: partial
        mechanism: partial mechanism
        source_artifacts: []
        min_product: "0.130.0"
        min_nils_cli: "v0.20.0"
        acceptance:
          - kind: ci
            note: note
            descriptive_only: true
        source_manifest: ["x:1"]
      claude:
        state: planned-not-shipped
        mechanism: planned mechanism
        source_artifacts: []
        min_product: "2.1.145"
        min_nils_cli: "v0.20.0"
        acceptance:
          - kind: ci
            note: note
            descriptive_only: true
        source_manifest: ["x:1"]
  - id: variant-two
    ordinal: 2
    name: variant two
    products:
      codex:
        state: not-shipped
        mechanism: not shipped mechanism
        source_artifacts: []
        min_product: "0.130.0"
        min_nils_cli: "v0.20.0"
        acceptance:
          - kind: ci
            note: note
            descriptive_only: true
        source_manifest: ["x:1"]
      claude:
        state: shipped
        mechanism: shipped mechanism
        source_artifacts: []
        min_product: "2.1.145"
        min_nils_cli: "v0.20.0"
        acceptance:
          - kind: ci
            note: note
            descriptive_only: true
        source_manifest: ["x:1"]
"#,
    );
    let out = run(&[
        "render",
        "--source-root",
        root.to_str().unwrap(),
        "--target",
        "support-matrix",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let rendered = fs::read_to_string(root.join("build/shared/SUPPORT_MATRIX.md"))
        .expect("rendered support matrix missing");
    assert!(rendered.contains("| 1. variant one | codex | partial |"));
    assert!(rendered.contains("| 1. variant one | claude | planned-not-shipped |"));
    assert!(rendered.contains("| 2. variant two | codex | not-shipped |"));
}

#[test]
fn render_support_matrix_reports_manifest_validation_errors() {
    let tmp = TempDir::new().unwrap();
    let root = fixture(&tmp);

    let cases = [
        (
            "schema_version: 2",
            "schema_version mismatch",
            r#"
schema_version: 2
render:
  target: support-matrix
  root_view: SUPPORT_MATRIX.md
  output: build/shared/SUPPORT_MATRIX.md
surfaces: []
"#,
        ),
        (
            "render target",
            "render.target must be",
            r#"
schema_version: 1
render:
  target: product
  root_view: SUPPORT_MATRIX.md
  output: build/shared/SUPPORT_MATRIX.md
surfaces: []
"#,
        ),
        (
            "empty root view",
            "render.root_view must not be empty",
            r#"
schema_version: 1
render:
  target: support-matrix
  root_view: ""
  output: build/shared/SUPPORT_MATRIX.md
surfaces: []
"#,
        ),
        (
            "empty surface id",
            "empty id",
            r#"
schema_version: 1
render:
  target: support-matrix
  root_view: SUPPORT_MATRIX.md
  output: build/shared/SUPPORT_MATRIX.md
surfaces:
  - id: ""
    ordinal: 1
    name: bad
    products:
      codex:
        state: shipped
        mechanism: ok
        source_artifacts: []
        min_product: "0.130.0"
        min_nils_cli: "v0.20.0"
        acceptance: []
        source_manifest: []
      claude:
        state: shipped
        mechanism: ok
        source_artifacts: []
        min_product: "2.1.145"
        min_nils_cli: "v0.20.0"
        acceptance: []
        source_manifest: []
"#,
        ),
        (
            "missing min product",
            "empty min_product",
            r#"
schema_version: 1
render:
  target: support-matrix
  root_view: SUPPORT_MATRIX.md
  output: build/shared/SUPPORT_MATRIX.md
surfaces:
  - id: bad
    ordinal: 1
    name: bad
    products:
      codex:
        state: shipped
        mechanism: ok
        source_artifacts: []
        min_product: ""
        min_nils_cli: "v0.20.0"
        acceptance: []
        source_manifest: []
      claude:
        state: shipped
        mechanism: ok
        source_artifacts: []
        min_product: "2.1.145"
        min_nils_cli: "v0.20.0"
        acceptance: []
        source_manifest: []
"#,
        ),
        (
            "command needs success",
            "command acceptance requires success",
            r#"
schema_version: 1
render:
  target: support-matrix
  root_view: SUPPORT_MATRIX.md
  output: build/shared/SUPPORT_MATRIX.md
surfaces:
  - id: bad
    ordinal: 1
    name: bad
    products:
      codex:
        state: shipped
        mechanism: ok
        source_artifacts: []
        min_product: "0.130.0"
        min_nils_cli: "v0.20.0"
        acceptance:
          - kind: ci
            command: "true"
        source_manifest: []
      claude:
        state: shipped
        mechanism: ok
        source_artifacts: []
        min_product: "2.1.145"
        min_nils_cli: "v0.20.0"
        acceptance: []
        source_manifest: []
"#,
        ),
        (
            "note must be descriptive",
            "note acceptance requires descriptive_only=true",
            r#"
schema_version: 1
render:
  target: support-matrix
  root_view: SUPPORT_MATRIX.md
  output: build/shared/SUPPORT_MATRIX.md
surfaces:
  - id: bad
    ordinal: 1
    name: bad
    products:
      codex:
        state: shipped
        mechanism: ok
        source_artifacts: []
        min_product: "0.130.0"
        min_nils_cli: "v0.20.0"
        acceptance:
          - kind: ci
            note: note
        source_manifest: []
      claude:
        state: shipped
        mechanism: ok
        source_artifacts: []
        min_product: "2.1.145"
        min_nils_cli: "v0.20.0"
        acceptance: []
        source_manifest: []
"#,
        ),
    ];

    for (name, expected, manifest) in cases {
        write(&root.join("manifests/surfaces.yaml"), manifest);
        let out = run(&[
            "render",
            "--source-root",
            root.to_str().unwrap(),
            "--target",
            "support-matrix",
        ]);
        assert_eq!(out.code, 2, "{name} stderr: {}", out.stderr_text());
        assert!(
            out.stderr_text().contains(expected),
            "{name} expected {expected:?}, got {}",
            out.stderr_text()
        );
    }
}

#[test]
fn render_support_matrix_update_golden_writes_shared_tree_only() {
    let tmp = TempDir::new().unwrap();
    let root = fixture(&tmp);
    let out = run(&[
        "render",
        "--source-root",
        root.to_str().unwrap(),
        "--target",
        "support-matrix",
        "--update-golden",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert!(root.join("tests/golden/shared/SUPPORT_MATRIX.md").exists());
    assert!(!root.join("tests/golden/codex").exists());
    assert!(!root.join("tests/golden/claude").exists());
}

fn write_common_manifests(root: &Path) {
    write(&root.join("manifests/plugins.yaml"), VALID_PLUGINS);
    write(&root.join("manifests/surfaces.yaml"), VALID_SURFACES);
    write(
        &root.join("manifests/product-capabilities.yaml"),
        VALID_PRODUCT_CAPABILITIES,
    );
    write(
        &root.join("manifests/runtime-roots.yaml"),
        VALID_RUNTIME_ROOTS,
    );
    write(&root.join("manifests/cli-tools.yaml"), VALID_CLI_TOOLS);
}

/// Retiring a skill (removing it from `manifests/skills.yaml`) must make
/// the next render reconcile `build/<product>/`: the retired skill's
/// outputs and its `.render-cache.json` entry are removed, and the
/// directory they emptied is pruned, while a sibling skill sharing the
/// parent dir survives. Without this, render is additive and the stale
/// build/ tree makes `prune-stale` treat the retired skill as still
/// expected (its recursive link-map entry expands over the stale tree),
/// silently keeping it in the live home.
#[test]
fn render_retiring_a_skill_reconciles_build_outputs_and_cache_entry() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let root_str = root.to_str().unwrap();
    write_common_manifests(&root);

    // Two codex skills sharing the `skills/market` parent dir; `retire`
    // also writes a sibling script to exercise multi-output cleanup.
    write(
        &root.join("manifests/skills.yaml"),
        r#"
schema_version: 1
skills:
  - id: market.keep
    domain: market
    source: core/skills/market/keep
    products:
      codex:
        name: /market-keep
        render_to: skills/market/keep/SKILL.md
    required_clis: {}
  - id: market.retire
    domain: market
    source: core/skills/market/retire
    products:
      codex:
        name: /market-retire
        render_to: skills/market/retire/SKILL.md
    required_clis: {}
"#,
    );
    write(
        &root.join("core/skills/market/keep/SKILL.md.tera"),
        "# keep\n",
    );
    write(
        &root.join("core/skills/market/retire/SKILL.md.tera"),
        "# retire\n",
    );
    write(
        &root.join("core/skills/market/retire/scripts/tool.sh"),
        "echo hi\n",
    );

    // First render: both skills land in build/ + cache.
    let first = run(&["render", "--source-root", root_str, "--product", "codex"]);
    assert_eq!(first.code, 0, "stderr: {}", first.stderr_text());
    assert!(
        root.join("build/codex/skills/market/keep/SKILL.md")
            .exists()
    );
    assert!(
        root.join("build/codex/skills/market/retire/SKILL.md")
            .exists()
    );
    assert!(
        root.join("build/codex/skills/market/retire/scripts/tool.sh")
            .exists()
    );
    let cache = fs::read_to_string(root.join("build/codex/.render-cache.json")).unwrap();
    assert!(cache.contains("market.retire"), "{cache}");

    // Retire `market.retire`: remove it from the manifest entirely.
    write(
        &root.join("manifests/skills.yaml"),
        r#"
schema_version: 1
skills:
  - id: market.keep
    domain: market
    source: core/skills/market/keep
    products:
      codex:
        name: /market-keep
        render_to: skills/market/keep/SKILL.md
    required_clis: {}
"#,
    );

    // Second render must reconcile build/: drop the retired skill's
    // outputs, prune the emptied directory, and drop the cache entry.
    let second = run(&["render", "--source-root", root_str, "--product", "codex"]);
    assert_eq!(second.code, 0, "stderr: {}", second.stderr_text());

    assert!(
        root.join("build/codex/skills/market/keep/SKILL.md")
            .exists(),
        "kept skill must survive reconcile",
    );
    assert!(
        !root.join("build/codex/skills/market/retire").exists(),
        "retired skill directory must be removed from build/",
    );
    let cache2 = fs::read_to_string(root.join("build/codex/.render-cache.json")).unwrap();
    assert!(
        !cache2.contains("market.retire"),
        "retired cache entry must be gone: {cache2}",
    );
    assert!(cache2.contains("market.keep"), "{cache2}");
}
