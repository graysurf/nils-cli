use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};

#[derive(Debug, Parser)]
#[command(
    name = "web-evidence",
    version,
    about = "Capture redacted static HTTP evidence for agent workflows.",
    long_about = "Capture redacted HTTP metadata, previews, and manifests into deterministic artifact directories for workflow evidence.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  web-evidence capture https://example.com --out /tmp/web-evidence\n  web-evidence capture https://example.com --out /tmp/web-evidence --format json\n  web-evidence capture https://example.com --out /tmp/web-evidence --method head\n  web-evidence completion zsh\n\nENVIRONMENT:\n  none\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Capture redacted HTTP metadata and artifact files for one URL.
    Capture(CaptureArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
pub struct CaptureArgs {
    /// HTTP or HTTPS URL to capture.
    #[arg(value_name = "URL")]
    pub url: String,

    /// Artifact run directory to create or reuse.
    #[arg(long = "out", value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub out_dir: PathBuf,

    /// Optional scenario or evidence label.
    #[arg(long, value_name = "LABEL")]
    pub label: Option<String>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,

    /// HTTP method to use.
    #[arg(long, value_enum, default_value_t = HttpMethod::Get)]
    pub method: HttpMethod,

    /// Total request timeout in seconds.
    #[arg(long, value_name = "SECONDS", default_value_t = 10)]
    pub timeout_seconds: u64,

    /// Maximum response body bytes to read before truncating the preview source.
    #[arg(long, value_name = "BYTES", default_value_t = 262_144)]
    pub max_body_bytes: usize,

    /// Maximum redacted body preview bytes to persist.
    #[arg(long, value_name = "BYTES", default_value_t = 8192)]
    pub body_preview_bytes: usize,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    /// Shell to generate completion script for.
    #[arg(value_enum)]
    pub shell: crate::completion::CompletionShell,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum HttpMethod {
    Get,
    Head,
}

impl HttpMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
        }
    }

    pub fn reqwest_method(self) -> reqwest::Method {
        match self {
            Self::Get => reqwest::Method::GET,
            Self::Head => reqwest::Method::HEAD,
        }
    }
}
