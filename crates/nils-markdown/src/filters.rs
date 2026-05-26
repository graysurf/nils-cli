use std::collections::HashMap;

use nils_common::markdown::canonicalize_table_cell;
use tera::{Filter, Tera, Value};

/// Tera filter wrapping
/// [`nils_common::markdown::canonicalize_table_cell`] so templates can
/// emit safe table cells with `{{ value | md_cell }}`. The filter
/// accepts strings (and stringifies numbers / bools) and escapes
/// pipes / collapses embedded newlines using the exact rule shared
/// with the rest of the workspace.
struct MdCell;

impl Filter for MdCell {
    fn filter(&self, value: &Value, _args: &HashMap<String, Value>) -> tera::Result<Value> {
        let rendered = match value {
            Value::String(s) => canonicalize_table_cell(s),
            Value::Null => String::new(),
            Value::Bool(b) => canonicalize_table_cell(&b.to_string()),
            Value::Number(n) => canonicalize_table_cell(&n.to_string()),
            other => {
                return Err(tera::Error::msg(format!(
                    "md_cell(): expected a stringifiable value, got {other:?}"
                )));
            }
        };
        Ok(Value::String(rendered))
    }

    fn is_safe(&self) -> bool {
        true
    }
}

/// Install every workspace-default Tera filter on `tera`. Called from
/// the [`crate::Engine`] builder so consumers don't have to remember
/// to do it manually.
pub(crate) fn install_defaults(tera: &mut Tera) {
    tera.register_filter("md_cell", MdCell);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;

    fn render(template: &str, value: serde_json::Value) -> String {
        let mut e = Engine::builder().build();
        e.register_template("cell", template).unwrap();
        e.render_value("cell", &value).unwrap()
    }

    #[test]
    fn pipe_is_escaped() {
        let out = render("{{ value | md_cell }}", serde_json::json!({"value": "a|b"}));
        assert_eq!(out, "a/b");
    }

    #[test]
    fn newline_runs_collapse_to_single_space() {
        let out = render(
            "{{ value | md_cell }}",
            serde_json::json!({"value": "alpha\nbeta\r\ngamma"}),
        );
        assert_eq!(out, "alpha beta gamma");
    }

    #[test]
    fn empty_input_renders_empty() {
        let out = render("{{ value | md_cell }}", serde_json::json!({"value": ""}));
        assert_eq!(out, "");
    }

    #[test]
    fn null_input_renders_empty() {
        let out = render("{{ value | md_cell }}", serde_json::json!({"value": null}));
        assert_eq!(out, "");
    }

    #[test]
    fn bool_and_number_are_canonicalized() {
        let true_out = render(
            "{{ value | md_cell }}",
            serde_json::json!({"value": true}),
        );
        assert_eq!(true_out, "true");

        let num_out = render(
            "{{ value | md_cell }}",
            serde_json::json!({"value": 42}),
        );
        assert_eq!(num_out, "42");
    }

    #[test]
    fn md_cell_matches_canonicalize_table_cell() {
        let input = "first|second\r\nthird\ndone";
        let out = render(
            "{{ value | md_cell }}",
            serde_json::json!({"value": input}),
        );
        assert_eq!(out, canonicalize_table_cell(input));
    }
}
