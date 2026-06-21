use minijinja::value::Kwargs;
use minijinja::{AutoEscape, Environment, UndefinedBehavior, Value};
use serde::Serialize;

use crate::error::RenderError;

/// minijinja-backed Markdown rendering engine for the nils-cli
/// workspace.
///
/// Engine is constructed via [`Engine::builder`] so the determinism
/// posture (no auto-escape, no `now()`) is enforced in one place.
/// Templates are registered as raw `(name, body)` pairs, which lets
/// consumer crates ship `.md.tera` assets through `include_str!`
/// without filesystem lookups at runtime.
pub struct Engine {
    env: Environment<'static>,
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
    /// content gate; everything else is delegated to minijinja's
    /// parser.
    pub fn register_template(&mut self, name: &str, body: &str) -> Result<(), RenderError> {
        if contains_now_call(body) {
            return Err(RenderError::NonDeterministicTemplate { name: name.into() });
        }
        self.env
            .add_template_owned(name.to_string(), body.to_string())
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
        let template = self
            .env
            .get_template(name)
            .map_err(|_| RenderError::MissingTemplate { name: name.into() })?;
        template
            .render(Value::from_serialize(view))
            .map_err(|source| RenderError::Render {
                name: name.into(),
                source,
            })
    }

    /// Render a registered template against a typed view struct.
    /// Consumers prepare a flat [`serde::Serialize`] view in Rust and
    /// hand it to this method; the engine performs the
    /// `serde_json::to_value` conversion and the render in one step.
    pub fn render<T: Serialize>(&self, name: &str, view: &T) -> Result<String, RenderError> {
        let value =
            serde_json::to_value(view).map_err(|source| RenderError::Serialize { source })?;
        self.render_value(name, &value)
    }

    /// Render a literal template body without persistently
    /// registering it. The body is checked for `now()` calls and
    /// then rendered with the engine's registered helpers and the
    /// supplied view. This is the migration path for callers that
    /// today use one-shot template rendering and treat every render
    /// as a fresh template.
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
        self.env
            .render_str(body, context)
            .map_err(|source| RenderError::Render {
                name: INLINE_NAME.into(),
                source,
            })
    }

    /// Attach a domain-specific function under `name`. This is the
    /// consumer extension point: nils-agent-runtime's
    /// `cli_ref / script / skill_ref / state_out` helpers register
    /// here without `nils-markdown` knowing the consumer's domain.
    ///
    /// The function receives keyword arguments as a
    /// [`minijinja::value::Kwargs`] and returns a
    /// [`minijinja::Value`]. Templates invoke it with named args, e.g.
    /// `{{ shout(v="ok") }}`.
    pub fn register_helper<F>(&mut self, name: &str, function: F)
    where
        F: Fn(Kwargs) -> Result<Value, minijinja::Error> + Send + Sync + 'static,
    {
        self.env.add_function(name.to_string(), function);
    }
}

/// Builder for [`Engine`]. Holds the deterministic defaults so
/// consumers cannot construct an engine with auto-escape or
/// `now()`-enabled templates by accident.
pub struct EngineBuilder {
    env: Environment<'static>,
}

impl EngineBuilder {
    fn new() -> Self {
        let mut env = Environment::new();
        // Disable auto-escape entirely: rendered Markdown must pass
        // raw HTML (`<b>bold</b>`) and table syntax through verbatim.
        env.set_auto_escape_callback(|_| AutoEscape::None);
        // Preserve the template body's trailing newline. minijinja
        // strips one trailing `\n` by default; tera (and our golden
        // fixtures) keep it, so opt back in for byte-identical output.
        env.set_keep_trailing_newline(true);
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        crate::filters::install_defaults(&mut env);
        Self { env }
    }

    pub fn build(self) -> Engine {
        Engine { env: self.env }
    }
}

impl Default for EngineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Serialize a view into a minijinja render context. The top-level
/// value must be a JSON object; we allow null / empty callers (the
/// nils-agent-runtime render path passes no view, the helpers carry
/// every variable) and map them to an empty context.
fn serialize_to_context<T: Serialize>(view: &T) -> Result<Value, minijinja::Error> {
    let value = serde_json::to_value(view).map_err(|e| {
        minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, e.to_string())
    })?;
    match value {
        serde_json::Value::Null => Ok(Value::from_serialize(serde_json::Map::<
            String,
            serde_json::Value,
        >::new())),
        serde_json::Value::Object(_) => Ok(Value::from_serialize(&value)),
        other => Err(minijinja::Error::new(
            minijinja::ErrorKind::InvalidOperation,
            format!("render_str view must serialize to a JSON object or null, got {other:?}"),
        )),
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
        assert_eq!(engine.env.templates().count(), 0);
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
                    "template error message should not be empty"
                );
            }
            other => panic!("expected TemplateParse, got {other:?}"),
        }
    }

    #[test]
    fn render_runtime_error_surfaces_name() {
        // minijinja's `upper` coerces a number to its string form
        // (`42` -> `"42"`) instead of erroring, unlike tera, so the
        // original `{{ value | upper }}`-on-a-number case no longer
        // exercises a render error. The `md_cell` filter still rejects
        // non-stringifiable values (arrays / maps), so we drive the
        // same `RenderError::Render` path through that filter instead —
        // the assertion (render fails and is reported by template name)
        // is unchanged.
        let mut engine = build();
        engine
            .register_template("strict", "{{ value | md_cell }}")
            .unwrap();
        let err = engine
            .render_value("strict", &serde_json::json!({"value": [1, 2]}))
            .unwrap_err();
        match err {
            RenderError::Render { name, .. } => assert_eq!(name, "strict"),
            other => panic!("expected Render, got {other:?}"),
        }
    }

    #[test]
    fn missing_top_level_field_errors() {
        let mut engine = build();
        engine
            .register_template("strict", "Hello, {{ name }}!")
            .unwrap();
        let err = engine
            .render_value("strict", &serde_json::json!({}))
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
            |kwargs: Kwargs| -> Result<Value, minijinja::Error> {
                let v: String = kwargs.get("v").map_err(|_| {
                    minijinja::Error::new(
                        minijinja::ErrorKind::MissingArgument,
                        "shout(): required arg `v`",
                    )
                })?;
                Ok(Value::from(v.to_uppercase()))
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
        let mut engine = build();
        engine.register_helper(
            "shout",
            |kwargs: Kwargs| -> Result<Value, minijinja::Error> {
                let v: String = kwargs.get("v").map_err(|_| {
                    minijinja::Error::new(
                        minijinja::ErrorKind::MissingArgument,
                        "shout(): required arg `v`",
                    )
                })?;
                Ok(Value::from(v.to_uppercase()))
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
