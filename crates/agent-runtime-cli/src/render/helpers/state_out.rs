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
        let domain = string_arg(args, "domain")?;
        let topic = optional_string_arg(args, "topic")?;
        let repo = optional_string_arg(args, "repo")?;
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

fn string_arg(args: &HashMap<String, Value>, name: &'static str) -> tera::Result<String> {
    let raw = args
        .get(name)
        .ok_or_else(|| tera::Error::msg(format!("state_out(): required arg `{name}` (string)")))?;
    raw.as_str().map(str::to_string).ok_or_else(|| {
        tera::Error::msg(format!(
            "state_out(): arg `{name}` must be a string, got {raw:?}"
        ))
    })
}

fn optional_string_arg(
    args: &HashMap<String, Value>,
    name: &'static str,
) -> tera::Result<Option<String>> {
    let Some(raw) = args.get(name) else {
        return Ok(None);
    };
    raw.as_str().map(|s| Some(s.to_string())).ok_or_else(|| {
        tera::Error::msg(format!(
            "state_out(): arg `{name}` must be a string, got {raw:?}"
        ))
    })
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
}
