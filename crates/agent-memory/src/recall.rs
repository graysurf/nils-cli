//! Bounded startup, curated on-demand, and untrusted candidate recall.

use std::fs;
use std::io::{self, Write};

use serde::Serialize;
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

#[derive(Serialize)]
struct RecallHit {
    scope: String,
    file: String,
    line: usize,
    text: String,
}

#[derive(Serialize)]
struct RecallOnDemandResponse<'a> {
    schema_version: String,
    ok: bool,
    profile: &'static str,
    trust: &'static str,
    curation: &'static str,
    term: &'a str,
    count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<&'a str>,
    hits: &'a [RecallHit],
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
        let agents_dir = layout.agents_dir();
        validate_agents_root_metadata(agent, fs::symlink_metadata(&agents_dir))?;
        let agent_dir = agents_dir.join(agent);
        validate_agent_scope_metadata(agent, fs::symlink_metadata(&agent_dir))?;
        scopes.push((format!("agents/{agent}"), agent_dir));
    }

    let format = output_format(args.format, args.json);
    let json_output = format.is_json();
    let needle = args.term.to_lowercase();
    let mut hits = Vec::new();
    let mut hit_count = 0;
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
                if contains_case_insensitive(line, &needle) {
                    hit_count += 1;
                    if json_output {
                        hits.push(RecallHit {
                            scope: scope.clone(),
                            file: name.clone(),
                            line: index + 1,
                            text: line.trim().to_string(),
                        });
                    } else {
                        println!("{scope}/{name}:{}: {}", index + 1, line.trim());
                    }
                }
            }
        }
    }

    if json_output {
        let doc = RecallOnDemandResponse {
            schema_version: schema_version_for("agent-memory", "recall-on-demand", 1),
            ok: hit_count != 0,
            profile: "on-demand",
            trust: "untrusted-memory-data",
            curation: "curated",
            term: args.term.as_str(),
            count: hit_count,
            agent: args.agent.as_deref(),
            hits: &hits,
        };
        let mut output = io::stdout().lock();
        serde_json::to_writer(&mut output, &doc)
            .map_err(|err| CliError::runtime(format!("failed to write recall JSON: {err}")))?;
        output
            .write_all(b"\n")
            .map_err(|err| CliError::runtime(format!("failed to write recall JSON: {err}")))?;
    }
    Ok(if hit_count == 0 {
        EXIT_RUNTIME
    } else {
        EXIT_OK
    })
}

fn contains_case_insensitive(haystack: &str, lowercase_needle: &str) -> bool {
    // `str::contains` keeps the substring search algorithm out of the
    // quadratic `windows(...).eq_ignore_ascii_case(...)` worst case. Recall
    // processes one line at a time, so the temporary lowercase buffer stays
    // bounded by the current line rather than the complete result set.
    haystack.to_lowercase().contains(lowercase_needle)
}

fn validate_agents_root_metadata(
    agent: &str,
    metadata: std::io::Result<fs::Metadata>,
) -> Result<(), CliError> {
    match metadata {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => Ok(()),
        Ok(_) => Err(CliError::runtime_typed(
            "agent-scope-untrusted",
            "agent scope root must be a non-symlink directory",
            false,
            "repair the agent memory layout before agent-scoped recall",
            "agent-memory doctor",
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(agent_scope_not_found(agent)),
        Err(err) => Err(CliError::runtime(format!(
            "failed to inspect agent scope root: {err}"
        ))),
    }
}

fn validate_agent_scope_metadata(
    agent: &str,
    metadata: std::io::Result<fs::Metadata>,
) -> Result<(), CliError> {
    match metadata {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {}
        Ok(_) => {
            return Err(agent_scope_not_found(agent));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(agent_scope_not_found(agent));
        }
        Err(err) => {
            return Err(CliError::runtime(format!(
                "failed to inspect agent scope {agent}: {err}"
            )));
        }
    }
    Ok(())
}

fn agent_scope_not_found(agent: &str) -> CliError {
    CliError::runtime_typed(
        "agent-scope-not-found",
        format!("agent scope is not available: {agent}"),
        false,
        "select an existing agent scope or initialize one",
        "agent-memory agents",
    )
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
