use super::{ContentType, ValidationError, ValidationResult};

pub fn validate_content(content_type: ContentType, input: &str) -> ValidationResult {
    match content_type {
        ContentType::Url => validate_url(input),
        ContentType::Json => validate_json(input),
        ContentType::Yaml => validate_yaml(input),
        ContentType::Xml => validate_xml(input),
        ContentType::Markdown => validate_markdown(input),
        ContentType::Text => ValidationResult::skipped(),
        ContentType::Unknown => ValidationResult::unknown(),
    }
}

pub(crate) fn looks_like_url(input: &str) -> bool {
    let candidate = input.trim();
    if candidate.is_empty() {
        return false;
    }
    if candidate.chars().any(|ch| ch.is_whitespace()) {
        return false;
    }
    let Some((scheme, remainder)) = candidate.split_once("://") else {
        return false;
    };
    !scheme.is_empty() && !remainder.chars().any(|ch| ch.is_whitespace())
}

fn validate_url(input: &str) -> ValidationResult {
    let candidate = input.trim();
    if !looks_like_url(candidate) {
        return invalid(
            "invalid-url",
            "URL must include a scheme and host, e.g. https://example.com",
            None,
        );
    }

    let Some((scheme, remainder)) = candidate.split_once("://") else {
        return invalid(
            "invalid-url",
            "URL must include a scheme and host, e.g. https://example.com",
            None,
        );
    };
    if !is_valid_url_scheme(scheme) {
        return invalid(
            "invalid-url",
            "URL scheme contains unsupported characters",
            Some("scheme".to_string()),
        );
    }

    let host_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let host = &remainder[..host_end];
    if host.is_empty() {
        return invalid(
            "invalid-url",
            "URL host is missing",
            Some("host".to_string()),
        );
    }
    if host.starts_with('.') || host.ends_with('.') || host.contains("..") {
        return invalid(
            "invalid-url",
            "URL host is malformed",
            Some("host".to_string()),
        );
    }
    if host.chars().any(|ch| ch.is_whitespace()) || host.contains('@') {
        return invalid(
            "invalid-url",
            "URL host is malformed",
            Some("host".to_string()),
        );
    }

    ValidationResult::valid()
}

fn is_valid_url_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

fn validate_json(input: &str) -> ValidationResult {
    let candidate = input.trim();
    match serde_json::from_str::<serde_json::Value>(candidate) {
        Ok(_) => ValidationResult::valid(),
        Err(err) => {
            let line = err.line();
            let column = err.column();
            let path = if line > 0 && column > 0 {
                Some(format!("line:{line},column:{column}"))
            } else {
                None
            };
            invalid("invalid-json", format!("invalid JSON syntax: {err}"), path)
        }
    }
}

fn validate_yaml(input: &str) -> ValidationResult {
    let lines: Vec<(usize, &str)> = input
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            Some((index + 1, line))
        })
        .collect();

    if lines.is_empty() {
        return invalid("invalid-yaml", "YAML content is empty", None);
    }

    let mut saw_yaml_token = false;
    for (line_no, raw_line) in &lines {
        if raw_line.contains('\t') {
            return invalid(
                "invalid-yaml",
                "YAML indentation must use spaces, not tabs",
                Some(format!("line:{line_no}")),
            );
        }

        let line = raw_line.trim_start();
        if line == "---" || line == "..." {
            saw_yaml_token = true;
            continue;
        }
        if line.starts_with("- ") {
            saw_yaml_token = true;
            if line.trim() == "-" {
                return invalid(
                    "invalid-yaml",
                    "YAML list item is missing a value",
                    Some(format!("line:{line_no}")),
                );
            }
            continue;
        }
        if let Some((key, _)) = line.split_once(':') {
            saw_yaml_token = true;
            if key.trim().is_empty() {
                return invalid(
                    "invalid-yaml",
                    "YAML mapping key cannot be empty",
                    Some(format!("line:{line_no}")),
                );
            }
            continue;
        }
        if lines.len() == 1 {
            return ValidationResult::valid();
        }
        return invalid(
            "invalid-yaml",
            "YAML line is not parseable in mapping/list form",
            Some(format!("line:{line_no}")),
        );
    }

    if saw_yaml_token || lines.len() == 1 {
        return ValidationResult::valid();
    }

    invalid("invalid-yaml", "YAML content is malformed", None)
}

fn validate_xml(input: &str) -> ValidationResult {
    let candidate = input.trim();
    if candidate.is_empty() {
        return invalid("invalid-xml", "XML content is empty", None);
    }

    let mut stack: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    let mut saw_element = false;

    while let Some(rel_start) = candidate[cursor..].find('<') {
        let start = cursor + rel_start;
        let token_start = start + 1;
        let Some(rel_end) = candidate[token_start..].find('>') else {
            return invalid(
                "invalid-xml",
                "XML contains an unclosed tag",
                Some(format!("byte:{token_start}")),
            );
        };
        let token_end = token_start + rel_end;
        let token = candidate[token_start..token_end].trim();
        cursor = token_end + 1;

        if token.is_empty() {
            return invalid(
                "invalid-xml",
                "XML tag cannot be empty",
                Some(format!("byte:{token_start}")),
            );
        }
        if token.starts_with("!--") {
            if !token.ends_with("--") {
                return invalid(
                    "invalid-xml",
                    "XML comment is malformed",
                    Some(format!("byte:{token_start}")),
                );
            }
            continue;
        }
        if token.starts_with('?') || token.starts_with('!') {
            continue;
        }

        if let Some(rest) = token.strip_prefix('/') {
            let name = match parse_xml_tag_name(rest) {
                Ok(name) => name.to_string(),
                Err(err) => return ValidationResult::invalid(vec![err]),
            };
            saw_element = true;
            let Some(open_name) = stack.pop() else {
                return invalid(
                    "invalid-xml",
                    "XML has a closing tag without a matching opening tag",
                    Some(format!("/{name}")),
                );
            };
            if open_name != name {
                return invalid(
                    "invalid-xml",
                    format!("XML closing tag does not match opening tag: expected </{open_name}>"),
                    Some(format!("/{name}")),
                );
            }
            continue;
        }

        let self_closing = token.ends_with('/');
        let open_token = if self_closing {
            token[..token.len() - 1].trim_end()
        } else {
            token
        };

        let name = match parse_xml_tag_name(open_token) {
            Ok(name) => name.to_string(),
            Err(err) => return ValidationResult::invalid(vec![err]),
        };
        saw_element = true;
        if !self_closing {
            stack.push(name);
        }
    }

    if !saw_element {
        return invalid("invalid-xml", "XML does not contain any element tags", None);
    }
    if !stack.is_empty() {
        return invalid(
            "invalid-xml",
            "XML has unclosed tags",
            Some(format!("/{}", stack.join("/"))),
        );
    }

    ValidationResult::valid()
}

fn parse_xml_tag_name(token: &str) -> Result<&str, ValidationError> {
    let name = token.split_whitespace().next().unwrap_or("");
    if name.is_empty() {
        return Err(ValidationError::new(
            "invalid-xml",
            "XML tag name cannot be empty",
        ));
    }

    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(ValidationError::new(
            "invalid-xml",
            "XML tag name cannot be empty",
        ));
    };
    if !(first.is_ascii_alphabetic() || matches!(first, '_' | ':')) {
        return Err(ValidationError::new(
            "invalid-xml",
            "XML tag name contains invalid characters",
        ));
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':')) {
        return Err(ValidationError::new(
            "invalid-xml",
            "XML tag name contains invalid characters",
        ));
    }
    Ok(name)
}

fn validate_markdown(input: &str) -> ValidationResult {
    let mut fence_count = 0usize;
    let mut last_fence_line = None;

    for (index, raw_line) in input.lines().enumerate() {
        let line_no = index + 1;
        let line = raw_line.trim_start();

        if line.starts_with("```") {
            fence_count += 1;
            last_fence_line = Some(line_no);
        }

        if let Some(link_start) = raw_line.find("](") {
            let candidate = &raw_line[link_start + 2..];
            if !candidate.contains(')') {
                return invalid(
                    "invalid-markdown",
                    "Markdown link is missing a closing parenthesis",
                    Some(format!("line:{line_no}")),
                );
            }
        }
    }

    if fence_count % 2 == 1 {
        return invalid(
            "invalid-markdown",
            "Markdown fenced code block is not closed",
            last_fence_line.map(|line_no| format!("line:{line_no}")),
        );
    }

    ValidationResult::valid()
}

fn invalid(
    code: impl Into<String>,
    message: impl Into<String>,
    path: Option<String>,
) -> ValidationResult {
    let mut err = ValidationError::new(code, message);
    if let Some(path) = path {
        err = err.with_path(path);
    }
    ValidationResult::invalid(vec![err])
}
