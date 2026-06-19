//! Command-level errors mapped to the workspace exit-code contract.

use nils_common::cli_contract::exit;

/// A command failure carrying a stable machine `code` (for the JSON error
/// envelope) and a BSD sysexits-aligned process `exit_code`.
#[derive(Debug, Clone)]
pub struct CommandError {
    pub exit_code: i32,
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
}

impl CommandError {
    fn new(exit_code: i32, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            exit_code,
            code: code.into(),
            message: message.into(),
            hint: None,
        }
    }

    /// Bad CLI usage discovered after parse (exit `64`).
    pub fn usage(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(exit::USAGE, code, message)
    }

    /// Invalid or missing input data, e.g. an unreadable or malformed key
    /// (exit `65`).
    pub fn data(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(exit::DATA, code, message)
    }

    /// A required service or resource is unavailable, e.g. a network or GitHub
    /// API failure (exit `69`).
    pub fn unavailable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(exit::UNAVAILABLE, code, message)
    }

    /// Attach an optional human-readable hint.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn exit_codes_match_category() {
        assert_eq!(CommandError::usage("c", "m").exit_code, 64);
        assert_eq!(CommandError::data("c", "m").exit_code, 65);
        assert_eq!(CommandError::unavailable("c", "m").exit_code, 69);
    }
}
