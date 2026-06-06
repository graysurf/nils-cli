use std::path::PathBuf;

use crate::paths;

#[derive(Debug)]
pub enum PromptTemplateError {
    PromptsDirNotFound,
    TemplateMissing { path: PathBuf },
    ReadFailed { path: PathBuf },
}

pub fn resolve_prompts_dir() -> Option<PathBuf> {
    let zdotdir = paths::resolve_zdotdir()?;
    let primary = zdotdir.join("prompts");
    if primary.is_dir() {
        return Some(primary);
    }

    let feature_dir = paths::resolve_feature_dir()?;
    let fallback = feature_dir.join("prompts");
    if fallback.is_dir() {
        return Some(fallback);
    }

    None
}

pub fn read_template(template_name: &str) -> Result<(PathBuf, String), PromptTemplateError> {
    let prompts_dir = resolve_prompts_dir().ok_or(PromptTemplateError::PromptsDirNotFound)?;
    let path = prompts_dir.join(format!("{template_name}.md"));

    if !path.is_file() {
        return Err(PromptTemplateError::TemplateMissing { path });
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|_| PromptTemplateError::ReadFailed { path: path.clone() })?;
    Ok((path, content))
}
