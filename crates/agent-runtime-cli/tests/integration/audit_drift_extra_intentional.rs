//! Integration coverage for Plan 04 Task 4.3 `audit-drift` intentional
//! product differences and extra live surfaces.

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn agent_runtime_bin() -> PathBuf {
    bin::resolve("agent-runtime")
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/render-determinism")
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_tree(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).unwrap();
        }
    }
}

fn copy_fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    copy_tree(&fixture_root(), tmp.path());
    tmp
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn run(args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    cmd::run(&agent_runtime_bin(), args, envs, None)
}

fn render_both_products(root: &Path) {
    for product in ["codex", "claude"] {
        let out = run(
            &[
                "render",
                "--source-root",
                root.to_str().unwrap(),
                "--product",
                product,
            ],
            &[],
        );
        assert_eq!(
            out.code,
            0,
            "render {product} exit={code} stderr={stderr}",
            code = out.code,
            stderr = out.stderr_text(),
        );
    }
}

fn audit(root: &Path, extra_args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    let mut args = vec!["audit-drift", "--source-root", root.to_str().unwrap()];
    args.extend(extra_args);
    run(&args, envs)
}

fn add_product_capability_diff(root: &Path) {
    let file = root.join("manifests/product-capabilities.yaml");
    let body = fs::read_to_string(&file).unwrap();
    write(
        &file,
        &format!(
            "{body}\nplugin_manifest_diff:\n  shared_fields:\n    - \"name\"\n  codex_only_fields:\n    - \"skills\"\n  claude_only_fields:\n    - \"homepage\"\n"
        ),
    );
}

fn add_plugin_manifests(root: &Path) {
    write(
        &root.join("targets/codex/plugins/reporting/.codex-plugin/plugin.json"),
        r#"{
  "name": "reporting",
  "skills": [
    {
      "id": "sample",
      "source": "core/skills/sample"
    }
  ]
}
"#,
    );
    write(
        &root.join("targets/claude/plugins/reporting/.claude-plugin/plugin.json"),
        r#"{
  "name": "reporting",
  "homepage": "https://github.com/graysurf/agent-runtime-kit"
}
"#,
    );
}

fn add_claude_link_map(root: &Path) {
    write(
        &root.join("targets/claude/link-map.yaml"),
        r#"schema_version: 1
entries:
  - id: reporting.plugin-manifest
    kind: plugin-manifest-copy
    source: targets/claude/plugins/reporting/.claude-plugin/plugin.json
    destination: plugins/reporting/.claude-plugin/plugin.json
  - id: reporting.skills-tree
    kind: symlinked-file
    source: build/claude/plugins/reporting/skills
    destination: plugins/reporting/skills
    recursive: true
"#,
    );
}

#[test]
fn documented_plugin_manifest_divergence_reports_intentional_difference_exit_zero() {
    let tmp = copy_fixture();
    add_product_capability_diff(tmp.path());
    add_plugin_manifests(tmp.path());

    let out = audit(tmp.path(), &[], &[]);
    assert_eq!(
        out.code,
        0,
        "intentional differences should not affect exit code; stderr=\n{}",
        out.stderr_text(),
    );
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("[intentional-difference/info/codex]"),
        "expected codex intentional difference; stderr=\n{stderr}",
    );
    assert!(
        stderr.contains("[intentional-difference/info/claude]"),
        "expected claude intentional difference; stderr=\n{stderr}",
    );
}

#[test]
fn untracked_file_under_live_home_install_root_reports_extra_exit_one() {
    let tmp = copy_fixture();
    add_plugin_manifests(tmp.path());
    add_claude_link_map(tmp.path());
    render_both_products(tmp.path());

    let home = tmp.path().join("home");
    let live_file = home.join(".claude/plugins/reporting/skills/unmanaged.txt");
    write(&live_file, "local unmanaged file\n");

    let home_str = home.to_str().unwrap();
    let out = audit(tmp.path(), &[], &[("HOME", home_str)]);
    assert_eq!(
        out.code,
        1,
        "extra live surface should warn; stderr=\n{}",
        out.stderr_text(),
    );
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("[extra/warn/claude]"),
        "expected extra/warn finding; stderr=\n{stderr}",
    );
    assert!(
        stderr.contains("plugins/reporting/skills/unmanaged.txt"),
        "expected extra finding to name unmanaged live file; stderr=\n{stderr}",
    );
}

#[test]
fn unrelated_live_home_sibling_outside_owned_link_map_roots_is_ignored() {
    let tmp = copy_fixture();
    add_plugin_manifests(tmp.path());
    add_claude_link_map(tmp.path());
    render_both_products(tmp.path());

    let home = tmp.path().join("home");
    write(
        &home.join(".claude/plugins/unrelated-plugin/notes.txt"),
        "operator-owned plugin file\n",
    );

    let home_str = home.to_str().unwrap();
    let out = audit(tmp.path(), &[], &[("HOME", home_str)]);
    assert_eq!(
        out.code,
        0,
        "unrelated plugin sibling should stay outside extra scan scope; stderr=\n{}",
        out.stderr_text(),
    );
    assert!(
        !out.stderr_text().contains("[extra/warn"),
        "unexpected extra finding for unrelated sibling; stderr=\n{}",
        out.stderr_text(),
    );
}
