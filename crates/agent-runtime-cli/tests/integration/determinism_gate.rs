//! Negative-control for the determinism gate.
//!
//! The cross-process determinism test in `render_determinism.rs`
//! proves that *current* render output is byte-stable, but it cannot
//! tell whether the clippy gate that protects future contributors
//! is actually in place. If `#![deny(...)]` were silently removed
//! from a `lib.rs` or the `disallowed-{types,methods}` list were
//! emptied, the byte-equal test would still pass — and the next
//! time someone introduced `HashMap` on the render path, the
//! contract would break with no warning.
//!
//! This file asserts the gate's *presence*: the required clippy.toml
//! entries exist, the `#![deny(...)]` attribute is in place on both
//! crate libs, and the scoped `#![allow(...)]` exemption is exactly
//! where `docs/determinism.md` claims it lives.

const AGENT_RUNTIME_CLIPPY_TOML: &str = include_str!("../../clippy.toml");
const NILS_COMMON_CLIPPY_TOML: &str = include_str!("../../../nils-common/clippy.toml");
const AGENT_RUNTIME_LIB_RS: &str = include_str!("../../src/lib.rs");
const NILS_COMMON_LIB_RS: &str = include_str!("../../../nils-common/src/lib.rs");
const HELPERS_MOD_RS: &str = include_str!("../../src/render/helpers/mod.rs");

const REQUIRED_DISALLOWED_TYPES: &[&str] = &[
    "std::collections::HashMap",
    "std::collections::HashSet",
    "std::collections::hash_map::DefaultHasher",
    "std::collections::hash_map::RandomState",
];

const REQUIRED_DISALLOWED_METHODS: &[&str] = &[
    "std::time::SystemTime::now",
    "std::time::Instant::now",
    "chrono::Utc::now",
    "chrono::Local::now",
];

fn assert_contains(haystack: &str, needle: &str, where_: &str) {
    assert!(
        haystack.contains(needle),
        "{where_} is missing required token {needle:?}; \
         determinism gate has drifted from docs/determinism.md"
    );
}

#[test]
fn agent_runtime_clippy_toml_lists_every_required_entry() {
    for ty in REQUIRED_DISALLOWED_TYPES {
        assert_contains(
            AGENT_RUNTIME_CLIPPY_TOML,
            ty,
            "crates/agent-runtime-cli/clippy.toml",
        );
    }
    for m in REQUIRED_DISALLOWED_METHODS {
        assert_contains(
            AGENT_RUNTIME_CLIPPY_TOML,
            m,
            "crates/agent-runtime-cli/clippy.toml",
        );
    }
}

#[test]
fn nils_common_clippy_toml_lists_every_required_entry() {
    for ty in REQUIRED_DISALLOWED_TYPES {
        assert_contains(
            NILS_COMMON_CLIPPY_TOML,
            ty,
            "crates/nils-common/clippy.toml",
        );
    }
    for m in REQUIRED_DISALLOWED_METHODS {
        assert_contains(NILS_COMMON_CLIPPY_TOML, m, "crates/nils-common/clippy.toml");
    }
}

#[test]
fn agent_runtime_lib_denies_the_determinism_lints() {
    assert_contains(
        AGENT_RUNTIME_LIB_RS,
        "#![deny(clippy::disallowed_types, clippy::disallowed_methods)]",
        "crates/agent-runtime-cli/src/lib.rs",
    );
}

#[test]
fn nils_common_lib_denies_the_determinism_lints() {
    assert_contains(
        NILS_COMMON_LIB_RS,
        "#![deny(clippy::disallowed_types, clippy::disallowed_methods)]",
        "crates/nils-common/src/lib.rs",
    );
}

#[test]
fn helpers_mod_carries_the_only_sanctioned_disallowed_types_allow() {
    assert_contains(
        HELPERS_MOD_RS,
        "#![allow(clippy::disallowed_types)]",
        "crates/agent-runtime-cli/src/render/helpers/mod.rs",
    );
}

/// `helpers/mod.rs` is the only file under `src/render/` allowed to
/// silence `disallowed_types`. Walk the render subtree and confirm no
/// other file carries the inner attribute.
///
/// `disallowed_methods` may be silenced anywhere off the render path
/// (and the explicit exemption in `nils-common::fs::temp_path` does
/// so), but on the render path itself the only sanctioned escape
/// hatch is `render::time::source_commit_timestamp`.
#[test]
fn render_subtree_has_no_unsanctioned_disallowed_types_allow() {
    use std::fs;
    use std::path::PathBuf;

    let render_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/render");
    let helpers_mod = render_dir.join("helpers/mod.rs");

    let mut offenders: Vec<String> = Vec::new();
    walk_rust_files(&render_dir, &mut |path| {
        if path == helpers_mod {
            return;
        }
        let body = fs::read_to_string(path).unwrap();
        if body.contains("#![allow(clippy::disallowed_types")
            || body.contains("#[allow(clippy::disallowed_types")
        {
            offenders.push(path.display().to_string());
        }
    });

    assert!(
        offenders.is_empty(),
        "only `render/helpers/mod.rs` may silence `clippy::disallowed_types`; \
         these files also carry the lint allow and would let a future \
         HashMap import slip through the determinism gate: {offenders:#?}"
    );
}

fn walk_rust_files(dir: &std::path::Path, f: &mut dyn FnMut(&std::path::Path)) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            walk_rust_files(&path, f);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            f(&path);
        }
    }
}
