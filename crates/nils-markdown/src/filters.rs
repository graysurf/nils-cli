use minijinja::value::ValueKind;
use minijinja::{Environment, Error, ErrorKind, Value};
use nils_common::markdown::canonicalize_table_cell;

/// minijinja filter wrapping
/// [`nils_common::markdown::canonicalize_table_cell`] so templates can
/// emit safe table cells with `{{ value | md_cell }}`. The filter
/// accepts strings (and stringifies numbers / bools) and escapes
/// pipes / collapses embedded newlines using the exact rule shared
/// with the rest of the workspace.
///
/// Auto-escape is disabled engine-wide, so no `safe`-marking is needed:
/// the rendered string is emitted verbatim.
fn md_cell(value: Value) -> Result<Value, Error> {
    let rendered = match value.kind() {
        ValueKind::String => canonicalize_table_cell(value.as_str().unwrap_or_default()),
        // minijinja represents JSON `null` as `None`; `Undefined`
        // (a missing key) also renders to an empty cell.
        ValueKind::None | ValueKind::Undefined => String::new(),
        ValueKind::Bool | ValueKind::Number => canonicalize_table_cell(&value.to_string()),
        _ => {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                format!("md_cell(): expected a stringifiable value, got {value:?}"),
            ));
        }
    };
    Ok(Value::from(rendered))
}

/// Install every workspace-default filter on `env`. Called from the
/// [`crate::Engine`] builder so consumers don't have to remember to do
/// it manually.
pub(crate) fn install_defaults(env: &mut Environment<'_>) {
    env.add_filter("md_cell", md_cell);
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
        let true_out = render("{{ value | md_cell }}", serde_json::json!({"value": true}));
        assert_eq!(true_out, "true");

        let num_out = render("{{ value | md_cell }}", serde_json::json!({"value": 42}));
        assert_eq!(num_out, "42");
    }

    #[test]
    fn md_cell_matches_canonicalize_table_cell() {
        let input = "first|second\r\nthird\ndone";
        let out = render("{{ value | md_cell }}", serde_json::json!({"value": input}));
        assert_eq!(out, canonicalize_table_cell(input));
    }
}
