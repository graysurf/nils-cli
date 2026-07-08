use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use clap::error::ErrorKind;
pub use nils_common::cli_contract::OutputFormat;
use nils_common::cli_contract::{Envelope, EnvelopeError, emit_parse_error, exit};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// Re-export exit-code constants under the historical names so the eight
// binaries can keep importing `EXIT_USAGE` / `EXIT_RUNTIME` while the values
// come from the shared `nils_common::cli_contract::exit` module.
pub const EXIT_OK: i32 = exit::SUCCESS;
pub const EXIT_RUNTIME: i32 = exit::RUNTIME;
pub const EXIT_USAGE: i32 = exit::USAGE;
pub const EXIT_DATA: i32 = exit::DATA;
pub const EXIT_UNAVAILABLE: i32 = exit::UNAVAILABLE;

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
    /// Machine-readable error code (the `error.code` envelope field).
    #[allow(dead_code)]
    pub fn code(&self) -> &str {
        &self.0.code
    }

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

    pub fn data(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Option<Value>,
    ) -> Self {
        Self(Box::new(CliErrorData {
            code: code.into(),
            message: message.into(),
            details,
            exit_code: EXIT_DATA,
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

    pub fn unavailable(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Option<Value>,
    ) -> Self {
        Self(Box::new(CliErrorData {
            code: code.into(),
            message: message.into(),
            details,
            exit_code: EXIT_UNAVAILABLE,
        }))
    }
}

pub fn render_success<T: Serialize>(
    schema_version: &'static str,
    _command: &'static str,
    format: OutputFormat,
    text: impl FnOnce() -> String,
    result: &T,
) -> i32 {
    match format {
        OutputFormat::Text => println!("{}", text()),
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&Envelope::success(schema_version, result))
                .expect("success envelope should serialize")
        ),
    }
    EXIT_OK
}

pub fn render_error(
    schema_version: &'static str,
    _command: &'static str,
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
        OutputFormat::Json => {
            let mut envelope_error = EnvelopeError::new(&err.code, &err.message);
            if let Some(details) = err.details {
                envelope_error = envelope_error.with_details(details);
            }
            let envelope: Envelope<()> = Envelope::failure(schema_version, envelope_error);
            println!(
                "{}",
                serde_json::to_string_pretty(&envelope).expect("error envelope should serialize")
            );
        }
    }
    exit_code
}

/// Route a clap parse error through the shared output contract.
///
/// `binary` should match the binary name advertised in `clap`'s `#[command(name)]`.
/// Help and version exits keep clap's native behavior; everything else lands
/// through `emit_parse_error` so `--format json` consumers see a JSON envelope.
pub fn handle_parse_error<I>(binary: &str, argv: I, err: clap::Error) -> i32
where
    I: IntoIterator<Item = OsString>,
{
    let kind = err.kind();
    if matches!(
        kind,
        ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    ) {
        let _ = err.print();
        return err.exit_code();
    }

    let argv: Vec<OsString> = argv.into_iter().collect();
    let format = detect_format_from_argv(&argv);
    let code = match kind {
        ErrorKind::InvalidSubcommand => "unknown-subcommand",
        _ => "parse-error",
    };
    let message = render_clap_message(&err);
    emit_parse_error(binary, format, code, &message)
}

fn detect_format_from_argv(argv: &[OsString]) -> OutputFormat {
    let mut iter = argv.iter().skip(1);
    while let Some(arg) = iter.next() {
        let arg = arg.to_string_lossy();
        if arg == "--json" {
            return OutputFormat::Json;
        }
        if arg == "--format"
            && let Some(next) = iter.next()
            && next.to_string_lossy().eq_ignore_ascii_case("json")
        {
            return OutputFormat::Json;
        }
        if let Some(rest) = arg.strip_prefix("--format=")
            && rest.eq_ignore_ascii_case("json")
        {
            return OutputFormat::Json;
        }
    }
    OutputFormat::Text
}

fn render_clap_message(err: &clap::Error) -> String {
    err.to_string()
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| {
            let line = line.trim();
            line.strip_prefix("error:")
                .map(str::trim)
                .unwrap_or(line)
                .to_string()
        })
        .unwrap_or_else(|| "command-line parse failed".to_string())
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

/// Returns true when `value` begins with a URI scheme (`^[A-Za-z][A-Za-z0-9+.-]*://`),
/// e.g. `https://`, `http://`, `git+ssh://`. Used to keep URLs out of the
/// filesystem-path normalizer, which would otherwise collapse the scheme's `//`.
fn is_url_like(value: &str) -> bool {
    let Some((scheme, _rest)) = value.split_once("://") else {
        return false;
    };
    let mut chars = scheme.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

pub fn display_path(path: &Path) -> String {
    // A URL (https://…, git+ssh://…) carried through this normalizer would have
    // its scheme separator collapsed by Path::components() (https://host →
    // https:/host) and the trailing .replace("//", "/"). Detect a leading URI
    // scheme and return the value unchanged so retained records keep lossless,
    // clickable provider links (nils-cli#1054). The PathBuf holds the original
    // bytes, so to_string_lossy recovers the URL as typed.
    let raw = path.to_string_lossy();
    if is_url_like(&raw) {
        return raw.into_owned();
    }
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
        Regex::new(r"(?i)(-----BEGIN [A-Z ]*PRIVATE KEY-----|Authorization:\s*Bearer\s+\S+|Cookie:\s*\S+|sk-[A-Za-z0-9_-]{12,}|ghp_[A-Za-z0-9_]+|[A-Za-z0-9_]*(?:API[_-]?KEY|TOKEN|KEY|SECRET|PASSWORD)[A-Za-z0-9_]*\s*=\s*\S+)")
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
    use super::{display_path, preview_text};
    use std::path::Path;

    #[test]
    fn display_path_preserves_url_scheme_double_slash() {
        // A URL routed through display_path must keep its scheme separator; the
        // Path::components() normalizer would otherwise collapse https:// to
        // https:/ (nils-cli#1054), breaking retained provider links.
        assert_eq!(
            display_path(Path::new(
                "https://github.com/sympoies/nils-cli/issues/1046#issuecomment-4909586590"
            )),
            "https://github.com/sympoies/nils-cli/issues/1046#issuecomment-4909586590"
        );
        assert_eq!(
            display_path(Path::new("http://example.com/a/b")),
            "http://example.com/a/b"
        );
        assert_eq!(
            display_path(Path::new("git+ssh://git@host/repo.git")),
            "git+ssh://git@host/repo.git"
        );
    }

    #[test]
    fn display_path_still_normalizes_filesystem_paths() {
        // Non-URL paths keep the existing normalization, including the
        // scheme-like ":" that is not a leading URI scheme.
        assert_eq!(
            display_path(Path::new("./docs/./runbook.md")),
            "docs/runbook.md"
        );
        assert_eq!(display_path(Path::new("a/b://c")), "a/b:/c");
        assert_eq!(display_path(Path::new(".")), ".");
    }

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
