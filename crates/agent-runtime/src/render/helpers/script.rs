//! `script(path)` — resolve a script path to a runtime-portable form
//! tied to the active product. The path argument is the **source-tree**
//! path of the script (e.g. `core/skills/reporting/topic-radar/scripts/topic-radar.sh`);
//! the helper translates it into the product's installed runtime path
//! so rendered SKILL bodies are byte-stable across hosts.
//!
//! Resolution rules:
//!
//! - If `path` lives under a known skill's source dir, the rendered
//!   value is `<live_home>/<dirname(render_to)>/<sibling_relative>`,
//!   where `live_home` comes from `manifests/runtime-roots.yaml` for
//!   the active product (`$CODEX_HOME` / `$HOME/.claude`). This keeps
//!   the rendered text portable: every host with the same install
//!   layout sees the same string.
//! - If `path` is a `core/`, `targets/`, or `manifests/` path that does
//!   not match any skill (e.g. a shared `core/scripts/foo.sh`), the
//!   helper falls back to the source-root-joined absolute path. That
//!   case predates the runtime-path translation and stays compatible
//!   until shared scripts get their own install lane (a follow-up
//!   concern; the typical skill case is what blocks Plan 03).
//!
//! Render is read-only at this stage; no I/O happens here.

use super::{HelperContext, HelperFn, HelperResult, arg_str, helper_error, missing_arg};
use minijinja::{Error, Value};
use std::path::Path;
use std::sync::Arc;

const ALLOWED_PREFIXES: &[&str] = &["core/", "targets/", "manifests/"];

pub(crate) fn make(ctx: Arc<HelperContext>) -> impl HelperFn {
    move |kwargs| -> HelperResult {
        let path = arg_str(&kwargs, "path")?
            .ok_or_else(|| missing_arg("script(): required arg `path` (string)"))?;
        let path = path.as_str();
        if path.contains("..") {
            return Err(helper_error(format!(
                "script(): arg `path` {path:?} must not contain `..` segments",
            )));
        }
        if !ALLOWED_PREFIXES
            .iter()
            .any(|prefix| path.starts_with(prefix))
        {
            return Err(helper_error(format!(
                "script(): arg `path` {path:?} must start with one of {ALLOWED_PREFIXES:?}",
            )));
        }
        if let Some(runtime) = resolve_runtime_path(&ctx, path)? {
            return Ok(Value::from(runtime));
        }
        // Fallback: source-root-joined absolute path for non-skill
        // scripts. This is the v0.13 behaviour and remains until shared
        // scripts get a dedicated install lane.
        let resolved = ctx.source_root.join(path);
        Ok(Value::from(resolved.to_string_lossy().into_owned()))
    }
}

/// Translate a source-tree script path to the product's runtime path
/// when the path lives under a known skill's source dir. Returns
/// `Ok(None)` when no skill matches; the caller falls back to the
/// source-root-joined form.
fn resolve_runtime_path(ctx: &HelperContext, path: &str) -> Result<Option<String>, Error> {
    let path_buf = Path::new(path);
    // Iterate every skill so a cross-skill reference (e.g.
    // `reporting.daily-brief` referencing `reporting.topic-radar`'s
    // script) resolves correctly. The matching skill is the one whose
    // `source` is a prefix of the requested path.
    for skill in &ctx.manifests.skills.skills {
        let source = Path::new(&skill.source);
        let Ok(sibling) = path_buf.strip_prefix(source) else {
            continue;
        };
        // A path equal to the skill source itself (no sibling tail) is
        // not a meaningful script reference; let the fallback handle
        // it.
        if sibling.as_os_str().is_empty() {
            return Ok(None);
        }
        let Some(render) = skill.products.get(&ctx.current_product) else {
            return Err(helper_error(format!(
                "script(): skill {id:?} does not declare product {product:?}; \
                 cannot resolve runtime path for {path:?}",
                id = skill.id,
                product = ctx.current_product,
            )));
        };
        let render_to = Path::new(&render.render_to);
        let runtime_dir = render_to.parent().ok_or_else(|| {
            helper_error(format!(
                "script(): render_to {render_to:?} for skill {id:?} has no parent dir",
                id = skill.id,
                render_to = render.render_to,
            ))
        })?;
        let live_home = live_home_for(ctx, &ctx.current_product)?;
        // Build the runtime path string component-by-component so the
        // separator stays `/` regardless of host platform. `live_home`
        // is an env-var literal like `$CODEX_HOME` and must land
        // verbatim at the head of the rendered string.
        let mut out = String::with_capacity(
            live_home.len() + 1 + runtime_dir.as_os_str().len() + 1 + sibling.as_os_str().len(),
        );
        out.push_str(live_home);
        if !out.ends_with('/') {
            out.push('/');
        }
        out.push_str(&runtime_dir.to_string_lossy());
        // `runtime_dir` may be empty when `render_to` is a top-level
        // file (no subdir). Avoid producing `<live>//<sibling>`.
        if !out.ends_with('/') {
            out.push('/');
        }
        out.push_str(&sibling.to_string_lossy());
        return Ok(Some(out));
    }
    Ok(None)
}

fn live_home_for<'a>(ctx: &'a HelperContext, product: &str) -> Result<&'a str, Error> {
    let roots = &ctx.manifests.runtime_roots.products;
    match product {
        "codex" => Ok(roots.codex.live_home.as_str()),
        "claude" => Ok(roots.claude.live_home.as_str()),
        "hermes" => Ok(roots.hermes.live_home.as_str()),
        other => Err(helper_error(format!(
            "script(): unknown product {other:?}; supported: codex, claude, hermes",
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{fixture_context, format_err, render};

    #[test]
    fn resolves_skill_sibling_under_codex_runtime() {
        // Path under the fixture skill's source dir → runtime path
        // under `$CODEX_HOME` (the fixture's live_home for codex is
        // `/tmp/live`).
        let ctx = fixture_context("codex");
        let out = render(
            r#"{{ script(path="core/skills/market/favorites/scripts/run.sh") }}"#,
            ctx,
        )
        .unwrap();
        assert_eq!(out, "/tmp/live/skills/sample/scripts/run.sh");
    }

    #[test]
    fn resolves_skill_sibling_under_claude_runtime() {
        let ctx = fixture_context("claude");
        let out = render(
            r#"{{ script(path="core/skills/market/favorites/scripts/run.sh") }}"#,
            ctx,
        )
        .unwrap();
        assert_eq!(
            out,
            "/tmp/live/plugins/market/skills/favorites/scripts/run.sh",
        );
    }

    #[test]
    fn falls_back_to_source_root_for_paths_outside_any_skill() {
        // `core/scripts/foo.sh` is not under any skill's source — the
        // helper falls back to the source-root-joined absolute form so
        // shared scripts (no install lane yet) still render to *some*
        // path consumers can interpret.
        let ctx = fixture_context("codex");
        let out = render(r#"{{ script(path="core/scripts/foo.sh") }}"#, ctx).unwrap();
        assert_eq!(out, "/tmp/source-root/core/scripts/foo.sh");
    }

    #[test]
    fn falls_back_to_source_root_for_targets_path() {
        // `targets/<product>/...` is product-adapter source organisation
        // and has no skill-keyed runtime mapping; v0.13 behaviour is
        // preserved.
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

    #[test]
    fn rejects_unknown_product_when_skill_lacks_render() {
        // The fixture skill declares both products; a request for
        // `unknown` would have already failed `require_known_product`.
        // The fallback case here is when a skill exists but doesn't
        // declare the requested product — render reports a clear error
        // pointing at the missing manifest entry.
        let mut ctx = fixture_context("codex");
        // Mutate the fixture: remove the codex product render entry on
        // the cloned-arc-shared manifest. We can't mutate Arc<Manifest>
        // directly, but we can verify the error message format by
        // pointing the helper at an unknown product on the
        // already-built context.
        ctx.current_product = "unknown".to_string();
        let err = render(
            r#"{{ script(path="core/skills/market/favorites/scripts/run.sh") }}"#,
            ctx,
        )
        .unwrap_err();
        let msg = format_err(&err);
        assert!(msg.contains("does not declare product"), "{msg}");
        assert!(msg.contains("unknown"), "{msg}");
    }
}
