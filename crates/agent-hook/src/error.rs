use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct HookError {
    pub code: String,
    pub message: String,
    pub details: Option<Box<Value>>,
    pub exit_code: i32,
}

impl HookError {
    pub fn usage(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message, None, 64)
    }

    pub fn data(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message, None, 65)
    }

    pub fn data_with(code: impl Into<String>, message: impl Into<String>, details: Value) -> Self {
        Self::new(code, message, Some(Box::new(details)), 65)
    }

    pub fn runtime(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message, None, 1)
    }

    pub fn unavailable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message, None, 75)
    }

    pub fn blocked(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message, None, 1)
    }

    fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Option<Box<Value>>,
        exit_code: i32,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details,
            exit_code,
        }
    }
}

impl std::fmt::Display for HookError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HookError {}

#[derive(Debug, Serialize)]
pub struct ErrorBody<'a> {
    pub code: &'a str,
    pub message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<&'a Value>,
}

#[derive(Debug, Serialize)]
pub struct ErrorEnvelope<'a> {
    pub schema_version: String,
    pub command: &'a str,
    pub ok: bool,
    pub error: ErrorBody<'a>,
}

#[derive(Debug, Serialize)]
pub struct SuccessEnvelope<'a, T: Serialize> {
    pub schema_version: String,
    pub command: &'a str,
    pub ok: bool,
    pub result: &'a T,
}

pub fn schema(command: &str) -> String {
    format!("cli.{}.v1", command.replace(' ', "-"))
}
