use anyhow::Error;

pub fn format_error(err: &Error) -> String {
    let rendered = format!("{err:#}");
    match hint_for(&rendered) {
        Some(hint) => format!("{rendered}\nHint: {hint}"),
        None => rendered,
    }
}

fn hint_for(rendered: &str) -> Option<&'static str> {
    let lower = rendered.to_ascii_lowercase();

    if lower.contains("git-lock") && lower.contains("not found") {
        return Some("run `git-lock list` to see available locks.");
    }

    if lower.contains("ambiguous") && lower.contains("label") {
        return Some("use the full git-lock label shown by `git-lock list`.");
    }

    if lower.contains("git ") && (lower.contains("failed") || lower.contains("not found")) {
        return Some("ensure `git` is installed and run this command inside a Git repository.");
    }

    None
}

#[cfg(test)]
mod tests {
    use super::format_error;
    use anyhow::anyhow;

    #[test]
    fn formats_label_not_found_hint() {
        let err = anyhow!("git-lock [repo:missing] not found");
        let rendered = format_error(&err);
        assert!(rendered.contains("run `git-lock list`"));
    }

    #[test]
    fn formats_ambiguous_label_hint() {
        let err = anyhow!("ambiguous label: rel");
        let rendered = format_error(&err);
        assert!(rendered.contains("use the full git-lock label"));
    }

    #[test]
    fn formats_git_command_hint() {
        let err = anyhow!("git [\"diff\", \"--stat\"] failed");
        let rendered = format_error(&err);
        assert!(rendered.contains("ensure `git` is installed"));
    }

    #[test]
    fn leaves_unknown_errors_unchanged() {
        let err = anyhow!("something else failed");
        let rendered = format_error(&err);
        assert_eq!(rendered, "something else failed");
    }
}
