//! `evidence` CLI dispatcher.
//!
//! Wires the clap command tree to the per-command modules and wraps every
//! `--format json` response in a `nils_common` Envelope whose schema_version
//! is `cli.evidence.<command>.v1` (built by `schema_version_for("evidence",
//! command, 1)`).
//!
//! Command set:
//! - `migrate`  — batch dry-run / apply of skill-usage rollups into the archive
//! - `discover` — read-only scan of the agent-out tree for archivable records
//! - `catalog`  — generate / filter the derived `evidence.catalog.v1`
//! - `query`    — filtered read over archived rollups (cross-version aware)
//! - `search`   — substring matcher over catalog rows' intent + summary
//! - `validate-hosts` / `validate-local` / `validate-record` — schema checks
//! - `completion` — clap_complete export

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
    record::RecordValidation,
};

const BINARY: &str = "evidence";

#[derive(Parser)]
#[command(
    name = BINARY,
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Evidence archive CLI for the nils-cli workspace"
)]
struct Cli {
    /// Output format (defaults to text).
    #[arg(long, global = true, value_enum)]
    format: Option<OutputFormat>,

    /// Hidden alias for `--format json` kept for symmetry with neighbouring
    /// CLIs.
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
    /// Validate a machine-local `agent-evidence-archive/config.yaml` document.
    /// Missing files return documented defaults with exit code 0.
    ValidateLocal {
        /// Path to the local config file. Missing files are allowed.
        #[arg(long)]
        input: String,
    },
    /// Validate an archived `skill-usage.rollup.json` document.
    ValidateRecord {
        /// Path to the rollup JSON file to validate. Use `-` for stdin.
        #[arg(long)]
        input: String,
    },

    /// Migrate skill-usage records out of the agent-out tree into the archive.
    /// Dry-run by default; use `--apply` to write, commit, and push.
    Migrate {
        /// Agent-out projects root. Defaults to `${AGENT_HOME}/out/projects`.
        #[arg(long)]
        source_out: Option<PathBuf>,
        /// Archive clone path. Defaults to `$AGENT_EVIDENCE_ARCHIVE_HOME`,
        /// the local config, or the XDG data-home default.
        #[arg(long)]
        archive: Option<PathBuf>,
        /// Path to the archive `config/hosts.yaml`. Defaults to
        /// `<archive>/config/hosts.yaml`.
        #[arg(long)]
        hosts: Option<PathBuf>,
        /// Only migrate records for this repo (`<owner__repo>` slug or repo
        /// name).
        #[arg(long)]
        repo: Option<String>,
        /// Only migrate records whose skill path/id contains this substring.
        #[arg(long)]
        skill: Option<String>,
        /// Only records started on or after this `YYYY-MM-DD`.
        #[arg(long)]
        since: Option<String>,
        /// Only records started on or before this `YYYY-MM-DD`.
        #[arg(long)]
        until: Option<String>,
        /// Only records that link a heuristic-inbox promotion case.
        #[arg(long)]
        promotion_only: bool,
        /// Host (FQDN) to attribute slug-only records to (e.g.
        /// `github.com`, `gitlab.gamania.com`). The agent-out `<owner__repo>`
        /// slug carries no host; under a multi-host `config/hosts.yaml` this
        /// override pins the host the operator vouches for. Must be present in
        /// `config/hosts.yaml`; records whose dir is not an `<owner__repo>`
        /// slug, or whose host is absent from the config, are reported as
        /// blocked.
        #[arg(long)]
        host: Option<String>,
        /// Apply the migration. Without this flag the command runs in dry-run
        /// mode.
        #[arg(long)]
        apply: bool,
    },
    /// Read-only scan of the agent-out tree for archivable records.
    Discover {
        /// Agent-out projects root. Defaults to `${AGENT_HOME}/out/projects`.
        #[arg(long)]
        source_out: Option<PathBuf>,
        /// Archive clone path. Defaults as for `migrate`.
        #[arg(long)]
        archive: Option<PathBuf>,
        /// Include `unknown` (unreadable/unparseable) candidates.
        #[arg(long)]
        include_unknown: bool,
    },
    /// Generate, write, or filter the derived archive catalog.
    Catalog {
        /// Write deterministic `<archive>/catalog.json`.
        #[arg(long)]
        write: bool,
        /// Filter records by case-insensitive substring.
        #[arg(long)]
        grep: Option<String>,
        /// No-op for catalog, kept for CLI parity: `--grep` already matches
        /// full-text intent + outcome summary, so this does not change results.
        #[arg(long)]
        deep: bool,
        /// Filter records by exact outcome status (case-insensitive).
        #[arg(long)]
        outcome: Option<String>,
        /// Filter records by promotion case id substring.
        #[arg(long = "case-id")]
        case_id: Option<String>,
        /// Archive clone path. Defaults as for `migrate`.
        #[arg(long)]
        archive: Option<PathBuf>,
    },
    /// Filtered read over archived rollups (cross-version aware).
    Query {
        /// Filter by skill path/id substring.
        #[arg(long)]
        skill: Option<String>,
        /// Filter by exact outcome status (case-insensitive).
        #[arg(long)]
        outcome: Option<String>,
        /// Filter by repo slug.
        #[arg(long)]
        repo: Option<String>,
        /// Filter by host FQDN.
        #[arg(long)]
        host: Option<String>,
        /// Filter by org / GitLab group path.
        #[arg(long)]
        org: Option<String>,
        /// Only records started on or after this `YYYY-MM-DD`.
        #[arg(long)]
        since: Option<String>,
        /// Only records started on or before this `YYYY-MM-DD`.
        #[arg(long)]
        until: Option<String>,
        /// Archive clone path. Defaults as for `migrate`.
        #[arg(long)]
        archive: Option<PathBuf>,
    },
    /// Substring search over catalog rows' intent + outcome summary.
    Search {
        /// Case-insensitive term to match.
        term: String,
        /// Archive clone path. Defaults as for `migrate`.
        #[arg(long)]
        archive: Option<PathBuf>,
    },
    /// Print a shell completion script for `evidence`.
    Completion {
        #[arg(value_enum)]
        shell: crate::completion::CompletionShell,
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
            return emit_parse_error(BINARY, format, code, &message);
        }
    };

    let format = cli.output_format();
    match cli.command {
        Command::ValidateHosts { input } => dispatch_hosts(&input, format),
        Command::ValidateLocal { input } => dispatch_local(&input, format),
        Command::ValidateRecord { input } => dispatch_record(&input, format),
        Command::Completion { shell } => crate::completion::run(shell),
        Command::Migrate {
            source_out,
            archive,
            hosts,
            repo,
            skill,
            since,
            until,
            promotion_only,
            host,
            apply,
        } => crate::migrate::dispatch(crate::migrate::DispatchArgs {
            source_out,
            archive,
            hosts,
            repo,
            skill,
            since,
            until,
            promotion_only,
            host,
            apply,
            working_repo_roots: crate::source::resolve_working_repo_roots(),
            format,
        }),
        Command::Discover {
            source_out,
            archive,
            include_unknown,
        } => crate::discover::dispatch(crate::discover::DispatchArgs {
            source_out,
            archive,
            include_unknown,
            format,
        }),
        Command::Catalog {
            write,
            grep,
            deep,
            outcome,
            case_id,
            archive,
        } => crate::catalog::dispatch(crate::catalog::DispatchArgs {
            write,
            grep,
            deep,
            outcome,
            case_id,
            archive,
            format,
        }),
        Command::Query {
            skill,
            outcome,
            repo,
            host,
            org,
            since,
            until,
            archive,
        } => crate::query::dispatch(crate::query::DispatchArgs {
            skill,
            outcome,
            repo,
            host,
            org,
            since,
            until,
            archive,
            format,
        }),
        Command::Search { term, archive } => crate::search::dispatch(crate::search::DispatchArgs {
            term,
            archive,
            format,
        }),
    }
}

/// Hand the clap-derived `Command` to other modules (used by the completion
/// generator).
pub fn cli_command() -> clap::Command {
    Cli::command()
}

/// Stable, namespaced error code for a validate-command IO failure (reading
/// the input file or stdin). Consistent with the `migrate-*` / `query-*`
/// family of namespaced codes elsewhere in the crate.
const VALIDATE_IO_CODE: &str = "evidence-validate-io";

fn dispatch_hosts(input: &str, format: OutputFormat) -> i32 {
    let raw = match load_input("hosts", input) {
        Ok(raw) => raw,
        Err(err) => return emit_error(format, "validate-hosts", VALIDATE_IO_CODE, &err),
    };
    match validate::hosts::validate_hosts_yaml(&raw) {
        Ok(v) => emit_hosts_success(v, format),
        Err(err) => emit_error(format, "validate-hosts", err.code(), &err.to_string()),
    }
}

fn dispatch_local(input: &str, format: OutputFormat) -> i32 {
    let validation = if input == "-" {
        let raw = match read_stdin() {
            Ok(raw) => raw,
            Err(err) => return emit_error(format, "validate-local", VALIDATE_IO_CODE, &err),
        };
        validate::local::validate_local_yaml(&raw)
    } else {
        let path = PathBuf::from(input);
        validate::local::validate_local_path(&path)
    };

    match validation {
        Ok(v) => emit_local_success(v, format),
        Err(err) => emit_error(format, "validate-local", err.code(), &err.to_string()),
    }
}

fn dispatch_record(input: &str, format: OutputFormat) -> i32 {
    let raw = match load_input("record", input) {
        Ok(raw) => raw,
        Err(err) => return emit_error(format, "validate-record", VALIDATE_IO_CODE, &err),
    };
    match validate::record::validate_rollup_yaml(&raw) {
        Ok(v) => emit_record_success(v, format),
        Err(err) => emit_error(format, "validate-record", err.code(), &err.to_string()),
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
                println!("  {host}: class={label}");
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
            println!(
                "  migrate_batch_size: {}",
                v.data.config.performance.migrate_batch_size
            );
            for w in &v.warnings {
                eprintln!("warning [{}]: {}", w.code, w.message);
            }
            exit::SUCCESS
        }
    }
}

fn emit_record_success(v: RecordValidation, format: OutputFormat) -> i32 {
    match format {
        OutputFormat::Json => emit_json("validate-record", &v.data, &v.warnings),
        OutputFormat::Text => {
            println!(
                "rollup: id={} schema={} {}/{}/{} outcome={}",
                v.data.id,
                v.data.schema,
                v.data.host,
                v.data.org,
                v.data.repo,
                v.data.outcome_status
            );
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

fn emit_error(format: OutputFormat, command: &str, code: &str, message: &str) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope: Envelope<()> = Envelope::failure(
                schema_version_for(BINARY, command, 1),
                EnvelopeError::new(code, message),
            );
            if let Ok(s) = serde_json::to_string(&envelope) {
                eprintln!("{s}");
            }
            exit::DATA
        }
        OutputFormat::Text => {
            eprintln!("error [{code}]: {message}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_surface_is_valid() {
        Cli::command().debug_assert();
        let names: Vec<String> = Cli::command()
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        for expected in [
            "migrate",
            "discover",
            "catalog",
            "query",
            "search",
            "validate-hosts",
            "validate-local",
            "validate-record",
            "completion",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "missing subcommand `{expected}` in {names:?}"
            );
        }
    }

    #[test]
    fn binary_name_is_evidence() {
        assert_eq!(Cli::command().get_name(), "evidence");
    }

    #[test]
    fn schema_version_uses_evidence_binary() {
        assert_eq!(
            schema_version_for(BINARY, "migrate", 1),
            "cli.evidence.migrate.v1"
        );
        assert_eq!(
            schema_version_for(BINARY, "query", 1),
            "cli.evidence.query.v1"
        );
    }

    #[test]
    fn migrate_parses_all_flags() {
        let cli = Cli::try_parse_from([
            "evidence",
            "migrate",
            "--source-out",
            "/out",
            "--archive",
            "/arch",
            "--repo",
            "kit",
            "--skill",
            "deliver",
            "--since",
            "2026-06-01",
            "--until",
            "2026-06-30",
            "--promotion-only",
            "--host",
            "gitlab.gamania.com",
            "--apply",
            "--format",
            "json",
        ])
        .expect("migrate parses");
        assert!(matches!(cli.output_format(), OutputFormat::Json));
        match cli.command {
            Command::Migrate {
                source_out,
                archive,
                repo,
                skill,
                since,
                until,
                promotion_only,
                host,
                apply,
                ..
            } => {
                assert_eq!(source_out, Some(PathBuf::from("/out")));
                assert_eq!(archive, Some(PathBuf::from("/arch")));
                assert_eq!(repo.as_deref(), Some("kit"));
                assert_eq!(skill.as_deref(), Some("deliver"));
                assert_eq!(since.as_deref(), Some("2026-06-01"));
                assert_eq!(until.as_deref(), Some("2026-06-30"));
                assert!(promotion_only);
                assert_eq!(host.as_deref(), Some("gitlab.gamania.com"));
                assert!(apply);
            }
            _ => panic!("expected Migrate"),
        }
    }

    #[test]
    fn query_parses_filters() {
        let cli = Cli::try_parse_from([
            "evidence",
            "query",
            "--skill",
            "s",
            "--outcome",
            "pass",
            "--repo",
            "kit",
            "--host",
            "github.com",
            "--org",
            "graysurf",
            "--since",
            "2026-01-01",
        ])
        .expect("query parses");
        assert!(matches!(cli.command, Command::Query { .. }));
    }

    #[test]
    fn json_alias_sets_format() {
        let cli = Cli::try_parse_from(["evidence", "catalog", "--json"]).expect("parses");
        assert!(matches!(cli.output_format(), OutputFormat::Json));
    }
}
