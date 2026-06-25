//! `agent-memory add` — create a note and its `MEMORY.md` index entry as one
//! guarded operation, so the two never drift.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::json;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use nils_common::fs::display_path;

use crate::cli::AddArgs;
use crate::frontmatter::{self, VALID_TYPES};
use crate::{CliError, EXIT_OK, Layout};

pub(crate) fn run(layout: &Layout, args: &AddArgs) -> Result<i32, CliError> {
    if !VALID_TYPES.contains(&args.r#type.as_str()) {
        return Err(CliError::usage(format!(
            "invalid type '{}': expected one of user|feedback|project|reference",
            args.r#type
        )));
    }
    if !frontmatter::is_valid_slug(&args.name) {
        return Err(CliError::usage(format!(
            "invalid name '{}': use ASCII letters, digits, '-' or '_'",
            args.name
        )));
    }

    let scope = args.scope.as_deref().unwrap_or("global");
    let dir = layout.resolve_scope(Some(scope));
    if !dir.is_dir() {
        return Err(CliError::runtime(format!(
            "not found: {}",
            display_path(&dir)
        )));
    }

    let note_path = dir.join(format!("{}.md", args.name));
    if note_path.exists() {
        return Err(CliError::runtime(format!(
            "already exists: {}",
            display_path(&note_path)
        )));
    }
    let index_path = dir.join("MEMORY.md");
    if !index_path.is_file() {
        return Err(CliError::runtime(format!(
            "no MEMORY.md in {}",
            display_path(&dir)
        )));
    }

    let body = read_body(args)?;
    let contents = frontmatter::render_note(
        &args.name,
        &args.description,
        &args.r#type,
        args.session_id.as_deref(),
        &body,
    );

    let title = args.title.clone().unwrap_or_else(|| args.name.clone());
    let hook = args
        .hook
        .clone()
        .unwrap_or_else(|| args.description.clone());
    let index_line = format!("- [{title}]({}.md) — {hook}\n", args.name);

    // Write the note first (atomic via temp+rename); then append the index line
    // (also atomic). If the index update fails, roll the note back so the store
    // is never left with a note that has no index entry.
    write_atomic(&note_path, &contents)?;
    if let Err(err) = append_index_line(&index_path, &index_line) {
        let _ = fs::remove_file(&note_path);
        return Err(err);
    }

    let format = if args.json {
        OutputFormat::Json
    } else {
        args.format
    };
    if format.is_json() {
        let doc = json!({
            "schema_version": schema_version_for("agent-memory", "add", 1),
            "ok": true,
            "scope": scope,
            "note": display_path(&note_path),
            "index": display_path(&index_path),
        });
        println!(
            "{}",
            serde_json::to_string(&doc).expect("add report should serialize")
        );
    } else {
        println!("created: {}", display_path(&note_path));
        println!("indexed: {}", display_path(&index_path));
    }
    Ok(EXIT_OK)
}

fn read_body(args: &AddArgs) -> Result<String, CliError> {
    if let Some(path) = &args.body_file {
        return fs::read_to_string(path)
            .map_err(|err| CliError::runtime(format!("failed to read body file {path}: {err}")));
    }
    match args.body.as_deref() {
        Some("-") => {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|err| CliError::runtime(format!("failed to read stdin: {err}")))?;
            Ok(buffer)
        }
        Some(text) => Ok(text.to_string()),
        None => Ok(String::new()),
    }
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), CliError> {
    let tmp = temp_sibling(path);
    fs::write(&tmp, contents).map_err(|err| {
        CliError::runtime(format!("failed to write {}: {err}", display_path(&tmp)))
    })?;
    fs::rename(&tmp, path).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        CliError::runtime(format!("failed to finalize {}: {err}", display_path(path)))
    })
}

fn append_index_line(index_path: &Path, line: &str) -> Result<(), CliError> {
    let mut contents = fs::read_to_string(index_path).map_err(|err| {
        CliError::runtime(format!(
            "failed to read {}: {err}",
            display_path(index_path)
        ))
    })?;
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(line);
    write_atomic(index_path, &contents)
}

fn temp_sibling(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|value| value.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}
