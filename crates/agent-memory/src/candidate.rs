//! Producer-isolated, untrusted memory candidate lifecycle.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::json;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use nils_common::fs::display_path;

use crate::cli::{
    CandidateAddArgs, CandidateArgs, CandidateCommand, CandidateListArgs, CandidatePromoteArgs,
};
use crate::frontmatter::{self, VALID_TYPES};
use crate::{CliError, EXIT_OK, EXIT_USAGE, Layout, validate_id};

pub(crate) fn run(layout: &Layout, args: &CandidateArgs) -> Result<i32, CliError> {
    let (json, schema_command) = match &args.command {
        CandidateCommand::Add(args) => (args.json || args.format.is_json(), "candidate-add"),
        CandidateCommand::List(args) => (args.json || args.format.is_json(), "candidate-list"),
        CandidateCommand::Promote(args) => {
            (args.json || args.format.is_json(), "candidate-promote")
        }
    };
    let result = match &args.command {
        CandidateCommand::Add(args) => add(layout, args),
        CandidateCommand::List(args) => list(layout, args),
        CandidateCommand::Promote(args) => promote(layout, args),
    };
    match result {
        Err(err) if json => {
            print_json_error(schema_command, &err);
            Ok(err.exit_code)
        }
        other => other,
    }
}

fn add(layout: &Layout, args: &CandidateAddArgs) -> Result<i32, CliError> {
    validate_id(&args.producer)?;
    validate_slug(&args.name)?;
    validate_optional_single_line("title", args.title.as_deref())?;
    validate_optional_single_line("hook", args.hook.as_deref())?;
    let producer_dir = ensure_producer_dir(layout, &args.producer)?;
    let index = producer_dir.join("MEMORY.md");
    ensure_regular_file(&index, "candidate index")?;

    let candidate = producer_dir.join(format!("{}.md", args.name));
    if fs::symlink_metadata(&candidate).is_ok() {
        return Err(CliError::runtime(format!(
            "already exists: {}",
            display_path(&candidate)
        )));
    }
    let mut body = read_body(args.body.as_deref(), args.body_file.as_deref())?;
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    let title = args.title.as_deref().unwrap_or(&args.name);
    let hook = args.hook.as_deref().unwrap_or("untrusted candidate");
    let line = format!("- [{title}]({}.md) — {hook}\n", args.name);

    write_atomic(&candidate, &body)?;
    if let Err(err) = append_index_line(&index, &line) {
        let _ = fs::remove_file(&candidate);
        return Err(err);
    }

    let format = output_format(args.format, args.json);
    if format.is_json() {
        let doc = json!({
            "schema_version": schema_version_for("agent-memory", "candidate-add", 1),
            "ok": true,
            "trust": "untrusted",
            "producer": args.producer,
            "candidate": display_path(&candidate),
            "index": display_path(&index),
        });
        println!(
            "{}",
            serde_json::to_string(&doc).expect("candidate add should serialize")
        );
    } else {
        println!("created untrusted candidate: {}", display_path(&candidate));
        println!("indexed: {}", display_path(&index));
    }
    Ok(EXIT_OK)
}

fn list(layout: &Layout, args: &CandidateListArgs) -> Result<i32, CliError> {
    print_list(
        layout,
        args.producer.as_deref(),
        output_format(args.format, args.json),
        "candidate-list",
    )
}

struct CandidateRow {
    producer: String,
    file: String,
    path: PathBuf,
    mtime: Option<u64>,
    preview: String,
}

pub(crate) fn print_list(
    layout: &Layout,
    producer: Option<&str>,
    format: OutputFormat,
    schema_command: &str,
) -> Result<i32, CliError> {
    let producers = producer_dirs(layout, producer)?;
    let mut rows = Vec::new();
    for (producer, dir) in producers {
        for entry in fs::read_dir(&dir).map_err(|err| {
            CliError::runtime(format!("failed to read {}: {err}", display_path(&dir)))
        })? {
            let entry =
                entry.map_err(|err| CliError::runtime(format!("failed to read entry: {err}")))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|err| {
                CliError::runtime(format!("failed to inspect {}: {err}", display_path(&path)))
            })?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || path.extension().is_none_or(|ext| ext != "md")
                || path.file_name().is_some_and(|name| name == "MEMORY.md")
            {
                continue;
            }
            let contents = fs::read_to_string(&path).map_err(|err| {
                CliError::runtime(format!("failed to read {}: {err}", display_path(&path)))
            })?;
            let preview = contents
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("")
                .chars()
                .take(160)
                .collect();
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| value.as_secs());
            rows.push(CandidateRow {
                producer: producer.clone(),
                file: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path,
                mtime,
                preview,
            });
        }
    }
    rows.sort_by(|left, right| (&left.producer, &left.file).cmp(&(&right.producer, &right.file)));

    if format.is_json() {
        let candidates: Vec<_> = rows
            .iter()
            .map(|row| {
                json!({
                    "producer": row.producer,
                    "file": row.file,
                    "path": display_path(&row.path),
                    "mtime": row.mtime,
                    "preview": row.preview,
                })
            })
            .collect();
        let doc = json!({
            "schema_version": schema_version_for("agent-memory", schema_command, 1),
            "ok": true,
            "trust": "untrusted",
            "count": rows.len(),
            "candidates": candidates,
        });
        println!(
            "{}",
            serde_json::to_string(&doc).expect("candidate list should serialize")
        );
    } else {
        println!("UNTRUSTED memory candidates; review and verify before promotion.");
        for row in &rows {
            println!("{}/{}\t{}", row.producer, row.file, row.preview);
        }
    }
    Ok(EXIT_OK)
}

fn promote(layout: &Layout, args: &CandidatePromoteArgs) -> Result<i32, CliError> {
    validate_id(&args.producer)?;
    validate_slug(&args.name)?;
    if !VALID_TYPES.contains(&args.r#type.as_str()) {
        return Err(CliError::usage(format!(
            "invalid type '{}': expected one of user|feedback|project|reference",
            args.r#type
        )));
    }
    validate_single_line("description", &args.description)?;
    validate_optional_single_line("title", args.title.as_deref())?;
    validate_optional_single_line("hook", args.hook.as_deref())?;
    validate_single_line("session-id", &args.session_id)?;

    let producer_dir = layout.candidates_dir().join(&args.producer);
    ensure_regular_dir(&producer_dir, "candidate producer directory")?;
    let source = producer_dir.join(format!("{}.md", args.name));
    ensure_regular_file(&source, "candidate source")?;
    let candidate_index = producer_dir.join("MEMORY.md");
    ensure_regular_file(&candidate_index, "candidate index")?;

    let global = layout.global_dir();
    ensure_supported_global_dir(&global)?;
    let global_index = global.join("MEMORY.md");
    ensure_regular_file(&global_index, "global index")?;
    let destination = global.join(format!("{}.md", args.name));
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(CliError::runtime(format!(
            "already exists: {}",
            display_path(&destination)
        )));
    }

    let source_body = fs::read_to_string(&source).map_err(|err| {
        CliError::runtime(format!("failed to read {}: {err}", display_path(&source)))
    })?;
    let note = frontmatter::render_note(
        &args.name,
        &args.description,
        &args.r#type,
        Some(&args.session_id),
        &source_body,
    );
    let title = args.title.as_deref().unwrap_or(&args.name);
    let hook = args.hook.as_deref().unwrap_or(&args.description);
    let global_line = format!("- [{title}]({}.md) — {hook}\n", args.name);
    let global_original = fs::read_to_string(&global_index).map_err(|err| {
        CliError::runtime(format!(
            "failed to read {}: {err}",
            display_path(&global_index)
        ))
    })?;
    let destination_marker = format!("]({}.md)", args.name);
    if global_original
        .lines()
        .any(|line| line.contains(&destination_marker))
    {
        return Err(CliError::runtime(format!(
            "global index already references {}.md",
            args.name
        )));
    }
    let candidate_original = fs::read_to_string(&candidate_index).map_err(|err| {
        CliError::runtime(format!(
            "failed to read {}: {err}",
            display_path(&candidate_index)
        ))
    })?;
    let global_updated = append_line(&global_original, &global_line);
    let candidate_updated = remove_candidate_link(&candidate_original, &args.name);

    if args.apply {
        apply_promotion(PromotionFiles {
            source: &source,
            destination: &destination,
            global_index: &global_index,
            candidate_index: &candidate_index,
            note: &note,
            global_updated: &global_updated,
            candidate_updated: &candidate_updated,
        })?;
    }

    let format = output_format(args.format, args.json);
    if format.is_json() {
        let doc = json!({
            "schema_version": schema_version_for("agent-memory", "candidate-promote", 1),
            "ok": true,
            "applied": args.apply,
            "trust": if args.apply { "curated-after-explicit-apply" } else { "untrusted-candidate" },
            "producer": args.producer,
            "source": display_path(&source),
            "destination": display_path(&destination),
            "global_index": display_path(&global_index),
            "candidate_index": display_path(&candidate_index),
        });
        println!(
            "{}",
            serde_json::to_string(&doc).expect("candidate promotion should serialize")
        );
    } else if args.apply {
        println!("promoted: {}", display_path(&destination));
        println!("removed candidate: {}", display_path(&source));
    } else {
        println!(
            "dry-run: promote {} -> {}",
            display_path(&source),
            display_path(&destination)
        );
        println!("re-run with --apply after explicit approval");
    }
    Ok(EXIT_OK)
}

struct PromotionFiles<'a> {
    source: &'a Path,
    destination: &'a Path,
    global_index: &'a Path,
    candidate_index: &'a Path,
    note: &'a str,
    global_updated: &'a str,
    candidate_updated: &'a str,
}

fn apply_promotion(files: PromotionFiles<'_>) -> Result<(), CliError> {
    let source_backup = sibling(files.source, ".promote-backup");
    let global_backup = sibling(files.global_index, ".promote-backup");
    let candidate_backup = sibling(files.candidate_index, ".promote-backup");
    let destination_tmp = sibling(files.destination, ".promote-tmp");
    let global_tmp = sibling(files.global_index, ".promote-tmp");
    let candidate_tmp = sibling(files.candidate_index, ".promote-tmp");
    let transaction_paths = [
        &source_backup,
        &global_backup,
        &candidate_backup,
        &destination_tmp,
        &global_tmp,
        &candidate_tmp,
    ];
    if let Some(path) = transaction_paths
        .iter()
        .find(|path| fs::symlink_metadata(path).is_ok())
    {
        return Err(CliError::runtime(format!(
            "promotion transaction path already exists: {}",
            display_path(path)
        )));
    }

    write_new(&destination_tmp, files.note)?;
    if let Err(err) = write_new(&global_tmp, files.global_updated) {
        cleanup(&[&destination_tmp]);
        return Err(err);
    }
    if let Err(err) = write_new(&candidate_tmp, files.candidate_updated) {
        cleanup(&[&destination_tmp, &global_tmp]);
        return Err(err);
    }

    let mut progress = PromotionProgress::default();
    let result = (|| -> std::io::Result<()> {
        fs::rename(files.source, &source_backup)?;
        progress.source_backed_up = true;
        fs::rename(files.global_index, &global_backup)?;
        progress.global_backed_up = true;
        fs::rename(files.candidate_index, &candidate_backup)?;
        progress.candidate_backed_up = true;
        fs::rename(&destination_tmp, files.destination)?;
        progress.destination_installed = true;
        fs::rename(&global_tmp, files.global_index)?;
        progress.global_installed = true;
        fs::rename(&candidate_tmp, files.candidate_index)?;
        progress.candidate_installed = true;
        Ok(())
    })();

    if let Err(err) = result {
        let rollback_errors = rollback_promotion(
            &files,
            &source_backup,
            &global_backup,
            &candidate_backup,
            &destination_tmp,
            &global_tmp,
            &candidate_tmp,
            progress,
        );
        if !rollback_errors.is_empty() {
            return Err(CliError::runtime(format!(
                "promotion failed ({err}); rollback incomplete: {}. Preserve .promote-backup files for manual recovery",
                rollback_errors.join("; ")
            )));
        }
        return Err(CliError::runtime(format!(
            "promotion failed and was rolled back: {err}"
        )));
    }

    cleanup(&[&source_backup, &global_backup, &candidate_backup]);
    Ok(())
}

#[derive(Clone, Copy, Default)]
struct PromotionProgress {
    source_backed_up: bool,
    global_backed_up: bool,
    candidate_backed_up: bool,
    destination_installed: bool,
    global_installed: bool,
    candidate_installed: bool,
}

#[allow(clippy::too_many_arguments)]
fn rollback_promotion(
    files: &PromotionFiles<'_>,
    source_backup: &Path,
    global_backup: &Path,
    candidate_backup: &Path,
    destination_tmp: &Path,
    global_tmp: &Path,
    candidate_tmp: &Path,
    progress: PromotionProgress,
) -> Vec<String> {
    let mut errors = Vec::new();
    if progress.destination_installed {
        record_rollback_result(
            fs::remove_file(files.destination),
            "remove installed destination",
            &mut errors,
        );
    }
    if progress.global_installed {
        record_rollback_result(
            fs::remove_file(files.global_index),
            "remove installed global index",
            &mut errors,
        );
    }
    if progress.candidate_installed {
        record_rollback_result(
            fs::remove_file(files.candidate_index),
            "remove installed candidate index",
            &mut errors,
        );
    }
    if progress.source_backed_up {
        record_rollback_result(
            fs::rename(source_backup, files.source),
            "restore candidate source",
            &mut errors,
        );
    }
    if progress.global_backed_up {
        record_rollback_result(
            fs::rename(global_backup, files.global_index),
            "restore global index",
            &mut errors,
        );
    }
    if progress.candidate_backed_up {
        record_rollback_result(
            fs::rename(candidate_backup, files.candidate_index),
            "restore candidate index",
            &mut errors,
        );
    }
    for (path, action) in [
        (destination_tmp, "remove destination temp"),
        (global_tmp, "remove global-index temp"),
        (candidate_tmp, "remove candidate-index temp"),
    ] {
        if path.exists() {
            record_rollback_result(fs::remove_file(path), action, &mut errors);
        }
    }
    errors
}

fn record_rollback_result(result: std::io::Result<()>, action: &str, errors: &mut Vec<String>) {
    if let Err(err) = result {
        errors.push(format!("{action}: {err}"));
    }
}

fn producer_dirs(
    layout: &Layout,
    producer: Option<&str>,
) -> Result<Vec<(String, PathBuf)>, CliError> {
    if let Some(producer) = producer {
        validate_id(producer)?;
        let dir = layout.candidates_dir().join(producer);
        ensure_regular_dir(&dir, "candidate producer directory")?;
        return Ok(vec![(producer.to_string(), dir)]);
    }
    let root = layout.candidates_dir();
    if !root.exists() {
        return Ok(Vec::new());
    }
    ensure_regular_dir(&root, "candidate root")?;
    let mut dirs = Vec::new();
    for entry in fs::read_dir(&root).map_err(|err| {
        CliError::runtime(format!("failed to read {}: {err}", display_path(&root)))
    })? {
        let entry =
            entry.map_err(|err| CliError::runtime(format!("failed to read entry: {err}")))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|err| {
            CliError::runtime(format!("failed to inspect {}: {err}", display_path(&path)))
        })?;
        if metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && let Some(name) = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        {
            dirs.push((name, path));
        }
    }
    dirs.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(dirs)
}

fn ensure_producer_dir(layout: &Layout, producer: &str) -> Result<PathBuf, CliError> {
    let root = layout.candidates_dir();
    create_regular_dir(&root, "candidate root")?;
    let producer_dir = root.join(producer);
    create_regular_dir(&producer_dir, "candidate producer directory")?;
    let index = producer_dir.join("MEMORY.md");
    if !index.exists() {
        write_new(
            &index,
            &format!("# {producer} memory candidates\n\nUntrusted proposal data only.\n"),
        )?;
    }
    Ok(producer_dir)
}

fn create_regular_dir(path: &Path, label: &str) -> Result<(), CliError> {
    if path.exists() {
        return ensure_regular_dir(path, label);
    }
    fs::create_dir(path).map_err(|err| {
        CliError::runtime(format!(
            "failed to create {label} {}: {err}",
            display_path(path)
        ))
    })
}

fn ensure_regular_dir(path: &Path, label: &str) -> Result<(), CliError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        CliError::runtime(format!(
            "{label} not found at {}: {err}",
            display_path(path)
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CliError::runtime(format!(
            "{label} must be a regular, non-symlink directory: {}",
            display_path(path)
        )));
    }
    Ok(())
}

fn ensure_supported_global_dir(path: &Path) -> Result<(), CliError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        CliError::runtime(format!(
            "global directory not found at {}: {err}",
            display_path(path)
        ))
    })?;
    if metadata.is_dir() || (metadata.file_type().is_symlink() && path.is_dir()) {
        return Ok(());
    }
    Err(CliError::runtime(format!(
        "global directory is not a directory or valid directory symlink: {}",
        display_path(path)
    )))
}

fn ensure_regular_file(path: &Path, label: &str) -> Result<(), CliError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        CliError::runtime(format!(
            "{label} not found at {}: {err}",
            display_path(path)
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CliError::runtime(format!(
            "{label} must be a regular, non-symlink file: {}",
            display_path(path)
        )));
    }
    Ok(())
}

fn read_body(body: Option<&str>, body_file: Option<&str>) -> Result<String, CliError> {
    if let Some(path) = body_file {
        let path = Path::new(path);
        ensure_regular_file(path, "candidate body file")?;
        return fs::read_to_string(path).map_err(|err| {
            CliError::runtime(format!("failed to read {}: {err}", display_path(path)))
        });
    }
    match body {
        Some("-") => {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|err| CliError::runtime(format!("failed to read stdin: {err}")))?;
            Ok(buffer)
        }
        Some(value) => Ok(value.to_string()),
        None => Ok(String::new()),
    }
}

fn validate_slug(name: &str) -> Result<(), CliError> {
    if frontmatter::is_valid_slug(name) {
        Ok(())
    } else {
        Err(CliError::usage(format!(
            "invalid name '{name}': use ASCII letters, digits, '-' or '_'"
        )))
    }
}

fn validate_single_line(label: &str, value: &str) -> Result<(), CliError> {
    if value.trim().is_empty() || value.contains(['\n', '\r']) {
        return Err(CliError::usage(format!(
            "{label} must be a non-empty single-line value"
        )));
    }
    Ok(())
}

fn validate_optional_single_line(label: &str, value: Option<&str>) -> Result<(), CliError> {
    if let Some(value) = value {
        validate_single_line(label, value)?;
    }
    Ok(())
}

fn append_index_line(index: &Path, line: &str) -> Result<(), CliError> {
    ensure_regular_file(index, "candidate index")?;
    let original = fs::read_to_string(index).map_err(|err| {
        CliError::runtime(format!("failed to read {}: {err}", display_path(index)))
    })?;
    write_atomic(index, &append_line(&original, line))
}

fn append_line(original: &str, line: &str) -> String {
    let mut updated = original.to_string();
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(line);
    updated
}

fn remove_candidate_link(original: &str, slug: &str) -> String {
    let marker = format!("{slug}.md");
    let retained: Vec<_> = original
        .lines()
        .filter(|line| !line.contains(&marker))
        .collect();
    let mut updated = retained.join("\n");
    if original.ends_with('\n') {
        updated.push('\n');
    }
    updated
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), CliError> {
    let tmp = sibling(path, ".tmp");
    if fs::symlink_metadata(&tmp).is_ok() {
        return Err(CliError::runtime(format!(
            "temporary path already exists: {}",
            display_path(&tmp)
        )));
    }
    write_new(&tmp, contents)?;
    fs::rename(&tmp, path).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        CliError::runtime(format!("failed to finalize {}: {err}", display_path(path)))
    })
}

fn write_new(path: &Path, contents: &str) -> Result<(), CliError> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| {
            CliError::runtime(format!("failed to create {}: {err}", display_path(path)))
        })?;
    file.write_all(contents.as_bytes()).map_err(|err| {
        let _ = fs::remove_file(path);
        CliError::runtime(format!("failed to write {}: {err}", display_path(path)))
    })
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|value| value.to_os_string())
        .unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

fn cleanup(paths: &[&Path]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn output_format(format: OutputFormat, json: bool) -> OutputFormat {
    if json { OutputFormat::Json } else { format }
}

fn print_json_error(schema_command: &str, err: &CliError) {
    let code = if err.exit_code == EXIT_USAGE {
        "usage-error"
    } else {
        "runtime-error"
    };
    let doc = json!({
        "schema_version": schema_version_for("agent-memory", schema_command, 1),
        "ok": false,
        "error": {
            "code": code,
            "message": err.message,
        },
    });
    println!(
        "{}",
        serde_json::to_string(&doc).expect("candidate error should serialize")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_after_source_backup_does_not_delete_live_indexes() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let source = tmp.path().join("candidate.md");
        let destination = tmp.path().join("destination.md");
        let global_index = tmp.path().join("global-index.md");
        let candidate_index = tmp.path().join("candidate-index.md");
        fs::write(&source, "candidate").expect("source");
        fs::write(&global_index, "global-original").expect("global");
        fs::write(&candidate_index, "candidate-original").expect("candidate index");

        let source_backup = sibling(&source, ".promote-backup");
        let global_backup = sibling(&global_index, ".promote-backup");
        let candidate_backup = sibling(&candidate_index, ".promote-backup");
        let destination_tmp = sibling(&destination, ".promote-tmp");
        let global_tmp = sibling(&global_index, ".promote-tmp");
        let candidate_tmp = sibling(&candidate_index, ".promote-tmp");
        fs::rename(&source, &source_backup).expect("back up source");

        let files = PromotionFiles {
            source: &source,
            destination: &destination,
            global_index: &global_index,
            candidate_index: &candidate_index,
            note: "new note",
            global_updated: "new global",
            candidate_updated: "new candidate",
        };
        let errors = rollback_promotion(
            &files,
            &source_backup,
            &global_backup,
            &candidate_backup,
            &destination_tmp,
            &global_tmp,
            &candidate_tmp,
            PromotionProgress {
                source_backed_up: true,
                ..PromotionProgress::default()
            },
        );
        assert!(errors.is_empty(), "rollback errors: {errors:?}");

        assert_eq!(fs::read_to_string(&source).expect("source"), "candidate");
        assert_eq!(
            fs::read_to_string(&global_index).expect("global"),
            "global-original"
        );
        assert_eq!(
            fs::read_to_string(&candidate_index).expect("candidate index"),
            "candidate-original"
        );
    }

    #[test]
    fn rollback_after_install_restores_source_and_both_indexes() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let source = tmp.path().join("candidate.md");
        let destination = tmp.path().join("destination.md");
        let global_index = tmp.path().join("global-index.md");
        let candidate_index = tmp.path().join("candidate-index.md");
        let source_backup = sibling(&source, ".promote-backup");
        let global_backup = sibling(&global_index, ".promote-backup");
        let candidate_backup = sibling(&candidate_index, ".promote-backup");
        let destination_tmp = sibling(&destination, ".promote-tmp");
        let global_tmp = sibling(&global_index, ".promote-tmp");
        let candidate_tmp = sibling(&candidate_index, ".promote-tmp");
        fs::write(&source_backup, "candidate-original").expect("source backup");
        fs::write(&global_backup, "global-original").expect("global backup");
        fs::write(&candidate_backup, "candidate-index-original").expect("candidate backup");
        fs::write(&destination, "new note").expect("destination");
        fs::write(&global_index, "new global").expect("new global");
        fs::write(&candidate_index, "new candidate index").expect("new candidate");

        let files = PromotionFiles {
            source: &source,
            destination: &destination,
            global_index: &global_index,
            candidate_index: &candidate_index,
            note: "new note",
            global_updated: "new global",
            candidate_updated: "new candidate",
        };
        let errors = rollback_promotion(
            &files,
            &source_backup,
            &global_backup,
            &candidate_backup,
            &destination_tmp,
            &global_tmp,
            &candidate_tmp,
            PromotionProgress {
                source_backed_up: true,
                global_backed_up: true,
                candidate_backed_up: true,
                destination_installed: true,
                global_installed: true,
                candidate_installed: true,
            },
        );
        assert!(errors.is_empty(), "rollback errors: {errors:?}");

        assert!(!destination.exists());
        assert_eq!(
            fs::read_to_string(&source).expect("source"),
            "candidate-original"
        );
        assert_eq!(
            fs::read_to_string(&global_index).expect("global"),
            "global-original"
        );
        assert_eq!(
            fs::read_to_string(&candidate_index).expect("candidate index"),
            "candidate-index-original"
        );
    }

    #[test]
    fn rollback_reports_incomplete_restore_and_retains_backup() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let source = tmp.path().join("candidate.md");
        let destination = tmp.path().join("destination.md");
        let global_index = tmp.path().join("global-index.md");
        let candidate_index = tmp.path().join("candidate-index.md");
        let source_backup = sibling(&source, ".promote-backup");
        let global_backup = sibling(&global_index, ".promote-backup");
        let candidate_backup = sibling(&candidate_index, ".promote-backup");
        let destination_tmp = sibling(&destination, ".promote-tmp");
        let global_tmp = sibling(&global_index, ".promote-tmp");
        let candidate_tmp = sibling(&candidate_index, ".promote-tmp");
        fs::write(&source_backup, "candidate-original").expect("source backup");
        fs::create_dir(&source).expect("conflicting source directory");

        let files = PromotionFiles {
            source: &source,
            destination: &destination,
            global_index: &global_index,
            candidate_index: &candidate_index,
            note: "new note",
            global_updated: "new global",
            candidate_updated: "new candidate",
        };
        let errors = rollback_promotion(
            &files,
            &source_backup,
            &global_backup,
            &candidate_backup,
            &destination_tmp,
            &global_tmp,
            &candidate_tmp,
            PromotionProgress {
                source_backed_up: true,
                ..PromotionProgress::default()
            },
        );

        assert!(!errors.is_empty());
        assert!(errors[0].contains("restore candidate source"));
        assert!(source_backup.is_file(), "backup must remain for recovery");
    }
}
