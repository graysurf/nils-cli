mod detect;
mod validate;

use serde::{Deserialize, Serialize};

pub use detect::detect_content_type;
pub use validate::validate_content;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    Url,
    Json,
    Yaml,
    Xml,
    Markdown,
    Text,
    Unknown,
}

impl ContentType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Xml => "xml",
            Self::Markdown => "markdown",
            Self::Text => "text",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "url" => Some(Self::Url),
            "json" => Some(Self::Json),
            "yaml" => Some(Self::Yaml),
            "xml" => Some(Self::Xml),
            "markdown" => Some(Self::Markdown),
            "text" => Some(Self::Text),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

impl std::fmt::Display for ContentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    Valid,
    Invalid,
    Unknown,
    Skipped,
}

impl ValidationStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::Unknown => "unknown",
            Self::Skipped => "skipped",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "valid" => Some(Self::Valid),
            "invalid" => Some(Self::Invalid),
            "unknown" => Some(Self::Unknown),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

impl std::fmt::Display for ValidationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl ValidationError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            path: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationResult {
    pub status: ValidationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<ValidationError>>,
}

impl ValidationResult {
    pub fn valid() -> Self {
        Self {
            status: ValidationStatus::Valid,
            errors: None,
        }
    }

    pub fn invalid(errors: Vec<ValidationError>) -> Self {
        Self {
            status: ValidationStatus::Invalid,
            errors: Some(errors),
        }
    }

    pub fn unknown() -> Self {
        Self {
            status: ValidationStatus::Unknown,
            errors: None,
        }
    }

    pub fn skipped() -> Self {
        Self {
            status: ValidationStatus::Skipped,
            errors: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisMetadata {
    pub content_type: ContentType,
    pub validation: ValidationResult,
}

pub fn analyze(input: &str) -> AnalysisMetadata {
    let content_type = detect_content_type(input);
    let validation = validate_content(content_type, input);
    AnalysisMetadata {
        content_type,
        validation,
    }
}

#[cfg(test)]
mod tests {
    use super::{ContentType, ValidationStatus, analyze};

    fn assert_invalid(input: &str, expected_type: ContentType, expected_code: &str) {
        let result = analyze(input);
        assert_eq!(result.content_type, expected_type);
        assert_eq!(result.validation.status, ValidationStatus::Invalid);
        let errors = result
            .validation
            .errors
            .as_ref()
            .expect("invalid result must include errors");
        assert_eq!(errors[0].code, expected_code);
    }

    #[test]
    fn analyze_valid_url() {
        let result = analyze("https://example.com/docs?q=1");
        assert_eq!(result.content_type, ContentType::Url);
        assert_eq!(result.validation.status, ValidationStatus::Valid);
        assert!(result.validation.errors.is_none());
    }

    #[test]
    fn analyze_invalid_url() {
        assert_invalid("https://", ContentType::Url, "invalid-url");
    }

    #[test]
    fn analyze_detects_json_before_yaml() {
        let input = r#"{"name":"memo","priority":1}"#;
        let result = analyze(input);
        assert_eq!(result.content_type, ContentType::Json);
        assert_eq!(result.validation.status, ValidationStatus::Valid);
    }

    #[test]
    fn analyze_invalid_json() {
        assert_invalid(r#"{"name":"memo""#, ContentType::Json, "invalid-json");
    }

    #[test]
    fn analyze_valid_yaml() {
        let input = "name: memo\npriority: high";
        let result = analyze(input);
        assert_eq!(result.content_type, ContentType::Yaml);
        assert_eq!(result.validation.status, ValidationStatus::Valid);
    }

    #[test]
    fn analyze_invalid_yaml() {
        assert_invalid(
            "name: memo\n\tpriority: high",
            ContentType::Yaml,
            "invalid-yaml",
        );
    }

    #[test]
    fn analyze_valid_xml() {
        let result = analyze("<root><item>memo</item></root>");
        assert_eq!(result.content_type, ContentType::Xml);
        assert_eq!(result.validation.status, ValidationStatus::Valid);
    }

    #[test]
    fn analyze_invalid_xml() {
        assert_invalid("<root><item>memo</root>", ContentType::Xml, "invalid-xml");
    }

    #[test]
    fn analyze_valid_markdown() {
        let input = "# Inbox\n\n- [ ] buy milk";
        let result = analyze(input);
        assert_eq!(result.content_type, ContentType::Markdown);
        assert_eq!(result.validation.status, ValidationStatus::Valid);
    }

    #[test]
    fn analyze_invalid_markdown() {
        let input = "```rust\nfn main() {}\n";
        assert_invalid(input, ContentType::Markdown, "invalid-markdown");
    }

    #[test]
    fn analyze_plain_text_fallback() {
        let result = analyze("buy milk tomorrow");
        assert_eq!(result.content_type, ContentType::Text);
        assert_eq!(result.validation.status, ValidationStatus::Skipped);
        assert!(result.validation.errors.is_none());
    }

    #[test]
    fn analyze_empty_input_fallback() {
        let result = analyze("   \n\t");
        assert_eq!(result.content_type, ContentType::Unknown);
        assert_eq!(result.validation.status, ValidationStatus::Unknown);
        assert!(result.validation.errors.is_none());
    }
}
