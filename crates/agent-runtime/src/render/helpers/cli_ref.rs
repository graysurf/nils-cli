//! `cli_ref(name)` — resolve a CLI reference. The current skill's
//! `required_clis` map is the authoritative source for version floors;
//! when a binary appears there, we emit `name (floor)`. Falling back to
//! [`cli-tools.yaml`](super::HelperContext) covers third-party tools
//! without a per-skill floor. Unknown names surface a typed error so
//! drift audit's `cli_ref` rejection class fires loud and early.

use super::{HelperContext, HelperFn, HelperResult, arg_str, helper_error, missing_arg};
use minijinja::Value;
use std::sync::Arc;

pub(crate) fn make(ctx: Arc<HelperContext>) -> impl HelperFn {
    move |kwargs| -> HelperResult {
        let name = arg_str(&kwargs, "name")?
            .ok_or_else(|| missing_arg("cli_ref(): required arg `name` (string)"))?;
        if let Some(floor) = ctx.current_skill_required_clis.get(&name) {
            return Ok(Value::from(format!("{name} ({floor})")));
        }
        if ctx.manifests.cli_tools.formulas.contains_key(&name) {
            return Ok(Value::from(name));
        }
        Err(helper_error(format!(
            "cli_ref(): unknown binary {name:?} \
             (not declared in skill {skill:?} required_clis, \
             not present in cli-tools.yaml formulas)",
            skill = ctx.current_skill_id
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{fixture_context, format_err, render};

    #[test]
    fn renders_floor_for_required_cli() {
        let ctx = fixture_context("codex");
        let out = render(r#"{{ cli_ref(name="agent-out") }}"#, ctx).unwrap();
        assert_eq!(out, "agent-out (>=0.5.0)");
    }

    #[test]
    fn renders_third_party_tool_from_cli_tools_yaml() {
        let ctx = fixture_context("codex");
        let out = render(r#"{{ cli_ref(name="ripgrep") }}"#, ctx).unwrap();
        assert_eq!(out, "ripgrep");
    }

    #[test]
    fn rejects_unknown_binary() {
        let ctx = fixture_context("codex");
        let err = render(r#"{{ cli_ref(name="totally-not-real") }}"#, ctx).unwrap_err();
        let msg = format_err(&err);
        assert!(msg.contains("totally-not-real"), "{msg}");
        assert!(msg.contains("cli_ref()"), "{msg}");
        assert!(msg.contains("market.favorites"), "{msg}");
    }

    #[test]
    fn rejects_missing_arg() {
        let ctx = fixture_context("codex");
        let err = render(r#"{{ cli_ref() }}"#, ctx).unwrap_err();
        let msg = format_err(&err);
        assert!(msg.contains("required arg `name`"), "{msg}");
    }
}
