//! `script(path)` — resolve a path under the source root, restricted to
//! the three sanctioned subtrees: `core/`, `targets/`, `manifests/`.
//!
//! The returned value is the source-root-joined path as a string. Render
//! is read-only at this stage; no I/O happens here.

use super::HelperContext;
use std::collections::HashMap;
use std::sync::Arc;
use tera::{Function, Value};

const ALLOWED_PREFIXES: &[&str] = &["core/", "targets/", "manifests/"];

pub fn make(ctx: Arc<HelperContext>) -> impl Function + 'static {
    move |args: &HashMap<String, Value>| -> tera::Result<Value> {
        let raw = args
            .get("path")
            .ok_or_else(|| tera::Error::msg("script(): required arg `path` (string)"))?;
        let path = raw.as_str().ok_or_else(|| {
            tera::Error::msg(format!(
                "script(): arg `path` must be a string, got {raw:?}"
            ))
        })?;
        if path.contains("..") {
            return Err(tera::Error::msg(format!(
                "script(): arg `path` {path:?} must not contain `..` segments",
            )));
        }
        if !ALLOWED_PREFIXES
            .iter()
            .any(|prefix| path.starts_with(prefix))
        {
            return Err(tera::Error::msg(format!(
                "script(): arg `path` {path:?} must start with one of {ALLOWED_PREFIXES:?}",
            )));
        }
        let resolved = ctx.source_root.join(path);
        Ok(Value::String(resolved.to_string_lossy().into_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{fixture_context, format_err, render};

    #[test]
    fn resolves_a_core_path_against_source_root() {
        let ctx = fixture_context("codex");
        let out = render(r#"{{ script(path="core/scripts/foo.sh") }}"#, ctx).unwrap();
        assert_eq!(out, "/tmp/source-root/core/scripts/foo.sh");
    }

    #[test]
    fn resolves_a_targets_path_against_source_root() {
        let ctx = fixture_context("claude");
        let out = render(r#"{{ script(path="targets/claude/policy.md") }}"#, ctx).unwrap();
        assert_eq!(out, "/tmp/source-root/targets/claude/policy.md");
    }

    #[test]
    fn rejects_path_outside_allowed_subtrees() {
        let ctx = fixture_context("codex");
        let err = render(r#"{{ script(path="/etc/passwd") }}"#, ctx).unwrap_err();
        let msg = format_err(&err);
        assert!(msg.contains("script()"), "{msg}");
        assert!(msg.contains("/etc/passwd"), "{msg}");
    }

    #[test]
    fn rejects_traversal_in_path() {
        let ctx = fixture_context("codex");
        let err = render(r#"{{ script(path="core/../etc/passwd") }}"#, ctx).unwrap_err();
        let msg = format_err(&err);
        assert!(msg.contains(".."), "{msg}");
    }

    #[test]
    fn rejects_missing_arg() {
        let ctx = fixture_context("codex");
        let err = render(r#"{{ script() }}"#, ctx).unwrap_err();
        let msg = format_err(&err);
        assert!(msg.contains("required arg `path`"), "{msg}");
    }
}
