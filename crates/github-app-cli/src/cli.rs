//! Clap command model — the single source of truth for parsing and completion.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use nils_common::cli_contract::OutputFormat;

use crate::completion::CompletionShell;

#[derive(Debug, Parser)]
#[command(
    name = "github-app-cli",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Mint GitHub App installation tokens for forge-cli and other tooling.",
    long_about = "Mint short-lived GitHub App installation access tokens (and list \
installations) so automation can act under a GitHub App bot identity. In text mode the \
`token` command writes the raw token to stdout for capture via \
`GH_TOKEN=$(github-app-cli token ...)`; JSON mode reports only non-secret metadata.",
    after_help = "EXAMPLES:\n  \
github-app-cli token --app-id 123 --installation-id 456 --key app.pem\n  \
GH_TOKEN=\"$(github-app-cli token)\" forge-cli pr deliver ...\n  \
github-app-cli installations --app-id 123 --key app.pem\n\
\nENVIRONMENT:\n  \
GITHUB_APP_ID                Default --app-id (App ID or Client ID).\n  \
GITHUB_APP_INSTALLATION_ID   Default --installation-id.\n  \
GITHUB_APP_PRIVATE_KEY_PATH  Default --key (path to the RSA private-key PEM).\n  \
GITHUB_APP_PRIVATE_KEY       RSA private-key PEM contents (overrides --key).\n  \
GITHUB_API_URL               REST API base URL (default https://api.github.com).\n\
\nEXIT CODES:\n  \
0   success\n  \
64  command-line usage error\n  \
65  invalid input data (unreadable / malformed key)\n  \
69  GitHub API or network unavailable\n  \
70  internal software error",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Output format (defaults to text).
    #[arg(long, global = true, value_enum)]
    pub format: Option<OutputFormat>,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// Resolve the effective output format.
    pub fn output_format(&self) -> OutputFormat {
        self.format.unwrap_or_default()
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Mint an installation access token (text mode: token on stdout).
    Token(TokenArgs),
    /// List the App's installations and their installation IDs.
    Installations(InstallationsArgs),
    /// Print a shell completion script.
    Completion(CompletionArgs),
}

/// Authentication inputs shared by the API-backed subcommands.
#[derive(Debug, Args)]
pub struct AppAuthArgs {
    /// GitHub App ID or Client ID (used as the JWT issuer).
    #[arg(long, env = "GITHUB_APP_ID", value_name = "ID")]
    pub app_id: String,

    /// Path to the App's RSA private-key PEM. Overridden by GITHUB_APP_PRIVATE_KEY.
    #[arg(long, env = "GITHUB_APP_PRIVATE_KEY_PATH", value_name = "PATH")]
    pub key: Option<PathBuf>,

    /// GitHub REST API base URL (set for GitHub Enterprise).
    #[arg(
        long,
        env = "GITHUB_API_URL",
        default_value = "https://api.github.com",
        value_name = "URL"
    )]
    pub api_url: String,
}

#[derive(Debug, Args)]
pub struct TokenArgs {
    #[command(flatten)]
    pub auth: AppAuthArgs,

    /// Installation ID to mint a token for (discover via `installations`).
    #[arg(long, env = "GITHUB_APP_INSTALLATION_ID", value_name = "ID")]
    pub installation_id: String,
}

#[derive(Debug, Args)]
pub struct InstallationsArgs {
    #[command(flatten)]
    pub auth: AppAuthArgs,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    /// Shell to emit a completion script for.
    #[arg(value_enum)]
    pub shell: CompletionShell,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn installations_rejects_installation_id_flag() {
        // `--installation-id` belongs to `token` only; unknown here regardless
        // of environment. Required auth is supplied explicitly so the failure is
        // unambiguously the unknown flag, not a missing env-backed arg.
        let parsed = Cli::try_parse_from([
            "github-app-cli",
            "installations",
            "--app-id",
            "1",
            "--key",
            "k.pem",
            "--installation-id",
            "2",
        ]);
        assert!(parsed.is_err());
    }

    #[test]
    fn token_parses_with_all_required_args() {
        let parsed = Cli::try_parse_from([
            "github-app-cli",
            "token",
            "--app-id",
            "1",
            "--key",
            "k.pem",
            "--installation-id",
            "2",
        ]);
        assert!(parsed.is_ok());
    }

    #[test]
    fn completion_does_not_require_auth() {
        let parsed = Cli::try_parse_from(["github-app-cli", "completion", "zsh"]);
        assert!(parsed.is_ok());
    }
}
