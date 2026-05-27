//! `plan-archive` CLI dispatcher.
//!
//! Sprint 1 lands the three schema validator subcommands
//! (`validate-hosts`, `validate-local`, `validate-metadata`). The
//! `migrate`, `refresh`, and `query` subcommands are declared as
//! `unimplemented`-returning placeholders so the CLI surface is
//! discoverable to downstream skills and integration tests, but their
//! bodies land in later sprints (see `agent-runtime-kit`
//! `docs/plans/plan-archive-nils-cli/`).

use std::io::Write;
use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};
use nils_common::cli_contract::{
    Envelope, EnvelopeError, OutputFormat, emit_parse_error, exit, schema_version_for,
};
use serde::Serialize;

use crate::validate::{
    self,
    hosts::HostsValidation,
    local::{LocalSource, LocalValidation},
    metadata::MetadataValidation,
};

const BINARY: &str = "plan-archive";

#[derive(Parser)]
#[command(name = BINARY, version, about = "Plan archive CLI for nils-cli workspace")]
struct Cli {
    /// Output format (defaults to text).
    #[arg(long, global = true, value_enum)]
    format: Option<OutputFormat>,

    /// Hidden alias for `--format json` kept for symmetry with
    /// neighbouring CLIs.
    #[arg(long, global = true, hide = true, conflicts_with = "format")]
    json: bool,

    #[command(subcommand)]
    command: Command,
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
    /// Validate an archive `config/hosts.yaml` document.
    ValidateHosts {
        /// Path to the YAML file to validate. Use `-` for stdin.
        #[arg(long)]
        input: String,
    },
    /// Validate a machine-local `agent-plan-archive/config.yaml` document.
    /// Missing files return documented defaults with exit code 0.
    ValidateLocal {
        /// Path to the local config file. Missing files are allowed.
        #[arg(long)]
        input: String,
    },
    /// Validate an archived plan's `metadata.yaml` document.
    ValidateMetadata {
        /// Path to the YAML file to validate. Use `-` for stdin.
        #[arg(long)]
        input: String,
    },

    /// Migrate a closed plan folder into the archive repo.
    /// Dry-run by default; use `--apply` to write and commit.
    Migrate {
        /// Plan folder relative to the source repo root, e.g.
        /// `docs/plans/2026-05-27-my-plan/`.
        #[arg(long)]
        plan: PathBuf,
        /// Source working repo. Defaults to the current git repo
        /// root.
        #[arg(long)]
        source_repo: Option<PathBuf>,
        /// Archive clone path. Defaults to the machine-local
        /// config's `archive_clone_path`.
        #[arg(long)]
        archive: Option<PathBuf>,
        /// Path to the archive `config/hosts.yaml`. Defaults to
        /// `<archive>/config/hosts.yaml`.
        #[arg(long)]
        hosts: Option<PathBuf>,
        /// Issue URL to record in `metadata.yaml`.
        #[arg(long)]
        issue: Option<String>,
        /// Pull request URL to record in `metadata.yaml`.
        #[arg(long)]
        pr: Option<String>,
        /// Merge request URL to record in `metadata.yaml`.
        #[arg(long)]
        mr: Option<String>,
        /// Apply the migration. Without this flag the command runs in
        /// dry-run mode.
        #[arg(long)]
        apply: bool,
    },
    /// Read-only scan of plan folders for archive candidates.
    /// Classifies each folder as eligible, blocked, or unknown and
    /// suggests a `plan-archive migrate` command for eligible folders.
    /// Never mutates the source or archive repos.
    Discover {
        /// Source working repo. Defaults to the current git repo root.
        #[arg(long)]
        source_repo: Option<PathBuf>,
        /// Plan-folder root, relative to the source repo. Defaults to
        /// `docs/plans`.
        #[arg(long)]
        plans_root: Option<PathBuf>,
        /// Archive clone path. Defaults to the machine-local config's
        /// `archive_clone_path`.
        #[arg(long)]
        archive: Option<PathBuf>,
        /// Path to the archive `config/hosts.yaml`. Defaults to
        /// `<archive>/config/hosts.yaml`.
        #[arg(long)]
        hosts: Option<PathBuf>,
        /// Include `unknown` candidates in the output (default:
        /// eligible + blocked only). Counts always report all three.
        #[arg(long)]
        include_unknown: bool,
    },
    /// Fetch provider payloads and append scrubbed snapshots to
    /// `_index/`. Writes and scrubs but does not commit; the scrub
    /// log (if any) must be reviewed before committing.
    Refresh {
        /// Reference to refresh (issue/PR/MR URL).
        #[arg(long, conflicts_with_all = ["repo", "since"])]
        r#ref: Option<String>,
        /// Refresh every open reference for the given `host/org/repo`.
        #[arg(long, conflicts_with_all = ["ref", "since"])]
        repo: Option<String>,
        /// With `--repo`, only refresh refs updated on or after this
        /// `YYYY-MM-DD` date.
        #[arg(long, requires = "repo", conflicts_with = "ref")]
        since: Option<String>,
        /// Archive clone path. Defaults to the machine-local config's
        /// `archive_clone_path`.
        #[arg(long)]
        archive: Option<PathBuf>,
        /// Path to the archive `config/hosts.yaml`. Defaults to
        /// `<archive>/config/hosts.yaml`.
        #[arg(long)]
        hosts: Option<PathBuf>,
    },
    /// Print a shell completion script for `plan-archive`.
    Completion {
        #[arg(value_enum)]
        shell: crate::completion::CompletionShell,
    },

    /// Read or aggregate cached `_index/` snapshots.
    Query {
        /// Reference to look up (single-ref read).
        #[arg(long, conflicts_with_all = ["plan", "refs_from"])]
        r#ref: Option<String>,
        /// Filter by host FQDN (aggregate mode).
        #[arg(long, conflicts_with_all = ["ref", "plan", "refs_from"])]
        host: Option<String>,
        /// Filter by org or GitLab group path (aggregate mode).
        #[arg(long, conflicts_with_all = ["ref", "plan", "refs_from"])]
        org: Option<String>,
        /// Filter by repo slug (aggregate mode).
        #[arg(long, conflicts_with_all = ["ref", "plan", "refs_from"])]
        repo: Option<String>,
        /// Only snapshots fetched on or after this `YYYY-MM-DD`
        /// (aggregate mode).
        #[arg(long, conflicts_with_all = ["ref", "plan", "refs_from"])]
        since: Option<String>,
        /// Resolve refs from an archived plan path inside the archive
        /// (link traversal: plan → refs).
        #[arg(long, conflicts_with_all = ["ref", "host", "org", "repo", "since", "refs_from"])]
        plan: Option<String>,
        /// Read refs from a `metadata.yaml` path (link traversal:
        /// metadata → refs).
        #[arg(long, conflicts_with_all = ["ref", "host", "org", "repo", "since", "plan"])]
        refs_from: Option<String>,
        /// Archive clone path. Defaults to the machine-local config's
        /// `archive_clone_path`.
        #[arg(long)]
        archive: Option<PathBuf>,
    },
    /// Generate, write, or filter the derived archive catalog.
    Catalog {
        /// Write deterministic `<archive>/catalog.json`.
        #[arg(long)]
        write: bool,
        /// Filter records by case-insensitive substring.
        #[arg(long)]
        grep: Option<String>,
        /// Filter records by area tag.
        #[arg(long)]
        area: Option<String>,
        /// Return plans that reference this issue/PR/MR URL.
        #[arg(long = "refs-to")]
        refs_to: Option<String>,
        /// Archive clone path. Defaults to the machine-local config's
        /// `archive_clone_path`.
        #[arg(long)]
        archive: Option<PathBuf>,
    },
}

pub fn run() -> i32 {
    let cli = match Cli::try_parse() {
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
            return exit_code;
        }
    };

    let format = cli.output_format();
    match cli.command {
        Command::ValidateHosts { input } => dispatch_hosts(&input, format),
        Command::ValidateLocal { input } => dispatch_local(&input, format),
        Command::ValidateMetadata { input } => dispatch_metadata(&input, format),
        Command::Completion { shell } => crate::completion::run(shell),
        Command::Migrate {
            plan,
            source_repo,
            archive,
            hosts,
            issue,
            pr,
            mr,
            apply,
        } => crate::migrate::dispatch(crate::migrate::DispatchArgs {
            plan,
            source_repo,
            archive,
            hosts,
            issue,
            pr,
            mr,
            apply,
            format,
        }),
        Command::Discover {
            source_repo,
            plans_root,
            archive,
            hosts,
            include_unknown,
        } => crate::discover::dispatch(crate::discover::DispatchArgs {
            source_repo,
            plans_root,
            archive,
            hosts,
            include_unknown,
            format,
        }),
        Command::Refresh {
            r#ref,
            repo,
            since,
            archive,
            hosts,
        } => crate::refresh::dispatch(crate::refresh::DispatchArgs {
            r#ref,
            repo,
            since,
            archive,
            hosts,
            format,
        }),
        Command::Query {
            r#ref,
            host,
            org,
            repo,
            since,
            plan,
            refs_from,
            archive,
        } => crate::query::dispatch(crate::query::DispatchArgs {
            r#ref,
            host,
            org,
            repo,
            since,
            plan,
            refs_from,
            archive,
            format,
        }),
        Command::Catalog {
            write,
            grep,
            area,
            refs_to,
            archive,
        } => crate::catalog::dispatch(crate::catalog::DispatchArgs {
            write,
            grep,
            area,
            refs_to,
            archive,
            format,
        }),
    }
}

/// Hand the clap-derived `Command` to other modules (used by the
/// completion generator).
pub fn cli_command() -> clap::Command {
    Cli::command()
}

fn dispatch_hosts(input: &str, format: OutputFormat) -> i32 {
    let raw = match load_input("hosts", input) {
        Ok(raw) => raw,
        Err(err) => return emit_error(format, "validate-hosts", "io-error", &err, None),
    };
    match validate::hosts::validate_hosts_yaml(&raw) {
        Ok(v) => emit_hosts_success(v, format),
        Err(err) => emit_error(format, "validate-hosts", err.code(), &err.to_string(), None),
    }
}

fn dispatch_local(input: &str, format: OutputFormat) -> i32 {
    let validation = if input == "-" {
        let raw = match read_stdin() {
            Ok(raw) => raw,
            Err(err) => return emit_error(format, "validate-local", "io-error", &err, None),
        };
        validate::local::validate_local_yaml(&raw)
    } else {
        let path = PathBuf::from(input);
        validate::local::validate_local_path(&path)
    };

    match validation {
        Ok(v) => emit_local_success(v, format),
        Err(err) => emit_error(format, "validate-local", err.code(), &err.to_string(), None),
    }
}

fn dispatch_metadata(input: &str, format: OutputFormat) -> i32 {
    let raw = match load_input("metadata", input) {
        Ok(raw) => raw,
        Err(err) => return emit_error(format, "validate-metadata", "io-error", &err, None),
    };
    match validate::metadata::validate_metadata_yaml(&raw) {
        Ok(v) => emit_metadata_success(v, format),
        Err(err) => emit_error(
            format,
            "validate-metadata",
            err.code(),
            &err.to_string(),
            None,
        ),
    }
}

fn load_input(label: &str, input: &str) -> Result<String, String> {
    if input == "-" {
        return read_stdin();
    }
    std::fs::read_to_string(input)
        .map_err(|err| format!("failed to read {label} input `{input}`: {err}"))
}

fn read_stdin() -> Result<String, String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|err| format!("failed to read stdin: {err}"))?;
    Ok(buf)
}

fn emit_hosts_success(v: HostsValidation, format: OutputFormat) -> i32 {
    match format {
        OutputFormat::Json => emit_json("validate-hosts", &v.data, &v.warnings),
        OutputFormat::Text => {
            let summary = &v.data.summary;
            println!(
                "hosts: {} entries ({} personal, {} employer)",
                summary.host_count, summary.personal_count, summary.employer_count
            );
            for (host, entry) in &v.data.config.hosts {
                let label = match entry.class {
                    validate::hosts::HostClass::Personal => "personal",
                    validate::hosts::HostClass::Employer => "employer",
                };
                let employer = entry
                    .employer
                    .as_deref()
                    .map(|e| format!(" employer={e}"))
                    .unwrap_or_default();
                let retention = entry
                    .retention
                    .as_deref()
                    .map(|r| format!(" retention={r}"))
                    .unwrap_or_default();
                println!("  {host}: class={label}{employer}{retention}");
            }
            for w in &v.warnings {
                eprintln!("warning [{}]: {}", w.code, w.message);
            }
            exit::SUCCESS
        }
    }
}

fn emit_local_success(v: LocalValidation, format: OutputFormat) -> i32 {
    match format {
        OutputFormat::Json => emit_json("validate-local", &v.data, &v.warnings),
        OutputFormat::Text => {
            let source = match v.data.source {
                LocalSource::Defaults => "defaults",
                LocalSource::File => "file",
            };
            println!(
                "local config: source={source} archive_clone_path={}",
                v.data.config.archive_clone_path.display()
            );
            for root in &v.data.config.working_repo_roots {
                println!("  working_repo_root: {}", root.display());
            }
            println!(
                "  refresh_batch_size: {}",
                v.data.config.performance.refresh_batch_size
            );
            for w in &v.warnings {
                eprintln!("warning [{}]: {}", w.code, w.message);
            }
            exit::SUCCESS
        }
    }
}

fn emit_metadata_success(v: MetadataValidation, format: OutputFormat) -> i32 {
    match format {
        OutputFormat::Json => emit_json("validate-metadata", &v.data, &v.warnings),
        OutputFormat::Text => {
            let s = &v.data.config.source;
            println!(
                "metadata: host={} org_or_group_path={} repo={} branch={} commit={}",
                s.host, s.org_or_group_path, s.repo, s.branch, s.archive_commit
            );
            println!("  original_path: {}", s.original_path);
            if let Some(cls) = &v.data.config.captured_classification {
                let label = match cls.class {
                    validate::hosts::HostClass::Personal => "personal",
                    validate::hosts::HostClass::Employer => "employer",
                };
                println!("  captured_classification: {label}");
            } else {
                println!("  captured_classification: (none — pre-classification plan)");
            }
            for w in &v.warnings {
                eprintln!("warning [{}]: {}", w.code, w.message);
            }
            exit::SUCCESS
        }
    }
}

fn emit_json<T: Serialize>(
    command: &str,
    data: &T,
    warnings: &[validate::ValidationWarning],
) -> i32 {
    let envelope = Envelope::success(schema_version_for(BINARY, command, 1), data).with_warnings(
        warnings
            .iter()
            .map(|w| format!("[{}] {}", w.code, w.message)),
    );
    match serde_json::to_string(&envelope) {
        Ok(s) => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            if writeln!(handle, "{s}").is_err() {
                return exit::SOFTWARE;
            }
            exit::SUCCESS
        }
        Err(_) => exit::SOFTWARE,
    }
}

fn emit_error(
    format: OutputFormat,
    command: &str,
    code: &str,
    message: &str,
    hint: Option<&str>,
) -> i32 {
    match format {
        OutputFormat::Json => {
            let mut err = EnvelopeError::new(code, message);
            if let Some(h) = hint {
                err = err.with_hint(h);
            }
            let envelope: Envelope<()> =
                Envelope::failure(schema_version_for(BINARY, command, 1), err);
            if let Ok(s) = serde_json::to_string(&envelope) {
                eprintln!("{s}");
            }
            exit::DATA
        }
        OutputFormat::Text => {
            eprintln!("error [{code}]: {message}");
            if let Some(h) = hint {
                eprintln!("hint: {h}");
            }
            exit::DATA
        }
    }
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
