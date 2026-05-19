use nils_common::cli_contract::exit;
use serde_json::json;

#[derive(Debug)]
pub struct AppError {
    exit_code: i32,
    code: Box<str>,
    message: Box<str>,
    details: Option<Box<serde_json::Value>>,
}

impl AppError {
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            exit_code: exit::USAGE,
            code: "invalid-arguments".into(),
            message: message.into().into_boxed_str(),
            details: None,
        }
    }

    pub fn data(message: impl Into<String>) -> Self {
        Self {
            exit_code: exit::DATA,
            code: "invalid-input".into(),
            message: message.into().into_boxed_str(),
            details: None,
        }
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self {
            exit_code: exit::RUNTIME,
            code: "runtime-failure".into(),
            message: message.into().into_boxed_str(),
            details: None,
        }
    }

    pub fn db(err: rusqlite::Error) -> Self {
        Self::runtime(format!("database error: {err}"))
    }

    pub fn db_open(err: impl std::fmt::Display) -> Self {
        Self::runtime(format!("database open failed: {err}")).with_code("db-open-failed")
    }

    pub fn db_query(err: rusqlite::Error) -> Self {
        Self::runtime(format!("database query failed: {err}")).with_code("db-query-failed")
    }

    pub fn db_write(err: rusqlite::Error) -> Self {
        Self::runtime(format!("database write failed: {err}")).with_code("db-write-failed")
    }

    pub fn invalid_cursor(cursor: &str) -> Self {
        Self::usage("cursor is invalid for current database state")
            .with_code("invalid-cursor")
            .with_details(json!({ "cursor": cursor }))
    }

    pub fn invalid_apply_payload(message: impl Into<String>, path: Option<&str>) -> Self {
        let mut err = Self::data(message).with_code("invalid-apply-payload");
        if let Some(path) = path {
            err = err.with_details(json!({ "path": path }));
        }
        err
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = code.into().into_boxed_str();
        self
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(Box::new(details));
        self
    }

    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }

    pub fn code(&self) -> &str {
        self.code.as_ref()
    }

    pub fn message(&self) -> &str {
        self.message.as_ref()
    }

    pub fn details(&self) -> Option<&serde_json::Value> {
        self.details.as_deref()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(value: anyhow::Error) -> Self {
        Self::runtime(value.to_string())
    }
}
