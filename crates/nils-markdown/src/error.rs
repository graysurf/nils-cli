use thiserror::Error;

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("template parse failed for `{name}`: {source}")]
    TemplateParse {
        name: String,
        #[source]
        source: tera::Error,
    },

    #[error("template not registered: `{name}`")]
    MissingTemplate { name: String },

    #[error(
        "template `{name}` references the non-deterministic `now()` function; \
         nils-markdown rejects templates that depend on wall-clock time"
    )]
    NonDeterministicTemplate { name: String },

    #[error("view serialization failed: {source}")]
    Serialize {
        #[source]
        source: serde_json::Error,
    },

    #[error("tera render failed for `{name}`: {source}")]
    Render {
        name: String,
        #[source]
        source: tera::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync_static<T: Send + Sync + 'static>() {}

    #[test]
    fn render_error_is_send_sync_static() {
        assert_send_sync_static::<RenderError>();
    }

    #[test]
    fn render_error_round_trips_through_anyhow() {
        let err = RenderError::MissingTemplate {
            name: "issue_body".into(),
        };
        let any: anyhow::Error = err.into();
        let printed = format!("{any}");
        assert!(printed.contains("issue_body"), "{printed}");
    }

    #[test]
    fn template_parse_error_renders_name_and_source() {
        let tera_err = tera::Error::msg("syntax error at line 3");
        let err = RenderError::TemplateParse {
            name: "dashboard".into(),
            source: tera_err,
        };
        let printed = format!("{err}");
        assert!(printed.contains("dashboard"), "{printed}");
        assert!(printed.contains("syntax error"), "{printed}");
    }

    #[test]
    fn missing_template_renders_name() {
        let err = RenderError::MissingTemplate {
            name: "absent".into(),
        };
        assert_eq!(format!("{err}"), "template not registered: `absent`");
    }

    #[test]
    fn non_deterministic_template_renders_name() {
        let err = RenderError::NonDeterministicTemplate {
            name: "time_based".into(),
        };
        let printed = format!("{err}");
        assert!(printed.contains("time_based"), "{printed}");
        assert!(printed.contains("non-deterministic"), "{printed}");
    }

    #[test]
    fn serialize_error_renders_source() {
        let source =
            serde_json::from_str::<serde_json::Value>("{ not json").expect_err("invalid json");
        let err = RenderError::Serialize { source };
        let printed = format!("{err}");
        assert!(
            printed.starts_with("view serialization failed"),
            "{printed}"
        );
    }

    #[test]
    fn render_runtime_error_renders_name() {
        let err = RenderError::Render {
            name: "lifecycle".into(),
            source: tera::Error::msg("missing variable"),
        };
        let printed = format!("{err}");
        assert!(printed.contains("lifecycle"), "{printed}");
        assert!(printed.contains("missing variable"), "{printed}");
    }
}
