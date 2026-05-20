//! Cross-process determinism gate. Spawns the `agent-runtime` binary
//! twice against `tests/fixtures/render-determinism/`, deleting
//! `.render-cache.json` between runs so the second invocation is a
//! cache-miss rather than a cache-hit. A byte-level walk over both
//! `build/<product>/` trees must compare equal — if it ever diverges,
//! something inside the render pipeline started depending on
//! per-process state (HashMap iteration order, wall-clock time, env).
//!
//! Negative-control reminder: injecting a `SystemTime::now()`-derived
//! value into any helper or writer path makes this test diverge on
//! the second run. The crate-wide `clippy::disallowed_methods` gate
//! is the first line of defense; this test is the byte-level proof
//! that the gate's intent holds end-to-end.

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn agent_runtime_bin() -> PathBuf {
    bin::resolve("agent-runtime")
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/render-determinism")
}

/// Copy the read-only on-disk fixture into a tempdir so each test run
/// gets a private working tree (render writes `build/` next to the
/// manifests).
fn copy_fixture_to_tempdir() -> TempDir {
    let tmp = TempDir::new().unwrap();
    copy_tree(&fixture_root(), tmp.path());
    tmp
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

fn run(args: &[&str]) -> CmdOutput {
    cmd::run(&agent_runtime_bin(), args, &[], None)
}

fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            walk(base, &path, out);
            continue;
        }
        // The cache file changes between cache-miss and cache-hit runs
        // by design (the recorded hash and on-disk pretty-print may
        // shift across versions). The determinism contract covers the
        // rendered output tree, not the cache scratchpad.
        if path.file_name().and_then(|n| n.to_str()) == Some(".render-cache.json") {
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

fn assert_cross_process_byte_equal(product: &str) {
    let tmp = copy_fixture_to_tempdir();
    let root_str = tmp.path().to_str().unwrap();

    // First run — populates `build/<product>/` and `.render-cache.json`.
    let first = run(&["render", "--source-root", root_str, "--product", product]);
    assert_eq!(first.code, 0, "first render exit: {}", first.stderr_text());

    let build_dir = tmp.path().join("build").join(product);
    let cache_path = build_dir.join(".render-cache.json");
    assert!(
        cache_path.exists(),
        "cache file should exist after first run"
    );
    let snapshot_first = snapshot(&build_dir);

    // Force a cache-miss path on the second run by deleting the cache.
    fs::remove_file(&cache_path).unwrap();
    let second = run(&["render", "--source-root", root_str, "--product", product]);
    assert_eq!(
        second.code,
        0,
        "second render exit: {}",
        second.stderr_text()
    );
    let snapshot_second = snapshot(&build_dir);
    assert_eq!(
        snapshot_second, snapshot_first,
        "cross-process render for product={product} was not byte-identical",
    );
}

#[test]
fn cross_process_render_is_byte_identical_for_codex() {
    assert_cross_process_byte_equal("codex");
}

#[test]
fn cross_process_render_is_byte_identical_for_claude() {
    assert_cross_process_byte_equal("claude");
}
