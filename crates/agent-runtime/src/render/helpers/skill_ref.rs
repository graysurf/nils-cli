//! `skill_ref(id)` — resolve a tracked skill id (`<domain>.<skill>`) to
//! the product-native invocation name for the active product. Falls back
//! to the canonical id when the per-product entry omits `name`.

use super::{HelperContext, HelperFn, HelperResult, arg_str, helper_error, missing_arg};
use minijinja::Value;
use std::sync::Arc;

pub(crate) fn make(ctx: Arc<HelperContext>) -> impl HelperFn {
    move |kwargs| -> HelperResult {
        let id = arg_str(&kwargs, "id")?
            .ok_or_else(|| missing_arg("skill_ref(): required arg `id` (string)"))?;
        let skill = ctx
            .manifests
            .skills
            .skills
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| {
                helper_error(format!(
                    "skill_ref(): no skill with id {id:?} in skills.yaml"
                ))
            })?;
        let render = skill.products.get(&ctx.current_product).ok_or_else(|| {
            helper_error(format!(
                "skill_ref(): skill {id:?} has no entry for product {product:?}",
                product = ctx.current_product
            ))
        })?;
        let label = render.name.clone().unwrap_or_else(|| id.to_string());
        Ok(Value::from(label))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{fixture_context, format_err, render};

    #[test]
    fn returns_product_native_name_for_codex() {
        let ctx = fixture_context("codex");
        let out = render(r#"{{ skill_ref(id="market.favorites") }}"#, ctx).unwrap();
        assert_eq!(out, "/codex-name");
    }

    #[test]
    fn returns_product_native_name_for_claude() {
        let ctx = fixture_context("claude");
        let out = render(r#"{{ skill_ref(id="market.favorites") }}"#, ctx).unwrap();
        assert_eq!(out, "market:favorites");
    }

    #[test]
    fn rejects_unknown_skill_id() {
        let ctx = fixture_context("codex");
        let err = render(r#"{{ skill_ref(id="market.unknown") }}"#, ctx).unwrap_err();
        let msg = format_err(&err);
        assert!(msg.contains("market.unknown"), "{msg}");
        assert!(msg.contains("skill_ref()"), "{msg}");
    }

    #[test]
    fn rejects_skill_missing_for_current_product() {
        let mut ctx = fixture_context("codex");
        ctx.current_product = "unknown-product".to_string();
        let err = render(r#"{{ skill_ref(id="market.favorites") }}"#, ctx).unwrap_err();
        let msg = format_err(&err);
        assert!(msg.contains("unknown-product"), "{msg}");
    }

    #[test]
    fn rejects_missing_arg() {
        let ctx = fixture_context("codex");
        let err = render(r#"{{ skill_ref() }}"#, ctx).unwrap_err();
        let msg = format_err(&err);
        assert!(msg.contains("required arg `id`"), "{msg}");
    }
}
