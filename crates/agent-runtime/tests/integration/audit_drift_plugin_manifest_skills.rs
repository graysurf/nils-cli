//! Integration coverage for the `plugin-manifest-skills` drift class.
//!
//! The Codex per-domain plugin manifest
//! (`targets/codex/plugins/<domain>/.codex-plugin/plugin.json`) carries a
//! hand-maintained `skills` array. The `intentional-difference` class only
//! reports that the array is codex-only; nothing validates its *contents*
//! against `manifests/plugins.yaml` (`contained_skills`) /
//! `manifests/skills.yaml` (`source`). This class closes that gap and
//! blocks on any divergence (graysurf/agent-runtime-kit#225 / nils-cli
//! repro of the #220 rename that shipped a dangling skill `source`).

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

fn audit(root: &Path) -> CmdOutput {
    cmd::run(
        &agent_runtime_bin(),
        &["audit-drift", "--source-root", root.to_str().unwrap()],
        &[],
        None,
    )
}

/// Declare a `sample` plugin in `plugins.yaml`. `contained_skills` lists
/// the full `domain.skill` ids the Codex plugin.json is expected to mirror.
/// The fixture's `skills.yaml` ships `sample.determinism`
/// (`core/skills/sample`) and `sample.codex-only` (`core/skills/codex_only`),
/// both with on-disk source directories.
fn write_sample_plugin(root: &Path, contained: &[&str]) {
    let lines: String = contained
        .iter()
        .map(|id| format!("      - {id}\n"))
        .collect();
    write(
        &root.join("manifests/plugins.yaml"),
        &format!(
            "schema_version: 1\nplugins:\n  - id: sample\n    domain: sample\n    contained_skills:\n{lines}    product_manifests: {{}}\n"
        ),
    );
}

/// Write the Codex `sample` plugin.json `skills` array verbatim. Each
/// `(id, source)` pair becomes one `{ "id", "source" }` object.
fn write_codex_sample_plugin_json(root: &Path, skills: &[(&str, &str)]) {
    let entries: Vec<String> = skills
        .iter()
        .map(|(id, source)| {
            format!("    {{\n      \"id\": \"{id}\",\n      \"source\": \"{source}\"\n    }}")
        })
        .collect();
    write(
        &root.join("targets/codex/plugins/sample/.codex-plugin/plugin.json"),
        &format!(
            "{{\n  \"name\": \"sample\",\n  \"skills\": [\n{}\n  ]\n}}\n",
            entries.join(",\n")
        ),
    );
}

#[test]
fn consistent_codex_plugin_skills_pass() {
    let tmp = copy_fixture();
    write_sample_plugin(tmp.path(), &["sample.determinism"]);
    write_codex_sample_plugin_json(tmp.path(), &[("determinism", "core/skills/sample")]);

    let out = audit(tmp.path());
    let stderr = out.stderr_text();
    assert_eq!(
        out.code, 0,
        "consistent plugin.json skills should not drift; stderr=\n{stderr}",
    );
    assert!(
        !stderr.contains("[plugin-manifest-skills"),
        "expected no plugin-manifest-skills finding; stderr=\n{stderr}",
    );
}

#[test]
fn dangling_renamed_skill_entry_blocks() {
    // Models the #220 regression: a renamed/removed skill left a stale
    // plugin.json entry whose id is no longer in contained_skills, has no
    // skills.yaml entry, and points at a source dir that no longer exists.
    let tmp = copy_fixture();
    write_sample_plugin(tmp.path(), &["sample.determinism"]);
    write_codex_sample_plugin_json(
        tmp.path(),
        &[
            ("determinism", "core/skills/sample"),
            ("renamed-away", "core/skills/renamed_away"),
        ],
    );

    let out = audit(tmp.path());
    let stderr = out.stderr_text();
    assert_eq!(
        out.code, 2,
        "dangling plugin.json skill entry must block; stderr=\n{stderr}",
    );
    assert!(
        stderr.contains("[plugin-manifest-skills/block/codex]"),
        "expected blocking plugin-manifest-skills finding; stderr=\n{stderr}",
    );
    assert!(
        stderr.contains("renamed-away") || stderr.contains("sample.renamed-away"),
        "finding should name the stale skill; stderr=\n{stderr}",
    );
}

#[test]
fn contained_skill_missing_from_plugin_json_blocks() {
    // A skill added to contained_skills but never advertised in the Codex
    // plugin.json would be silently absent from the live Codex home.
    let tmp = copy_fixture();
    write_sample_plugin(tmp.path(), &["sample.determinism", "sample.codex-only"]);
    write_codex_sample_plugin_json(tmp.path(), &[("determinism", "core/skills/sample")]);

    let out = audit(tmp.path());
    let stderr = out.stderr_text();
    assert_eq!(
        out.code, 2,
        "contained_skill missing from plugin.json must block; stderr=\n{stderr}",
    );
    assert!(
        stderr.contains("[plugin-manifest-skills/block/codex]"),
        "expected blocking plugin-manifest-skills finding; stderr=\n{stderr}",
    );
    assert!(
        stderr.contains("sample.codex-only"),
        "finding should name the unadvertised skill; stderr=\n{stderr}",
    );
}

#[test]
fn plugin_json_source_mismatch_blocks() {
    // The id is in contained_skills and skills.yaml, but the plugin.json
    // `source` does not match the manifest source.
    let tmp = copy_fixture();
    write_sample_plugin(tmp.path(), &["sample.determinism"]);
    write_codex_sample_plugin_json(tmp.path(), &[("determinism", "core/skills/codex_only")]);

    let out = audit(tmp.path());
    let stderr = out.stderr_text();
    assert_eq!(
        out.code, 2,
        "plugin.json source mismatch must block; stderr=\n{stderr}",
    );
    assert!(
        stderr.contains("[plugin-manifest-skills/block/codex]"),
        "expected blocking plugin-manifest-skills finding; stderr=\n{stderr}",
    );
    assert!(
        stderr.contains("core/skills/sample"),
        "finding should name the expected manifest source; stderr=\n{stderr}",
    );
}
