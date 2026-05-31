use super::ContentType;
use super::validate::looks_like_url;

pub fn detect_content_type(input: &str) -> ContentType {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return ContentType::Unknown;
    }
    if looks_like_url(trimmed) {
        return ContentType::Url;
    }
    if looks_like_json(trimmed) {
        return ContentType::Json;
    }
    if looks_like_xml(trimmed) {
        return ContentType::Xml;
    }
    if looks_like_yaml(trimmed) {
        return ContentType::Yaml;
    }
    if looks_like_markdown(trimmed) {
        return ContentType::Markdown;
    }
    ContentType::Text
}

fn looks_like_json(input: &str) -> bool {
    matches!(input.chars().next(), Some('{') | Some('['))
}

fn looks_like_xml(input: &str) -> bool {
    if !input.starts_with('<') || !input.contains('>') {
        return false;
    }
    matches!(
        input.chars().nth(1),
        Some(ch) if ch.is_ascii_alphabetic() || matches!(ch, '/' | '!' | '?')
    )
}

fn looks_like_yaml(input: &str) -> bool {
    if input.starts_with("---") || input.starts_with("- ") {
        return true;
    }

    for raw_line in input.lines() {
        let line = raw_line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, _)) = line.split_once(':') {
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            if line.contains("://") {
                continue;
            }
            if key.contains('{') || key.contains('[') {
                continue;
            }
            return true;
        }
    }
    false
}

fn looks_like_markdown(input: &str) -> bool {
    let mut saw_inline_marker = false;
    for raw_line in input.lines() {
        let line = raw_line.trim_start();
        if line.starts_with('#')
            || line.starts_with("> ")
            || line.starts_with("```")
            || is_ordered_list_item(line)
            || contains_markdown_link(line)
        {
            return true;
        }
        if line.contains("**") || line.contains("__") || line.contains('`') {
            saw_inline_marker = true;
        }
    }
    saw_inline_marker
}

fn is_ordered_list_item(line: &str) -> bool {
    let mut chars = line.chars().peekable();
    let mut saw_digit = false;
    while let Some(ch) = chars.peek() {
        if ch.is_ascii_digit() {
            saw_digit = true;
            chars.next();
            continue;
        }
        break;
    }
    if !saw_digit {
        return false;
    }
    matches!(chars.next(), Some('.')) && matches!(chars.next(), Some(' '))
}

fn contains_markdown_link(line: &str) -> bool {
    let Some(open_bracket) = line.find('[') else {
        return false;
    };
    let Some(link_start_rel) = line[open_bracket + 1..].find("](") else {
        return false;
    };
    let link_start = open_bracket + 1 + link_start_rel + 2;
    line[link_start..].contains(')')
}
