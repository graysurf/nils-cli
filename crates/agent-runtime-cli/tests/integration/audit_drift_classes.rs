//! Integration tests for `agent-runtime audit-drift`. Each test:
//!
//! 1. Copies the `render-determinism` fixture into a tempdir (the
//!    determinism fixture is already a valid minimal source root).
//! 2. Optionally mutates the copy so one specific drift class fires.
//! 3. Runs `agent-runtime render` for both products to populate
//!    `build/<product>/`.
//! 4. Runs `agent-runtime audit-drift` and asserts the exit code +
//!    that the expected class is named in the finding lines.
//!
//! The five test arms map 1:1 onto Task 3.4's fixture matrix:
//!
//! | Fixture                     | Class                | Expected exit |
//! | --------------------------- | -------------------- | ------------- |
//! | clean                       | none                 | 0             |
//! | manifest-placeholder-pin    | source-manifest      | 1             |
//! | rendered-stale              | rendered-target      | 1             |
//! | agent-home-leak             | agent-home-leak      | 2             |
//! | docs-home-mismatch          | docs-home            | 2             |

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

fn run(args: &[&str]) -> CmdOutput {
    cmd::run(&agent_runtime_bin(), args, &[], None)
}

fn render_both_products(root: &Path) {
    for product in ["codex", "claude"] {
        let out = run(&[
            "render",
            "--source-root",
            root.to_str().unwrap(),
            "--product",
            product,
        ]);
        assert_eq!(
            out.code,
            0,
            "render {product} exit={code} stderr={stderr}",
            code = out.code,
            stderr = out.stderr_text(),
        );
    }
}

fn audit(root: &Path) -> CmdOutput {
    run(&["audit-drift", "--source-root", root.to_str().unwrap()])
}

#[test]
fn clean_fixture_exits_zero() {
    let tmp = copy_fixture();
    render_both_products(tmp.path());
    let out = audit(tmp.path());
    assert_eq!(
        out.code,
        0,
        "audit-drift on clean fixture should exit 0; stderr=\n{stderr}",
        stderr = out.stderr_text(),
    );
}

#[test]
fn manifest_placeholder_pin_exits_one() {
    let tmp = copy_fixture();
    render_both_products(tmp.path());
    // Inject `<TBD: pin during Phase 1>` into runtime-roots AFTER
    // render so the rendered tree still matches a fresh render
    // (manifest bytes affect the cache hash but not the rendered
    // output for our fixture).
    let manifest = tmp.path().join("manifests/runtime-roots.yaml");
    let body = fs::read_to_string(&manifest).unwrap();
    let mutated = body.replace(
        "min_version: \"0.1.0\"",
        "min_version: \"<TBD: pin during Phase 1>\"",
    );
    // The fixture currently uses placeholder text already — to be sure
    // we have an injection, write a body that definitely contains the
    // needle whether or not the original did.
    if mutated == body {
        fs::write(
            &manifest,
            format!("{body}\n# audit-drift: <TBD: pin during Phase 1>\n"),
        )
        .unwrap();
    } else {
        fs::write(&manifest, mutated).unwrap();
    }
    let out = audit(tmp.path());
    assert_eq!(
        out.code,
        1,
        "manifest-placeholder-pin should exit 1; stderr=\n{stderr}",
        stderr = out.stderr_text(),
    );
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("[source-manifest/warn]"),
        "expected source-manifest/warn in stderr; got\n{stderr}",
    );
}

#[test]
fn rendered_stale_exits_one() {
    let tmp = copy_fixture();
    render_both_products(tmp.path());
    // Mutate a rendered file so the live build diverges from a fresh
    // re-render. Append a trailing line — small enough not to look
    // like a leak class.
    let stale = tmp.path().join("build/codex/skills/sample/SKILL.md");
    let body = fs::read_to_string(&stale).unwrap();
    fs::write(&stale, format!("{body}stale edit\n")).unwrap();
    let out = audit(tmp.path());
    assert_eq!(
        out.code,
        1,
        "rendered-stale should exit 1; stderr=\n{stderr}",
        stderr = out.stderr_text(),
    );
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("[rendered-target/warn"),
        "expected rendered-target/warn in stderr; got\n{stderr}",
    );
}

#[test]
fn agent_home_leak_exits_two() {
    let tmp = copy_fixture();
    // Inject `$AGENT_HOME` into a *source* template so the leak shows
    // up in both `core/` and the rendered `build/<product>/` tree
    // after render. Doing it source-side rather than post-render
    // means the rendered tree matches a fresh re-render (so the
    // rendered-target diff class stays quiet — only the leak class
    // fires).
    let tera = tmp.path().join("core/skills/sample/SKILL.md.tera");
    let body = fs::read_to_string(&tera).unwrap();
    fs::write(&tera, format!("{body}\nleak: $AGENT_HOME\n")).unwrap();
    render_both_products(tmp.path());
    let out = audit(tmp.path());
    assert_eq!(
        out.code,
        2,
        "agent-home-leak should exit 2; stderr=\n{stderr}",
        stderr = out.stderr_text(),
    );
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("[agent-home-leak/block"),
        "expected agent-home-leak/block in stderr; got\n{stderr}",
    );
}

#[test]
fn docs_home_mismatch_exits_two() {
    let tmp = copy_fixture();
    // Inject the *wrong* docs-home into a codex-side template. After
    // render, build/codex/.../SKILL.md will have
    // `--docs-home "$HOME/.claude"` — docs_home class fires (block).
    let tera = tmp.path().join("core/skills/sample/SKILL.md.tera");
    let body = fs::read_to_string(&tera).unwrap();
    fs::write(
        &tera,
        format!("{body}\nrun --docs-home \"$HOME/.claude\" --foo\n"),
    )
    .unwrap();
    render_both_products(tmp.path());
    let out = audit(tmp.path());
    assert_eq!(
        out.code,
        2,
        "docs-home-mismatch should exit 2; stderr=\n{stderr}",
        stderr = out.stderr_text(),
    );
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("[docs-home/block"),
        "expected docs-home/block in stderr; got\n{stderr}",
    );
}
