use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    Usage,
    Backend,
    Upstream,
    Journal,
    Transport,
    Permission,
    Policy,
}

impl ErrorClass {
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Usage => 64,
            Self::Backend => 69,
            Self::Upstream => 70,
            Self::Journal => 74,
            Self::Transport => 75,
            Self::Permission => 77,
            Self::Policy => 78,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::Backend => "backend",
            Self::Upstream => "upstream",
            Self::Journal => "journal",
            Self::Transport => "transport",
            Self::Permission => "permission",
            Self::Policy => "policy",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CliError {
    class: ErrorClass,
    message: String,
    operation: Option<String>,
    hints: Vec<String>,
}

impl CliError {
    pub fn new(class: ErrorClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: normalize_message(message.into()),
            operation: None,
            hints: Vec::new(),
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Usage, message)
    }

    pub fn backend(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Backend, message)
    }

    pub fn upstream(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Upstream, message)
    }

    pub fn journal(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Journal, message)
    }

    pub fn transport(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Transport, message)
    }

    pub fn permission(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Permission, message)
    }

    pub fn policy(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Policy, message)
    }

    pub fn with_operation(mut self, operation: impl Into<String>) -> Self {
        let operation = operation.into();
        if !operation.trim().is_empty() {
            self.operation = Some(operation);
        }
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        let hint = hint.into();
        if !hint.trim().is_empty() {
            self.hints.push(hint);
        }
        self
    }

    pub const fn class(&self) -> ErrorClass {
        self.class
    }

    pub const fn exit_code(&self) -> u8 {
        self.class.exit_code()
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn operation(&self) -> Option<&str> {
        self.operation.as_deref()
    }

    pub fn hints(&self) -> &[String] {
        &self.hints
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "error: {}", self.message)
    }
}

impl std::error::Error for CliError {}

fn normalize_message(message: String) -> String {
    message
        .trim()
        .trim_start_matches("error:")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{CliError, ErrorClass};

    #[test]
    fn exit_classes_are_stable_and_distinct() {
        let classes = [
            ErrorClass::Usage,
            ErrorClass::Backend,
            ErrorClass::Upstream,
            ErrorClass::Journal,
            ErrorClass::Transport,
            ErrorClass::Permission,
            ErrorClass::Policy,
        ];
        let mut codes = classes.map(ErrorClass::exit_code).to_vec();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), classes.len());
    }

    #[test]
    fn normalizes_nested_error_prefixes() {
        let error = CliError::backend(" error: broken ");
        assert_eq!(error.message(), "broken");
        assert_eq!(error.exit_code(), 69);
    }
}
