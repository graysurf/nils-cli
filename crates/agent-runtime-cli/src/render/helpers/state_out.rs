//! `state_out(domain, topic=, repo=)` — emit either an `agent-out
//! path-for` invocation (mode `runtime`) or a literal resolved path
//! (mode `literal`), driven by the current skill's `state_out_mode`.
//!
//! Per Resolved Decision #9 the runtime form is the durable contract —
//! it preserves env-driven state-home overrides at execution time and
//! keeps render output stable across hosts. Literal mode is reserved
//! for skills that can't `exec` (a Plan 04 concern); we surface a clear
//! typed error rather than guessing a literal path here.

use super::HelperContext;
use crate::render::manifest::StateOutMode;
use std::collections::HashMap;
use std::sync::Arc;
use tera::{Function, Value};

pub fn make(ctx: Arc<HelperContext>) -> impl Function + 'static {
    move |args: &HashMap<String, Value>| -> tera::Result<Value> {
        let domain = validated_arg(args, "domain", /* required */ true)?
            .expect("required arg returned None unexpectedly");
        let topic = validated_arg(args, "topic", false)?;
        let repo = validated_arg(args, "repo", false)?;
        match ctx.current_skill_state_out_mode {
            StateOutMode::Runtime => {
                let mut cmd = format!("agent-out path-for --domain {domain}");
                if let Some(t) = topic {
                    cmd.push_str(&format!(" --topic {t}"));
                }
                if let Some(r) = repo {
                    cmd.push_str(&format!(" --repo {r}"));
                }
                Ok(Value::String(cmd))
            }
            StateOutMode::Literal => Err(tera::Error::msg(format!(
                "state_out(): literal mode not yet wired for skill {skill:?} \
                 (Plan 04 lands literal-mode resolution)",
                skill = ctx.current_skill_id
            ))),
        }
    }
}

/// Allowed character set for `state_out` args. The rendered output is a
/// command string that downstream agent runtimes feed to a shell, so the
/// helper rejects anything that could break out of the argv slot:
/// shell metacharacters (` ; | & $ \` < > * ? \n` etc.), shell quote
/// characters, and any whitespace. Identifiers in the source doc are
/// kebab-case slugs (`market`, `favorites`, `nils-cli`); this charset
/// is intentionally narrow.
fn arg_charset_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':'))
}

fn validated_arg(
    args: &HashMap<String, Value>,
    name: &'static str,
    required: bool,
) -> tera::Result<Option<String>> {
    let Some(raw) = args.get(name) else {
        if required {
            return Err(tera::Error::msg(format!(
                "state_out(): required arg `{name}` (string)"
            )));
        }
        return Ok(None);
    };
    let value = raw.as_str().ok_or_else(|| {
        tera::Error::msg(format!(
            "state_out(): arg `{name}` must be a string, got {raw:?}"
        ))
    })?;
    if !arg_charset_ok(value) {
        return Err(tera::Error::msg(format!(
            "state_out(): arg `{name}` {value:?} contains characters outside \
             the allowed set [A-Za-z0-9._:/-]; the rendered command is \
             shell-executed downstream and must stay quote-free",
        )));
    }
    Ok(Some(value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{fixture_context, format_err, render};
    use crate::render::manifest::StateOutMode;

    #[test]
    fn runtime_mode_emits_path_for_invocation() {
        let ctx = fixture_context("codex");
        let out = render(
            r#"{{ state_out(domain="market", topic="favorites") }}"#,
            ctx,
        )
        .unwrap();
        assert_eq!(out, "agent-out path-for --domain market --topic favorites");
    }

    #[test]
    fn runtime_mode_emits_invocation_without_topic_when_unset() {
        let ctx = fixture_context("codex");
        let out = render(r#"{{ state_out(domain="market") }}"#, ctx).unwrap();
        assert_eq!(out, "agent-out path-for --domain market");
    }

    #[test]
    fn runtime_mode_includes_repo_when_present() {
        let ctx = fixture_context("codex");
        let out = render(
            r#"{{ state_out(domain="market", topic="favorites", repo="nils-cli") }}"#,
            ctx,
        )
        .unwrap();
        assert_eq!(
            out,
            "agent-out path-for --domain market --topic favorites --repo nils-cli",
        );
    }

    #[test]
    fn literal_mode_returns_typed_error_until_plan_04() {
        let mut ctx = fixture_context("codex");
        ctx.current_skill_state_out_mode = StateOutMode::Literal;
        let err = render(r#"{{ state_out(domain="market") }}"#, ctx).unwrap_err();
        let msg = format_err(&err);
        assert!(msg.contains("literal mode"), "{msg}");
        assert!(msg.contains("market.favorites"), "{msg}");
    }

    #[test]
    fn rejects_missing_domain() {
        let ctx = fixture_context("codex");
        let err = render(r#"{{ state_out() }}"#, ctx).unwrap_err();
        let msg = format_err(&err);
        assert!(msg.contains("required arg `domain`"), "{msg}");
    }

    /// Shell-meta injection attempt — the rendered command string is
    /// shell-executed downstream, so a `domain=; rm -rf $HOME` literal
    /// would propagate. The charset gate rejects it before render.
    #[test]
    fn rejects_shell_metacharacters_in_args() {
        let ctx = fixture_context("codex");
        let err = render(r#"{{ state_out(domain="market; rm -rf $HOME") }}"#, ctx).unwrap_err();
        let msg = format_err(&err);
        assert!(msg.contains("characters outside the allowed set"), "{msg}");
        assert!(msg.contains("rm -rf"), "{msg}");
    }

    #[test]
    fn rejects_whitespace_in_args() {
        let ctx = fixture_context("codex");
        let err = render(
            r#"{{ state_out(domain="market", topic="favorites bar") }}"#,
            ctx,
        )
        .unwrap_err();
        assert!(format_err(&err).contains("favorites bar"));
    }

    #[test]
    fn rejects_dollar_sign_in_args() {
        let ctx = fixture_context("codex");
        let err = render(r#"{{ state_out(domain="market", topic="$HOME") }}"#, ctx).unwrap_err();
        let msg = format_err(&err);
        assert!(msg.contains("state_out()"), "{msg}");
        assert!(msg.contains("$HOME"), "{msg}");
    }

    #[test]
    fn rejects_empty_string_arg() {
        let ctx = fixture_context("codex");
        let err = render(r#"{{ state_out(domain="") }}"#, ctx).unwrap_err();
        assert!(format_err(&err).contains("allowed set"));
    }

    #[test]
    fn accepts_kebab_case_and_slashes() {
        // Real source-doc identifiers include kebab-case slugs (`nils-cli`),
        // dotted scopes (`market.favorites`), and a few slash-bearing repos.
        let ctx = fixture_context("codex");
        let out = render(
            r#"{{ state_out(domain="market", topic="market.favorites", repo="sympoies/nils-cli") }}"#,
            ctx,
        )
        .unwrap();
        assert_eq!(
            out,
            "agent-out path-for --domain market --topic market.favorites --repo sympoies/nils-cli",
        );
    }
}
