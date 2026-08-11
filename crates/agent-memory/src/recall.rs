//! Bounded startup, curated on-demand, and untrusted candidate recall.

use std::fs;

use serde_json::json;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use nils_common::fs::display_path;

use crate::cli::{
    RecallArgs, RecallCandidatesArgs, RecallCommand, RecallOnDemandArgs, RecallStartupArgs,
};
use crate::{CliError, EXIT_OK, EXIT_RUNTIME, EXIT_USAGE, Layout, markdown_files, validate_id};

pub(crate) fn run(layout: &Layout, args: &RecallArgs) -> Result<i32, CliError> {
    let (json, schema_command) = match &args.command {
        RecallCommand::Startup(args) => (args.json || args.format.is_json(), "recall-startup"),
        RecallCommand::OnDemand(args) => (args.json || args.format.is_json(), "recall-on-demand"),
        RecallCommand::Candidates(args) => {
            (args.json || args.format.is_json(), "recall-candidates")
        }
    };
    let result = match &args.command {
        RecallCommand::Startup(args) => startup(layout, args),
        RecallCommand::OnDemand(args) => on_demand(layout, args),
        RecallCommand::Candidates(args) => candidates(layout, args),
    };
    match result {
        Err(err) if json => {
            print_json_error(schema_command, &err);
            Ok(err.exit_code)
        }
        other => other,
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
    scope: String,
    file: String,
    line: usize,
    text: String,
}

fn on_demand(layout: &Layout, args: &RecallOnDemandArgs) -> Result<i32, CliError> {
    if args.term.trim().is_empty() {
        return Err(CliError::usage("on-demand recall term must not be empty"));
    }
    let global = layout.global_dir();
    if !global.is_dir() {
        return Err(CliError::runtime(format!(
            "not found: {}",
            display_path(&global)
        )));
    }
    let mut scopes = vec![("global".to_string(), global)];
    if let Some(agent) = args.agent.as_deref() {
        validate_id(agent)?;
        let agent_dir = layout.agents_dir().join(agent);
        validate_agent_scope_metadata(agent, fs::symlink_metadata(&agent_dir))?;
        scopes.push((format!("agents/{agent}"), agent_dir));
    }

    let needle = args.term.to_lowercase();
    let mut hits = Vec::new();
    for (scope, directory) in scopes {
        for file in markdown_files(&directory)? {
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
                        scope: scope.clone(),
                        file: name.clone(),
                        line: index + 1,
                        text: line.trim().to_string(),
                    });
                }
            }
        }
    }

    let format = output_format(args.format, args.json);
    if format.is_json() {
        let records: Vec<_> = hits
            .iter()
            .map(|hit| {
                json!({
                    "scope": hit.scope,
                    "file": hit.file,
                    "line": hit.line,
                    "text": hit.text,
                })
            })
            .collect();
        let mut doc = json!({
            "schema_version": schema_version_for("agent-memory", "recall-on-demand", 1),
            "ok": !hits.is_empty(),
            "profile": "on-demand",
            "trust": "untrusted-memory-data",
            "curation": "curated",
            "term": args.term,
            "count": hits.len(),
            "hits": records,
        });
        if let Some(agent) = args.agent.as_deref() {
            doc["agent"] = json!(agent);
        }
        println!(
            "{}",
            serde_json::to_string(&doc).expect("on-demand recall should serialize")
        );
    } else {
        for hit in &hits {
            println!("{}/{}:{}: {}", hit.scope, hit.file, hit.line, hit.text);
        }
    }
    Ok(if hits.is_empty() {
        EXIT_RUNTIME
    } else {
        EXIT_OK
    })
}

fn validate_agent_scope_metadata(
    agent: &str,
    metadata: std::io::Result<fs::Metadata>,
) -> Result<(), CliError> {
    match metadata {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {}
        Ok(_) => {
            return Err(CliError::runtime_typed(
                "agent-scope-not-found",
                format!("agent scope is not available: {agent}"),
                false,
                "select an existing agent scope or initialize one",
                "agent-memory agents",
            ));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(CliError::runtime_typed(
                "agent-scope-not-found",
                format!("agent scope is not available: {agent}"),
                false,
                "select an existing agent scope or initialize one",
                "agent-memory agents",
            ));
        }
        Err(err) => {
            return Err(CliError::runtime(format!(
                "failed to inspect agent scope {agent}: {err}"
            )));
        }
    }
    Ok(())
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

fn print_json_error(schema_command: &str, err: &CliError) {
    let default_code = if err.exit_code == EXIT_USAGE {
        "usage-error"
    } else {
        "runtime-error"
    };
    let mut error = json!({
        "code": err.code.unwrap_or(default_code),
        "message": err.message,
    });
    if let Some(details) = &err.details {
        error["details"] = json!({
            "retryable": details.retryable,
            "next_action": details.next_action,
            "recovery": {
                "command": details.recovery_command,
            },
        });
    }
    let doc = json!({
        "schema_version": schema_version_for("agent-memory", schema_command, 1),
        "ok": false,
        "error": error,
    });
    println!(
        "{}",
        serde_json::to_string(&doc).expect("recall error should serialize")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_scope_not_found_remains_typed_and_non_retryable() {
        let err = validate_agent_scope_metadata(
            "codex",
            Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        )
        .expect_err("missing agent scope");

        assert_eq!(err.code, Some("agent-scope-not-found"));
        assert!(!err.details.expect("typed details").retryable);
    }

    #[test]
    fn agent_scope_operational_error_is_not_misreported_as_missing() {
        let err = validate_agent_scope_metadata(
            "codex",
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        )
        .expect_err("agent scope inspection failure");

        assert_eq!(err.code, None);
        assert!(err.message.contains("failed to inspect agent scope codex"));
    }
}
