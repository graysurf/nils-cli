use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use nils_common::cli_contract::OutputFormat;

/// Render a `.md.tera` template against a JSON data file using the
/// shared `nils_markdown::Engine`. The deterministic posture (no
/// autoescape, no `now()`, registered template name = template file
/// stem) matches the workspace's Tier-A migration golden harness.
#[derive(Debug, Parser)]
#[command(
    name = "md-render",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about,
    long_about = None
)]
pub struct Cli {
    /// Output format envelope. `text` writes rendered Markdown to
    /// stdout; `json` wraps the rendered body in the
    /// `cli.md-render.render.v1` envelope.
    #[arg(long, global = true, value_enum)]
    pub format: Option<OutputFormat>,

    #[command(subcommand)]
    pub command: Option<Command>,

    /// Convenience: when no subcommand is given, treat the remaining
    /// flags as a `render` invocation. This keeps the most common
    /// usage (`md-render --template ... --data ...`) short.
    #[command(flatten)]
    pub default_render: RenderArgs,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Render a template against a JSON data file (explicit form).
    Render(RenderArgs),
    /// Emit shell completion script (bash or zsh) to stdout.
    Completion {
        /// Shell to emit completion for.
        #[arg(value_enum)]
        shell: CompletionShell,
    },
}

#[derive(Debug, Clone, clap::Args)]
pub struct RenderArgs {
    /// Path to the `.md.tera` template file.
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    pub template: Option<PathBuf>,

    /// Path to the JSON data file. The contents are passed to
    /// `Engine::render_value` as the template's view.
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    pub data: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum CompletionShell {
    Bash,
    Zsh,
}
