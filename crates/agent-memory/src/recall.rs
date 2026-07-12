//! Bounded startup, curated on-demand, and untrusted candidate recall.

use std::fs;

use serde_json::json;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use nils_common::fs::display_path;

use crate::cli::{
    RecallArgs, RecallCandidatesArgs, RecallCommand, RecallOnDemandArgs, RecallStartupArgs,
};
use crate::{CliError, EXIT_OK, EXIT_RUNTIME, Layout, markdown_files};

pub(crate) fn run(layout: &Layout, args: &RecallArgs) -> Result<i32, CliError> {
    match &args.command {
        RecallCommand::Startup(args) => startup(layout, args),
        RecallCommand::OnDemand(args) => on_demand(layout, args),
        RecallCommand::Candidates(args) => candidates(layout, args),
    }
}

fn startup(layout: &Layout, args: &RecallStartupArgs) -> Result<i32, CliError> {
    let profile = layout.profiles_dir().join("startup");
    let profile_metadata = fs::symlink_metadata(&profile).map_err(|err| {
        CliError::runtime(format!(
            "startup profile directory not found at {}: {err}",
            display_path(&profile)
        ))
    })?;
    if profile_metadata.file_type().is_symlink() || !profile_metadata.is_dir() {
        return Err(CliError::runtime(format!(
            "startup profile must be a regular, non-symlink directory: {}",
            display_path(&profile)
        )));
    }
    let index = profile.join("MEMORY.md");
    let metadata = fs::symlink_metadata(&index).map_err(|err| {
        CliError::runtime(format!(
            "startup profile not found at {}: {err}",
            display_path(&index)
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CliError::runtime(format!(
            "startup profile must be a regular, non-symlink file: {}",
            display_path(&index)
        )));
    }
    let contents = fs::read_to_string(&index).map_err(|err| {
        CliError::runtime(format!("failed to read {}: {err}", display_path(&index)))
    })?;
    let bytes = contents.len();
    if bytes > args.max_bytes {
        return Err(CliError::runtime(format!(
            "startup profile is {bytes} bytes and exceeds {} bytes",
            args.max_bytes
        )));
    }

    let format = output_format(args.format, args.json);
    if format.is_json() {
        let doc = json!({
            "schema_version": schema_version_for("agent-memory", "recall-startup", 1),
            "ok": true,
            "profile": "startup",
            "trust": "untrusted",
            "bytes": bytes,
            "max_bytes": args.max_bytes,
            "content": contents,
        });
        println!(
            "{}",
            serde_json::to_string(&doc).expect("startup recall should serialize")
        );
    } else {
        print!("{contents}");
    }
    Ok(EXIT_OK)
}

struct RecallHit {
    file: String,
    line: usize,
    text: String,
}

fn on_demand(layout: &Layout, args: &RecallOnDemandArgs) -> Result<i32, CliError> {
    let global = layout.global_dir();
    if !global.is_dir() {
        return Err(CliError::runtime(format!(
            "not found: {}",
            display_path(&global)
        )));
    }
    let needle = args.term.to_lowercase();
    let mut hits = Vec::new();
    for file in markdown_files(&global)? {
        let Some(name) = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        if name == "MEMORY.md" {
            continue;
        }
        let contents = fs::read_to_string(&file).map_err(|err| {
            CliError::runtime(format!("failed to read {}: {err}", display_path(&file)))
        })?;
        for (index, line) in contents.lines().enumerate() {
            if line.to_lowercase().contains(&needle) {
                hits.push(RecallHit {
                    file: name.clone(),
                    line: index + 1,
                    text: line.trim().to_string(),
                });
            }
        }
    }

    let format = output_format(args.format, args.json);
    if format.is_json() {
        let records: Vec<_> = hits
            .iter()
            .map(|hit| {
                json!({
                    "scope": "global",
                    "file": hit.file,
                    "line": hit.line,
                    "text": hit.text,
                })
            })
            .collect();
        let doc = json!({
            "schema_version": schema_version_for("agent-memory", "recall-on-demand", 1),
            "ok": !hits.is_empty(),
            "profile": "on-demand",
            "trust": "untrusted-memory-data",
            "curation": "curated",
            "term": args.term,
            "count": hits.len(),
            "hits": records,
        });
        println!(
            "{}",
            serde_json::to_string(&doc).expect("on-demand recall should serialize")
        );
    } else {
        for hit in &hits {
            println!("global/{}:{}: {}", hit.file, hit.line, hit.text);
        }
    }
    Ok(if hits.is_empty() {
        EXIT_RUNTIME
    } else {
        EXIT_OK
    })
}

fn candidates(layout: &Layout, args: &RecallCandidatesArgs) -> Result<i32, CliError> {
    crate::candidate::print_list(
        layout,
        args.producer.as_deref(),
        output_format(args.format, args.json),
        "recall-candidates",
    )
}

fn output_format(format: OutputFormat, json: bool) -> OutputFormat {
    if json { OutputFormat::Json } else { format }
}
