use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use clap::ValueEnum;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const EXIT_OK: i32 = 0;
pub const EXIT_RUNTIME: i32 = 1;
pub const EXIT_USAGE: i32 = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug)]
pub struct CliError(Box<CliErrorData>);

#[derive(Debug)]
struct CliErrorData {
    code: String,
    message: String,
    details: Option<Value>,
    exit_code: i32,
}

impl CliError {
    pub fn usage(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Option<Value>,
    ) -> Self {
        Self(Box::new(CliErrorData {
            code: code.into(),
            message: message.into(),
            details,
            exit_code: EXIT_USAGE,
        }))
    }

    pub fn runtime(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Option<Value>,
    ) -> Self {
        Self(Box::new(CliErrorData {
            code: code.into(),
            message: message.into(),
            details,
            exit_code: EXIT_RUNTIME,
        }))
    }
}

#[derive(Serialize)]
struct SuccessEnvelope<'a, T: Serialize> {
    schema_version: &'a str,
    command: &'a str,
    ok: bool,
    result: &'a T,
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    schema_version: &'a str,
    command: &'a str,
    ok: bool,
    error: ErrorView<'a>,
}

#[derive(Serialize)]
struct ErrorView<'a> {
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
}

pub fn render_success<T: Serialize>(
    schema_version: &'static str,
    command: &'static str,
    format: OutputFormat,
    text: impl FnOnce() -> String,
    result: &T,
) -> i32 {
    match format {
        OutputFormat::Text => println!("{}", text()),
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&SuccessEnvelope {
                schema_version,
                command,
                ok: true,
                result,
            })
            .expect("success envelope should serialize")
        ),
    }
    EXIT_OK
}

pub fn render_error(
    schema_version: &'static str,
    command: &'static str,
    format: OutputFormat,
    err: CliError,
) -> i32 {
    let err = *err.0;
    let exit_code = err.exit_code;
    match format {
        OutputFormat::Text => {
            eprintln!("error[{}]: {}", err.code, err.message);
            if let Some(details) = &err.details {
                eprintln!("details: {details}");
            }
        }
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&ErrorEnvelope {
                schema_version,
                command,
                ok: false,
                error: ErrorView {
                    code: &err.code,
                    message: &err.message,
                    details: err.details,
                },
            })
            .expect("error envelope should serialize")
        ),
    }
    exit_code
}

pub fn ensure_non_empty(flag: &str, value: &str) -> Result<(), CliError> {
    if value.trim().is_empty() {
        return Err(CliError::usage(
            "empty-value",
            format!("{flag} must not be empty"),
            Some(json!({ "flag": flag })),
        ));
    }
    Ok(())
}

pub fn absolute_path(path: &Path) -> Result<PathBuf, CliError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir().map_err(|err| {
        CliError::runtime(
            "cwd-unavailable",
            format!("failed to read current directory: {err}"),
            None,
        )
    })?;
    Ok(cwd.join(path))
}

pub fn display_path(path: &Path) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => parts.push(value.to_string_lossy().to_string()),
            Component::ParentDir => parts.push("..".to_string()),
            Component::RootDir | Component::Prefix(_) => {
                parts.push(component.as_os_str().to_string_lossy().to_string())
            }
        }
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/").replace("//", "/")
    }
}

pub fn normalized_paths(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|path| display_path(path)).collect()
}

pub fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            CliError::runtime(
                "create-dir-failed",
                format!("failed to create {}: {err}", parent.display()),
                Some(json!({ "path": display_path(parent) })),
            )
        })?;
    }
    let body = serde_json::to_string_pretty(value).expect("record should serialize");
    fs::write(path, format!("{body}\n")).map_err(|err| {
        CliError::runtime(
            "write-failed",
            format!("failed to write {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, CliError> {
    let body = fs::read_to_string(path).map_err(|err| {
        CliError::runtime(
            "read-failed",
            format!("failed to read {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    serde_json::from_str(&body).map_err(|err| {
        CliError::runtime(
            "invalid-json",
            format!("failed to parse {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })
}

pub fn record_path(out_dir: &Path, file_name: &str) -> Result<PathBuf, CliError> {
    Ok(absolute_path(out_dir)?.join(file_name))
}

pub fn redact_text(input: &str) -> String {
    static SECRET_RE: OnceLock<Regex> = OnceLock::new();
    let re = SECRET_RE.get_or_init(|| {
        Regex::new(r"(?i)(sk-[A-Za-z0-9_-]+|ghp_[A-Za-z0-9_]+|[A-Za-z0-9_]*(?:TOKEN|KEY|SECRET|PASSWORD)[A-Za-z0-9_]*=\S+)")
            .expect("secret regex should compile")
    });
    re.replace_all(input, "[REDACTED]").into_owned()
}

pub fn redact_strings(values: &[String]) -> Vec<String> {
    values.iter().map(|value| redact_text(value)).collect()
}

pub fn preview_text(bytes: &[u8], limit: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.as_ref();
    let preview = if text.len() > limit {
        let mut end = limit;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    } else {
        text
    };
    redact_text(preview)
}

#[cfg(test)]
mod tests {
    use super::preview_text;

    #[test]
    fn preview_text_truncates_on_utf8_char_boundary() {
        let bytes = [0xe2, 0x82, 0xac, b' ', b'o', b'u', b't'];

        assert_eq!(preview_text(&bytes, 1), "");
        assert_eq!(preview_text(&bytes, 2), "");
        assert_eq!(preview_text(&bytes, 3).as_bytes(), &[0xe2, 0x82, 0xac]);
        assert_eq!(
            preview_text(&bytes, 4).as_bytes(),
            &[0xe2, 0x82, 0xac, b' ']
        );
    }
}
