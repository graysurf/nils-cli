use std::ffi::OsString;
use std::io::{Read, Write};

use clap::{Args, Parser, Subcommand};

use api_testing_core::cli_contract::{Envelope, handle_parse_error};
use api_testing_core::cli_util;
use api_testing_core::suite::filter::parse_csv_list;
use api_testing_core::suite::resolve::{
    find_repo_root, resolve_path_from_repo_root, resolve_suite_selection,
};
use api_testing_core::suite::runner::{SuiteRunOptions, run_suite};
use api_testing_core::suite::schema::load_and_validate_suite;
use api_testing_core::suite::summary::{SummaryOptions, render_summary_from_json_str};
use nils_term::progress::{Progress, ProgressEnabled, ProgressOptions};

mod completion;

const ROOT_AFTER_HELP: &str = "\
EXAMPLES:
  api-test run --suite smoke --out results.json
  api-test summary --in results.json --out summary.md
  api-test completion zsh

ENVIRONMENT:
  API_TEST_PROGRESS
  API_TEST_OUTPUT_DIR
  API_TEST_ALLOW_WRITES_ENABLED
  API_TEST_REST_URL, API_TEST_GQL_URL, API_TEST_GRPC_URL, API_TEST_WS_URL
  API_TEST_AUTH_JSON
  GITHUB_STEP_SUMMARY

EXIT CODES:
  0   success
  1   runtime error
  64  command-line usage error
  65  invalid input data";

#[derive(Parser)]
#[command(
    name = "api-test",
    version,
    about = "API suite runner (run + summary)",
    long_about = "Run API suite manifests, write structured results, and render Markdown summaries. Bare run flags are treated as the `run` subcommand.",
    after_help = ROOT_AFTER_HELP,
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run a suite (default)
    Run(RunArgs),
    /// Render a Markdown summary from results JSON
    Summary(SummaryArgs),
    /// Print shell completion script
    Completion(CompletionArgs),
}

#[derive(Args)]
struct CompletionArgs {
    /// Shell to generate completion for
    #[arg(value_enum)]
    shell: completion::CompletionShell,
}

#[derive(Args)]
struct RunArgs {
    /// Resolve suite under tests/api/suites/<name>.suite.json (fallback: setup/api/suites)
    #[arg(
        long = "suite",
        conflicts_with = "suite_file",
        required_unless_present = "suite_file"
    )]
    suite: Option<String>,

    /// Explicit suite file path (overrides canonical path)
    #[arg(
        long = "suite-file",
        conflicts_with = "suite",
        required_unless_present = "suite"
    )]
    suite_file: Option<String>,

    /// Write JSON results to a file (stdout always emits JSON)
    #[arg(long = "out")]
    out: Option<String>,

    /// Write JUnit XML to a file (optional)
    #[arg(long = "junit")]
    junit: Option<String>,

    /// Allow write-capable cases (still requires allowWrite: true in the case)
    #[arg(long = "allow-writes")]
    allow_writes: bool,

    /// Run only cases that include this tag (repeatable; AND semantics)
    #[arg(long = "tag")]
    tags: Vec<String>,

    /// Run only the listed case IDs (comma-separated)
    #[arg(long = "only")]
    only: Option<String>,

    /// Skip the listed case IDs (comma-separated)
    #[arg(long = "skip")]
    skip: Option<String>,

    /// Stop after the first failed case
    #[arg(long = "fail-fast", conflicts_with = "continue_")]
    fail_fast: bool,

    /// Continue after failures (default)
    #[arg(long = "continue", conflicts_with = "fail_fast")]
    continue_: bool,
}

#[derive(Args)]
struct SummaryArgs {
    /// Input results JSON file path (default: stdin)
    #[arg(long = "in")]
    input: Option<String>,

    /// Write Markdown summary to a file (optional)
    #[arg(long = "out")]
    out: Option<String>,

    /// Show slowest N executed cases (default: 5)
    #[arg(long = "slow")]
    slow: Option<u32>,

    /// Do not show skipped cases list
    #[arg(long = "hide-skipped")]
    hide_skipped: bool,

    /// Max failed cases to print (default: 50)
    #[arg(long = "max-failed")]
    max_failed: Option<u32>,

    /// Max skipped cases to print (default: 50)
    #[arg(long = "max-skipped")]
    max_skipped: Option<u32>,

    /// Do not write to $GITHUB_STEP_SUMMARY
    #[arg(long = "no-github-summary")]
    no_github_summary: bool,
}

fn argv_with_default_command(raw_args: &[String]) -> Vec<String> {
    let mut argv = vec!["api-test".to_string()];
    if raw_args.is_empty() {
        return argv;
    }

    let first = raw_args[0].as_str();
    let is_root_help = first == "-h" || first == "--help";
    let is_root_version = first == "-V" || first == "--version";

    let is_explicit_command = matches!(first, "run" | "summary" | "completion");
    if !is_explicit_command && !is_root_help && !is_root_version {
        argv.push("run".to_string());
    }

    argv.extend_from_slice(raw_args);
    argv
}

fn print_root_help() {
    println!("API suite runner with implicit `run` default for suite execution.");
    println!();
    println!("Usage: api-test <command> [args]");
    println!();
    println!("Commands:");
    println!("  run      Run a suite (default)");
    println!("  summary  Render a Markdown summary from results JSON");
    println!("  completion  Print shell completion script");
    println!();
    println!("Common options (run; see `api-test run --help` for full details):");
    println!("  --suite <name>        Resolve suite under tests/api/suites/<name>.suite.json");
    println!("  --suite-file <path>   Explicit suite file path");
    println!("  --tag <tag>           Filter cases by tag (repeatable; AND semantics)");
    println!("  --only <csv>          Run only listed case IDs (comma-separated)");
    println!("  --skip <csv>          Skip listed case IDs (comma-separated)");
    println!("  --fail-fast           Stop after first failure");
    println!("  --out <path>          Write results JSON to a file (stdout still emits JSON)");
    println!("  --junit <path>        Write optional JUnit XML to a file");
    println!("  -h, --help            Print help");
    println!("  -V, --version         Print version");
    println!();
    println!("EXAMPLES:");
    println!("  api-test --help");
    println!("  api-test --suite smoke --help");
    println!("  api-test run --suite smoke --out results.json");
    println!("  api-test summary --in results.json --out summary.md");
    println!("  api-test completion zsh");
    println!();
    println!("ENVIRONMENT:");
    println!("  API_TEST_PROGRESS");
    println!("  API_TEST_OUTPUT_DIR");
    println!("  API_TEST_ALLOW_WRITES_ENABLED");
    println!("  API_TEST_REST_URL, API_TEST_GQL_URL, API_TEST_GRPC_URL, API_TEST_WS_URL");
    println!("  API_TEST_AUTH_JSON");
    println!("  GITHUB_STEP_SUMMARY");
    println!();
    println!("EXIT CODES:");
    println!("  0   success");
    println!("  1   runtime error");
    println!("  64  command-line usage error");
    println!("  65  invalid input data");
}

fn progress_enabled_from_env(raw: Option<&str>) -> ProgressEnabled {
    let Some(value) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return ProgressEnabled::Auto;
    };

    if value.eq_ignore_ascii_case("auto") {
        return ProgressEnabled::Auto;
    }

    if value.eq_ignore_ascii_case("on")
        || value == "1"
        || value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("yes")
    {
        return ProgressEnabled::On;
    }

    if value.eq_ignore_ascii_case("off")
        || value == "0"
        || value.eq_ignore_ascii_case("false")
        || value.eq_ignore_ascii_case("no")
    {
        return ProgressEnabled::Off;
    }

    ProgressEnabled::Auto
}

fn suite_progress(total_cases: usize) -> Progress {
    let enabled = progress_enabled_from_env(std::env::var("API_TEST_PROGRESS").ok().as_deref());
    Progress::new(
        total_cases as u64,
        ProgressOptions::default()
            .with_prefix("api-test ")
            .with_enabled(enabled),
    )
}

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let is_root_help = raw_args.len() == 1 && (raw_args[0] == "-h" || raw_args[0] == "--help");
    if raw_args.is_empty() || is_root_help {
        print_root_help();
        return 0;
    }

    let argv = argv_with_default_command(&raw_args);
    let argv_os: Vec<OsString> = argv.iter().map(OsString::from).collect();

    let cli = match Cli::try_parse_from(argv_os.iter()) {
        Ok(v) => v,
        Err(err) => return handle_parse_error("api-test", argv_os, err),
    };

    match cli.command {
        None => {
            print_root_help();
            0
        }
        Some(Command::Run(args)) => cmd_run(&args),
        Some(Command::Summary(args)) => cmd_summary(&args),
        Some(Command::Completion(args)) => completion::run(args.shell),
    }
}

fn cmd_run(args: &RunArgs) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let repo_root = match find_repo_root(&cwd) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };

    let sel = match resolve_suite_selection(
        &repo_root,
        args.suite.as_deref(),
        args.suite_file.as_deref(),
    ) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };

    let loaded = match load_and_validate_suite(&sel.suite_path) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };

    let progress = suite_progress(loaded.manifest.cases.len());

    let out_dir_base_raw = std::env::var("API_TEST_OUTPUT_DIR")
        .ok()
        .unwrap_or_default();
    let out_dir_base = out_dir_base_raw.trim();
    let output_dir_base = if out_dir_base.is_empty() {
        repo_root.join("out/api-test-runner")
    } else {
        resolve_path_from_repo_root(&repo_root, out_dir_base)
    };

    let mut stderr = std::io::stderr().lock();
    let stderr_writer: &mut dyn std::io::Write = &mut stderr;
    let allow_writes_env = cli_util::bool_from_env(
        std::env::var("API_TEST_ALLOW_WRITES_ENABLED").ok(),
        "API_TEST_ALLOW_WRITES_ENABLED",
        false,
        Some("api-test"),
        stderr_writer,
    );
    let allow_writes_flag = args.allow_writes || allow_writes_env;

    let env_rest_url = std::env::var("API_TEST_REST_URL").ok().unwrap_or_default();
    let env_gql_url = std::env::var("API_TEST_GQL_URL").ok().unwrap_or_default();
    let env_grpc_url = std::env::var("API_TEST_GRPC_URL").ok().unwrap_or_default();
    let env_ws_url = std::env::var("API_TEST_WS_URL").ok().unwrap_or_default();

    let only_ids: std::collections::HashSet<String> = args
        .only
        .as_deref()
        .map(parse_csv_list)
        .unwrap_or_default()
        .into_iter()
        .collect();
    let skip_ids: std::collections::HashSet<String> = args
        .skip
        .as_deref()
        .map(parse_csv_list)
        .unwrap_or_default()
        .into_iter()
        .collect();

    let opts = SuiteRunOptions {
        required_tags: args.tags.clone(),
        only_ids,
        skip_ids,
        allow_writes_flag,
        fail_fast: args.fail_fast,
        output_dir_base,
        env_rest_url,
        env_gql_url,
        env_grpc_url,
        env_ws_url,
        progress: Some(progress),
    };

    let run_output = match run_suite(&repo_root, loaded, opts) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("{err:#}");
            return 1;
        }
    };

    // Wrap suite results in the shared CLI envelope; both stdout and the
    // optional `--out` file emit the byte-identical envelope so consumers see
    // one shape (`render_summary_from_json_str` transparently unwraps the
    // envelope so old summary callers keep working).
    let envelope = Envelope::success("cli.api-test.run.v1", &run_output.results);
    let json_line = match serde_json::to_string(&envelope) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("error: failed to serialize results JSON: {err}");
            return 1;
        }
    };

    if let Some(out_path_raw) = args.out.as_deref() {
        let out_path_abs = resolve_path_from_repo_root(&repo_root, out_path_raw);
        if let Some(parent) = out_path_abs.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&out_path_abs, format!("{json_line}\n")) {
            eprintln!("error: failed to write --out file: {err}");
            return 1;
        }
    }

    if let Some(junit_path_raw) = args.junit.as_deref() {
        let junit_path_abs = resolve_path_from_repo_root(&repo_root, junit_path_raw);
        if let Err(err) =
            api_testing_core::suite::junit::write_junit_file(&run_output.results, &junit_path_abs)
        {
            eprintln!("error: failed to write junit file: {err:#}");
            return 1;
        }
    }

    eprintln!(
        "api-test-runner: suite={} total={} passed={} failed={} skipped={} outputDir={}",
        run_output.results.suite,
        run_output.results.summary.total,
        run_output.results.summary.passed,
        run_output.results.summary.failed,
        run_output.results.summary.skipped,
        run_output.results.output_dir
    );

    println!("{json_line}");

    run_output.results.exit_code()
}

fn cmd_summary(args: &SummaryArgs) -> i32 {
    let raw = if let Some(path) = args.input.as_deref() {
        std::fs::read_to_string(path).unwrap_or_default()
    } else {
        let mut buf = String::new();
        let _ = std::io::stdin().read_to_string(&mut buf);
        buf
    };

    let opts = SummaryOptions {
        slow_n: args.slow.unwrap_or(5).try_into().unwrap_or(5),
        hide_skipped: args.hide_skipped,
        max_failed: args.max_failed.unwrap_or(50).try_into().unwrap_or(50),
        max_skipped: args.max_skipped.unwrap_or(50).try_into().unwrap_or(50),
    };

    let md = render_summary_from_json_str(raw.trim(), args.input.as_deref(), &opts);

    if let Some(out_path) = args.out.as_deref() {
        if let Some(parent) = std::path::Path::new(out_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(out_path, &md) {
            eprintln!("error: failed to write --out file: {err}");
            return 1;
        }
    }

    if !args.no_github_summary
        && let Ok(path) = std::env::var("GITHUB_STEP_SUMMARY")
    {
        let path = path.trim();
        if !path.is_empty() {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| writeln!(f, "\n{md}\n"));
        }
    }

    print!("{md}");
    0
}

#[cfg(test)]
mod tests {
    use super::{argv_with_default_command, progress_enabled_from_env};
    use nils_term::progress::ProgressEnabled;
    use pretty_assertions::assert_eq;

    #[test]
    fn argv_with_default_command_inserts_run_except_for_explicit_commands() {
        let argv = argv_with_default_command(&[]);
        assert_eq!(argv, vec!["api-test".to_string()]);

        let argv = argv_with_default_command(&["--help".to_string()]);
        assert_eq!(argv, vec!["api-test".to_string(), "--help".to_string()]);

        let argv = argv_with_default_command(&["summary".to_string()]);
        assert_eq!(argv, vec!["api-test".to_string(), "summary".to_string()]);

        let argv = argv_with_default_command(&["completion".to_string(), "zsh".to_string()]);
        assert_eq!(
            argv,
            vec![
                "api-test".to_string(),
                "completion".to_string(),
                "zsh".to_string()
            ]
        );

        let argv = argv_with_default_command(&["--suite".to_string(), "smoke".to_string()]);
        assert_eq!(
            argv,
            vec![
                "api-test".to_string(),
                "run".to_string(),
                "--suite".to_string(),
                "smoke".to_string()
            ]
        );
    }

    #[test]
    fn progress_enabled_from_env_parses_auto_on_off_aliases() {
        assert_eq!(progress_enabled_from_env(None), ProgressEnabled::Auto);
        assert_eq!(progress_enabled_from_env(Some("")), ProgressEnabled::Auto);
        assert_eq!(
            progress_enabled_from_env(Some("auto")),
            ProgressEnabled::Auto
        );
        assert_eq!(
            progress_enabled_from_env(Some("AUTO")),
            ProgressEnabled::Auto
        );

        assert_eq!(progress_enabled_from_env(Some("on")), ProgressEnabled::On);
        assert_eq!(progress_enabled_from_env(Some("1")), ProgressEnabled::On);
        assert_eq!(progress_enabled_from_env(Some("true")), ProgressEnabled::On);
        assert_eq!(progress_enabled_from_env(Some("yes")), ProgressEnabled::On);

        assert_eq!(progress_enabled_from_env(Some("off")), ProgressEnabled::Off);
        assert_eq!(progress_enabled_from_env(Some("0")), ProgressEnabled::Off);
        assert_eq!(
            progress_enabled_from_env(Some("false")),
            ProgressEnabled::Off
        );
        assert_eq!(progress_enabled_from_env(Some("no")), ProgressEnabled::Off);

        assert_eq!(
            progress_enabled_from_env(Some("unexpected")),
            ProgressEnabled::Auto
        );
    }
}
