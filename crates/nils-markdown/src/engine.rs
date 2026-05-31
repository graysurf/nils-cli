use serde::Serialize;
use tera::{Context, Tera};

use crate::error::RenderError;

/// Tera-backed Markdown rendering engine for the nils-cli workspace.
///
/// Engine is constructed via [`Engine::builder`] so the determinism
/// posture (no auto-escape, no `now()`) is enforced in one place.
/// Templates are registered as raw `(name, body)` pairs, which lets
/// consumer crates ship `.md.tera` assets through `include_str!`
/// without filesystem lookups at runtime.
pub struct Engine {
    tera: Tera,
}

impl Engine {
    /// Returns an [`EngineBuilder`] configured for deterministic
    /// rendering. Always start engines from `Engine::builder()` —
    /// `Engine` has no public constructor besides this builder.
    pub fn builder() -> EngineBuilder {
        EngineBuilder::new()
    }

    /// Register a template body under `name`. The body is parsed
    /// eagerly so syntax errors surface at registration time rather
    /// than render time. The check for `now()` calls is the only
    /// content gate; everything else is delegated to Tera's parser.
    pub fn register_template(&mut self, name: &str, body: &str) -> Result<(), RenderError> {
        if contains_now_call(body) {
            return Err(RenderError::NonDeterministicTemplate { name: name.into() });
        }
        self.tera
            .add_raw_template(name, body)
            .map_err(|source| RenderError::TemplateParse {
                name: name.into(),
                source,
            })
    }

    /// Render a registered template against an opaque
    /// [`serde_json::Value`] view. This is the entry point the
    /// `md-render` binary will use in Sprint 3.
    pub fn render_value(
        &self,
        name: &str,
        view: &serde_json::Value,
    ) -> Result<String, RenderError> {
        let context = Context::from_value(view.clone()).map_err(|source| RenderError::Render {
            name: name.into(),
            source,
        })?;
        self.render_context(name, &context)
    }

    /// Render a registered template against a typed view struct.
    /// Consumers prepare a flat [`serde::Serialize`] view in Rust and
    /// hand it to this method; the engine performs the
    /// `serde_json::to_value` conversion and the Tera render in one
    /// step.
    pub fn render<T: Serialize>(&self, name: &str, view: &T) -> Result<String, RenderError> {
        let value =
            serde_json::to_value(view).map_err(|source| RenderError::Serialize { source })?;
        self.render_value(name, &value)
    }

    /// Render a literal template body without persistently
    /// registering it. The body is checked for `now()` calls and
    /// then rendered with the engine's registered helpers and the
    /// supplied view. This is the migration path for callers that
    /// today use `Tera::render_str` directly and treat every render
    /// as a fresh one-shot template.
    pub fn render_str<T: Serialize>(
        &mut self,
        body: &str,
        view: &T,
    ) -> Result<String, RenderError> {
        const INLINE_NAME: &str = "<inline>";
        if contains_now_call(body) {
            return Err(RenderError::NonDeterministicTemplate {
                name: INLINE_NAME.into(),
            });
        }
        let context = serialize_to_context(view).map_err(|source| RenderError::Render {
            name: INLINE_NAME.into(),
            source,
        })?;
        self.tera
            .render_str(body, &context)
            .map_err(|source| RenderError::Render {
                name: INLINE_NAME.into(),
                source,
            })
    }

    /// Attach a domain-specific Tera function under `name`. This is
    /// the consumer extension point for Task 1.4: nils-agent-runtime's
    /// `cli_ref / script / skill_ref / state_out` helpers register
    /// here without `nils-markdown` knowing the consumer's domain.
    pub fn register_helper<F>(&mut self, name: &str, function: F)
    where
        F: tera::Function + 'static,
    {
        self.tera.register_function(name, function);
    }

    fn render_context(&self, name: &str, context: &Context) -> Result<String, RenderError> {
        if !self.tera.get_template_names().any(|n| n == name) {
            return Err(RenderError::MissingTemplate { name: name.into() });
        }
        self.tera
            .render(name, context)
            .map_err(|source| RenderError::Render {
                name: name.into(),
                source,
            })
    }
}

/// Builder for [`Engine`]. Holds the deterministic-Tera defaults so
/// consumers cannot construct an engine with auto-escape or
/// `now()`-enabled templates by accident.
pub struct EngineBuilder {
    tera: Tera,
}

impl EngineBuilder {
    fn new() -> Self {
        let mut tera = Tera::default();
        tera.autoescape_on(vec![]);
        crate::filters::install_defaults(&mut tera);
        Self { tera }
    }

    pub fn build(self) -> Engine {
        Engine { tera: self.tera }
    }
}

impl Default for EngineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Serialize a view into a Tera [`Context`]. Tera requires the
/// top-level value to be a JSON object; we allow null / empty
/// callers (the nils-agent-runtime render path passes no view, the
/// helpers carry every variable) and map them to an empty context.
fn serialize_to_context<T: Serialize>(view: &T) -> Result<Context, tera::Error> {
    let value = serde_json::to_value(view).map_err(tera::Error::json)?;
    match value {
        serde_json::Value::Null => Ok(Context::new()),
        serde_json::Value::Object(_) => Context::from_value(value),
        other => Err(tera::Error::msg(format!(
            "render_str view must serialize to a JSON object or null, got {other:?}"
        ))),
    }
}

fn contains_now_call(body: &str) -> bool {
    let bytes = body.as_bytes();
    let needle = b"now";
    let mut i = 0usize;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            let prev_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let mut j = i + needle.len();
            while j < bytes.len() && matches!(bytes[j], b' ' | b'\t') {
                j += 1;
            }
            let next_ok = j < bytes.len() && bytes[j] == b'(';
            if prev_ok && next_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Greeting {
        name: String,
    }

    fn build() -> Engine {
        Engine::builder().build()
    }

    #[test]
    fn build_yields_engine_with_no_templates() {
        let engine = build();
        assert_eq!(engine.tera.get_template_names().count(), 0);
    }

    #[test]
    fn register_then_render_value_round_trips() {
        let mut engine = build();
        engine
            .register_template("hello", "Hello, {{ name }}!")
            .unwrap();
        let view = serde_json::json!({"name": "world"});
        let out = engine.render_value("hello", &view).unwrap();
        assert_eq!(out, "Hello, world!");
    }

    #[test]
    fn render_struct_round_trips() {
        let mut engine = build();
        engine
            .register_template("hello", "Hello, {{ name }}!")
            .unwrap();
        let view = Greeting {
            name: "tera".into(),
        };
        let out = engine.render("hello", &view).unwrap();
        assert_eq!(out, "Hello, tera!");
    }

    #[test]
    fn missing_template_is_reported_by_name() {
        let engine = build();
        let err = engine
            .render_value("absent", &serde_json::json!({}))
            .unwrap_err();
        match err {
            RenderError::MissingTemplate { name } => assert_eq!(name, "absent"),
            other => panic!("expected MissingTemplate, got {other:?}"),
        }
    }

    #[test]
    fn template_with_now_call_is_rejected() {
        let mut engine = build();
        let err = engine
            .register_template("bad", "stamp: {{ now() }}")
            .unwrap_err();
        match err {
            RenderError::NonDeterministicTemplate { name } => assert_eq!(name, "bad"),
            other => panic!("expected NonDeterministicTemplate, got {other:?}"),
        }
    }

    #[test]
    fn template_with_now_call_and_whitespace_is_rejected() {
        let mut engine = build();
        let err = engine
            .register_template("bad", "{{   now  ( ) }}")
            .unwrap_err();
        assert!(matches!(err, RenderError::NonDeterministicTemplate { .. }));
    }

    #[test]
    fn identifier_containing_now_substring_is_allowed() {
        let mut engine = build();
        engine
            .register_template("snowflake", "Hello, {{ snowflake }}!")
            .unwrap();
        let view = serde_json::json!({"snowflake": "ok"});
        let out = engine.render_value("snowflake", &view).unwrap();
        assert_eq!(out, "Hello, ok!");
    }

    #[test]
    fn template_parse_error_surfaces_name_and_source() {
        let mut engine = build();
        let err = engine.register_template("broken", "{% if %}").unwrap_err();
        match err {
            RenderError::TemplateParse { name, source } => {
                assert_eq!(name, "broken");
                let printed = format!("{source}");
                assert!(
                    !printed.is_empty(),
                    "tera error message should not be empty"
                );
            }
            other => panic!("expected TemplateParse, got {other:?}"),
        }
    }

    #[test]
    fn render_runtime_error_surfaces_name() {
        let mut engine = build();
        engine
            .register_template("strict", "{{ value | upper }}")
            .unwrap();
        let err = engine
            .render_value("strict", &serde_json::json!({"value": 42}))
            .unwrap_err();
        match err {
            RenderError::Render { name, .. } => assert_eq!(name, "strict"),
            other => panic!("expected Render, got {other:?}"),
        }
    }

    #[test]
    fn engine_does_not_auto_escape_html() {
        let mut engine = build();
        engine.register_template("md", "value: {{ raw }}").unwrap();
        let view = serde_json::json!({"raw": "<b>bold</b>"});
        let out = engine.render_value("md", &view).unwrap();
        assert_eq!(out, "value: <b>bold</b>");
    }

    #[test]
    fn render_str_uses_registered_helpers_and_view() {
        let mut engine = build();
        engine.register_helper(
            "shout",
            |args: &std::collections::HashMap<String, tera::Value>| -> tera::Result<tera::Value> {
                let v = args
                    .get("v")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| tera::Error::msg("shout(): required arg `v`"))?;
                Ok(tera::Value::String(v.to_uppercase()))
            },
        );
        let out = engine
            .render_str(
                r#"hi {{ name }} / {{ shout(v="ok") }}"#,
                &serde_json::json!({"name": "tera"}),
            )
            .unwrap();
        assert_eq!(out, "hi tera / OK");
    }

    #[test]
    fn render_str_rejects_now_call() {
        let mut engine = build();
        let err = engine
            .render_str("stamp: {{ now() }}", &serde_json::json!({}))
            .unwrap_err();
        assert!(matches!(err, RenderError::NonDeterministicTemplate { .. }));
    }

    #[test]
    fn register_helper_attaches_consumer_function() {
        use std::collections::HashMap;
        let mut engine = build();
        engine.register_helper(
            "shout",
            |args: &HashMap<String, tera::Value>| -> tera::Result<tera::Value> {
                let v = args
                    .get("v")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| tera::Error::msg("shout(): required arg `v`"))?;
                Ok(tera::Value::String(v.to_uppercase()))
            },
        );
        engine
            .register_template("greet", r#"hey, {{ shout(v="world") }}!"#)
            .unwrap();
        let out = engine
            .render_value("greet", &serde_json::json!({}))
            .unwrap();
        assert_eq!(out, "hey, WORLD!");
    }
}
