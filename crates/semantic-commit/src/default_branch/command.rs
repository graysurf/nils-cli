use std::path::PathBuf;

use nils_common::default_branch_receipt::DefaultBranchReceipt;
use serde_json::Value;

use crate::commit::DefaultBranchCommitOptions;

use super::git::Git;
use super::{OptionKind, clap_command, option_for_spelling, preflight, receipt, transaction};

const EXIT_ERROR: i32 = 1;
const EXIT_USAGE: i32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputFormat {
    Text,
    Json,
}

struct Options {
    expect_head: String,
    receipt_out: Option<PathBuf>,
    repo: Option<PathBuf>,
    dry_run: bool,
    output_format: OutputFormat,
    commit: DefaultBranchCommitOptions,
}

pub(crate) fn run(args: &[String]) -> i32 {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    let state = match preflight::inspect(
        options.repo.as_deref(),
        &options.expect_head,
        options.receipt_out.as_deref(),
    ) {
        Ok(state) => state,
        Err(message) => return fail(&message),
    };
    let code = options
        .commit
        .run(state.root.clone(), state.head.clone(), options.dry_run);
    if code != 0 {
        return code;
    }
    if options.dry_run {
        return emit_preview(&receipt::preview(&state), options.output_format);
    }

    let git = Git::at(state.root.clone());
    let new_head = match git.stdout(["rev-parse", "--verify", "HEAD^{commit}"]) {
        Ok(value) => value,
        Err(message) => return partial_failure(None, &message),
    };
    let post = match transaction::verify(&state, &new_head) {
        Ok(post) => post,
        Err(message) => return partial_failure(Some(&new_head), &message),
    };
    let result = receipt::final_receipt(&state, new_head.clone(), post);
    let receipt_out = options
        .receipt_out
        .as_deref()
        .expect("mutating default-branch requires a receipt path");
    if let Err(message) = receipt::write(receipt_out, &result) {
        return partial_failure(Some(&new_head), &message);
    }
    emit_final(&result, options.output_format)
}

fn parse_args(args: &[String]) -> Result<Options, i32> {
    let mut expect_head = None;
    let mut receipt_out = None;
    let mut repo = None;
    let mut dry_run = false;
    let mut output_format = OutputFormat::Text;
    let mut output_format_bound = false;
    let mut commit = DefaultBranchCommitOptions::new();
    let mut index = 0;
    while index < args.len() {
        let spelling = args[index].as_str();
        if matches!(
            spelling,
            "--amend"
                | "--allow-empty"
                | "--message-only"
                | "--no-edit"
                | "--message-out"
                | "--no-progress"
                | "--quiet"
        ) {
            eprintln!("error: {spelling} is not supported by default-branch");
            return Err(EXIT_USAGE);
        }
        let Some(option) = option_for_spelling(spelling) else {
            eprintln!("error: unknown argument: {spelling}");
            print_usage(true);
            return Err(EXIT_USAGE);
        };
        match option.kind {
            OptionKind::ExpectHead => {
                reject_duplicate(expect_head.is_some(), "--expect-head")?;
                expect_head = Some(required_value(args, index, spelling)?);
                index += 2;
            }
            OptionKind::ReceiptOut => {
                reject_duplicate(receipt_out.is_some(), "--receipt-out")?;
                receipt_out = Some(PathBuf::from(required_value(args, index, spelling)?));
                index += 2;
            }
            OptionKind::Repo => {
                reject_duplicate(repo.is_some(), "--repo")?;
                repo = Some(PathBuf::from(required_value(args, index, spelling)?));
                index += 2;
            }
            OptionKind::Format => {
                reject_duplicate(output_format_bound, "--format")?;
                let value = required_value(args, index, spelling)?;
                output_format = match value.as_str() {
                    "text" => OutputFormat::Text,
                    "json" => OutputFormat::Json,
                    _ => {
                        eprintln!("error: invalid --format value: {value} (expected: text, json)");
                        return Err(EXIT_USAGE);
                    }
                };
                output_format_bound = true;
                index += 2;
            }
            OptionKind::Json => {
                reject_duplicate(output_format_bound, "--format")?;
                output_format = OutputFormat::Json;
                output_format_bound = true;
                index += 1;
            }
            OptionKind::DryRun => {
                dry_run = true;
                index += 1;
            }
            OptionKind::Help => {
                print_usage(false);
                return Err(0);
            }
            OptionKind::Message
            | OptionKind::MessageFile
            | OptionKind::Automation
            | OptionKind::Type
            | OptionKind::Scope
            | OptionKind::Subject
            | OptionKind::BodyBullet
            | OptionKind::Signoff
            | OptionKind::Trailer
            | OptionKind::AutoFix
            | OptionKind::MaxHeaderWidth => match commit.parse_message_argument(args, index) {
                Ok(Some(next)) => index = next,
                Ok(None) => {
                    eprintln!("error: parser contract rejected known option: {spelling}");
                    return Err(EXIT_USAGE);
                }
                Err(_) => return Err(EXIT_USAGE),
            },
        }
    }
    let expect_head = expect_head.ok_or_else(|| {
        eprintln!("error: --expect-head is required for default-branch");
        EXIT_USAGE
    })?;
    if dry_run && receipt_out.is_some() {
        eprintln!("error: --receipt-out is not accepted with --dry-run");
        return Err(EXIT_USAGE);
    }
    if !dry_run && receipt_out.is_none() {
        eprintln!("error: --receipt-out is required for default-branch");
        return Err(EXIT_USAGE);
    }
    commit.finish().map_err(|_| EXIT_USAGE)?;
    Ok(Options {
        expect_head,
        receipt_out,
        repo,
        dry_run,
        output_format,
        commit,
    })
}

fn reject_duplicate(seen: bool, option: &str) -> Result<(), i32> {
    if seen {
        eprintln!("error: {option} may be provided only once");
        return Err(EXIT_USAGE);
    }
    Ok(())
}

fn required_value(args: &[String], index: usize, option: &str) -> Result<String, i32> {
    args.get(index + 1).cloned().ok_or_else(|| {
        eprintln!("error: {option} requires a value");
        EXIT_USAGE
    })
}

fn emit_preview(preview: &Value, format: OutputFormat) -> i32 {
    match format {
        OutputFormat::Json => emit_json(serde_json::to_string(preview)),
        OutputFormat::Text => {
            println!("default-branch preflight validated; no commit created");
            0
        }
    }
}

fn emit_final(receipt: &DefaultBranchReceipt, format: OutputFormat) -> i32 {
    match format {
        OutputFormat::Json => emit_json(serde_json::to_string(receipt)),
        OutputFormat::Text => {
            println!("default-branch commit created and receipt written");
            0
        }
    }
}

fn emit_json(rendered: serde_json::Result<String>) -> i32 {
    match rendered {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(error) => fail(&format!("failed to render default-branch result: {error}")),
    }
}

fn fail(message: &str) -> i32 {
    eprintln!("error: {message}");
    EXIT_ERROR
}

fn partial_failure(new_head: Option<&str>, message: &str) -> i32 {
    match new_head {
        Some(new_head) => eprintln!(
            "error: default-branch commit {new_head} was created but finalization failed: {message}; inspect the commit and recover manually"
        ),
        None => eprintln!(
            "error: default-branch commit may have been created but HEAD could not be resolved: {message}; inspect the repository and recover manually"
        ),
    }
    EXIT_ERROR
}

fn print_usage(stderr: bool) {
    let usage = clap_command()
        .bin_name("semantic-commit default-branch")
        .render_long_help();
    if stderr {
        eprintln!("{usage}");
    } else {
        println!("{usage}");
    }
}
