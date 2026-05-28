use clap::{Parser, Subcommand};
use nils_common::cli_contract::{
    Envelope, OutputFormat, emit_parse_error, exit, schema_version_for,
};
use nils_term::progress::{Progress, ProgressFinish, ProgressOptions};
use serde::Serialize;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

const BINARY: &str = "cli-template";

#[derive(Parser)]
#[command(
    name = "cli-template",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Template CLI for nils-cli workspace"
)]
struct Cli {
    /// Log level (e.g. trace, debug, info, warn, error)
    #[arg(long, default_value = "info", global = true)]
    log_level: String,

    /// Output format (defaults to text).
    #[arg(long, global = true, value_enum)]
    format: Option<OutputFormat>,

    /// Hidden alias for `--format json` (kept for backwards compatibility).
    #[arg(long, global = true, hide = true, conflicts_with = "format")]
    json: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

impl Cli {
    fn output_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            self.format.unwrap_or_default()
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Print a greeting to stdout (text only).
    Hello {
        /// Name to greet (defaults to "world").
        name: Option<String>,
    },
    /// Render a short progress demo (progress on stderr, stdout stays clean).
    ProgressDemo,
    /// Emit a structured status envelope (text or JSON).
    Status,
}

#[derive(Serialize)]
struct StatusPayload {
    binary: &'static str,
    version: &'static str,
}

fn init_tracing(level: &str) {
    let filter = EnvFilter::try_new(level)
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));

    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn detect_format_from_argv() -> OutputFormat {
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--json" {
            return OutputFormat::Json;
        }
        if arg == "--format"
            && let Some(next) = iter.next()
            && next.eq_ignore_ascii_case("json")
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
    let rendered = err.to_string();
    rendered
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

fn parse_or_exit() -> Cli {
    match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            use clap::error::ErrorKind;
            let kind = err.kind();
            if matches!(
                kind,
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                err.exit();
            }

            let format = detect_format_from_argv();
            let code = match kind {
                ErrorKind::InvalidSubcommand => "unknown-subcommand",
                _ => "parse-error",
            };
            let message = render_clap_message(&err);
            let exit_code = emit_parse_error(BINARY, format, code, &message);
            std::process::exit(exit_code);
        }
    }
}

fn emit_status(format: OutputFormat) -> i32 {
    let payload = StatusPayload {
        binary: "cli-template",
        version: env!("CARGO_PKG_VERSION"),
    };
    match format {
        OutputFormat::Json => {
            let envelope = Envelope::success(schema_version_for(BINARY, "status", 1), payload);
            match serde_json::to_string(&envelope) {
                Ok(serialized) => {
                    println!("{serialized}");
                    exit::SUCCESS
                }
                Err(_) => exit::SOFTWARE,
            }
        }
        OutputFormat::Text => {
            println!("{} {}", payload.binary, payload.version);
            exit::SUCCESS
        }
    }
}

fn main() {
    let cli = parse_or_exit();
    init_tracing(&cli.log_level);
    let format = cli.output_format();

    let exit_code = match cli.command {
        Some(Command::Hello { name }) => {
            let name = name.unwrap_or_else(|| "world".to_string());
            let greeting = nils_common::greeting(&name);
            info!(%greeting, "generated greeting");
            println!("{greeting}");
            exit::SUCCESS
        }
        Some(Command::ProgressDemo) => {
            let progress = Progress::new(
                10,
                ProgressOptions::default()
                    .with_prefix("demo ")
                    .with_finish(ProgressFinish::Clear),
            );

            for i in 0..10_u64 {
                progress.set_message(format!("step {} of 10", i + 1));
                progress.inc(1);
                std::thread::sleep(std::time::Duration::from_millis(30));
            }

            progress.finish_and_clear();
            println!("done");
            exit::SUCCESS
        }
        Some(Command::Status) => emit_status(format),
        None => {
            info!("no subcommand selected");
            exit::SUCCESS
        }
    };

    std::process::exit(exit_code);
}
