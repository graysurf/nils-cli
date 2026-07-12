//! Explicit inactive history for curated notes superseded by runtime behavior.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use nils_common::fs::display_path;

use crate::cli::{
    ArchiveArgs, ArchiveCommand, ArchiveListArgs, ArchiveRetireArgs, ArchiveSearchArgs,
};
use crate::frontmatter::{self, is_valid_slug};
use crate::{CliError, EXIT_OK, EXIT_RUNTIME, EXIT_USAGE, Layout, markdown_files, memory_scopes};

pub(crate) fn run(layout: &Layout, args: &ArchiveArgs) -> Result<i32, CliError> {
    let (json_output, schema_command) = match &args.command {
        ArchiveCommand::List(args) => (args.json || args.format.is_json(), "archive-list"),
        ArchiveCommand::Search(args) => (args.json || args.format.is_json(), "archive-search"),
        ArchiveCommand::Retire(args) => (args.json || args.format.is_json(), "archive-retire"),
    };
    let result = match &args.command {
        ArchiveCommand::List(args) => list(layout, args),
        ArchiveCommand::Search(args) => search(layout, args),
        ArchiveCommand::Retire(args) => retire(layout, args),
    };
    match result {
        Ok(code) => Ok(code),
        Err(failure) if json_output => {
            print_json_error(schema_command, &failure);
            Ok(failure.exit_code())
        }
        Err(failure) => Err(failure.into_cli_error()),
    }
}

#[derive(Debug)]
struct ArchiveRow {
    file: String,
    name: String,
    description: String,
}

fn list(layout: &Layout, args: &ArchiveListArgs) -> Result<i32, ArchiveFailure> {
    let rows = archive_rows(layout)?;
    let format = output_format(args.format, args.json);
    if format.is_json() {
        let records: Vec<_> = rows
            .iter()
            .map(|row| {
                json!({
                    "file": row.file,
                    "name": row.name,
                    "description": row.description,
                    "status": "archived",
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&json!({
                "schema_version": schema_version_for("agent-memory", "archive-list", 1),
                "ok": true,
                "curation": "historical-inactive",
                "count": records.len(),
                "notes": records,
            }))
            .expect("archive list should serialize")
        );
    } else {
        for row in rows {
            println!("{}", row.file);
        }
    }
    Ok(EXIT_OK)
}

#[derive(Debug)]
struct ArchiveHit {
    file: String,
    line: usize,
    text: String,
}

fn search(layout: &Layout, args: &ArchiveSearchArgs) -> Result<i32, ArchiveFailure> {
    if args.term.trim().is_empty() {
        return Err(ArchiveFailure::usage(
            "archive search term must not be empty",
        ));
    }
    let root = superseded_dir(layout);
    let needle = args.term.to_lowercase();
    let mut hits = Vec::new();
    if root.is_dir() {
        for file in archive_markdown_files(&root)? {
            let file_name = file
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default();
            let contents = read_regular_file(&file, "archived note")?;
            for (index, line) in contents.lines().enumerate() {
                if line.to_lowercase().contains(&needle) {
                    hits.push(ArchiveHit {
                        file: file_name.clone(),
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
                    "file": hit.file,
                    "line": hit.line,
                    "text": hit.text,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&json!({
                "schema_version": schema_version_for("agent-memory", "archive-search", 1),
                "ok": !hits.is_empty(),
                "curation": "historical-inactive",
                "term": args.term,
                "count": records.len(),
                "hits": records,
            }))
            .expect("archive search should serialize")
        );
    } else {
        for hit in &hits {
            println!("archive/superseded/{}:{}: {}", hit.file, hit.line, hit.text);
        }
    }
    Ok(if hits.is_empty() {
        EXIT_RUNTIME
    } else {
        EXIT_OK
    })
}

#[derive(Clone, Debug)]
struct ActiveReference {
    file: String,
    line: usize,
    text: String,
}

#[derive(Debug)]
struct IndexUpdate {
    path: PathBuf,
    display: String,
    updated: String,
}

fn retire(layout: &Layout, args: &ArchiveRetireArgs) -> Result<i32, ArchiveFailure> {
    validate_retire_args(args)?;
    let source = layout.global_dir().join(format!("{}.md", args.name));
    let original = read_regular_file(&source, "curated source note")?;
    let frontmatter = frontmatter::parse(&original)
        .ok_or_else(|| ArchiveFailure::runtime("curated source note has no frontmatter"))?;
    let note_name = frontmatter.name.unwrap_or_else(|| args.name.clone());
    let description = frontmatter.description.unwrap_or_default();

    let target_dir = superseded_dir(layout);
    reject_symlink_ancestor(&layout.archive_dir(), "archive root")?;
    reject_symlink_ancestor(&target_dir, "superseded archive directory")?;
    let target = target_dir.join(format!("{}.md", args.name));
    if fs::symlink_metadata(&target).is_ok() {
        return Err(ArchiveFailure::runtime(format!(
            "archive target already exists: {}",
            display_path(&target)
        )));
    }

    let blockers = active_references(layout, &args.name, &source)?;
    if !blockers.is_empty() {
        return Err(ArchiveFailure::Blocked(blockers));
    }

    let index_updates = active_index_updates(layout, &args.name)?;
    if !index_updates
        .iter()
        .any(|update| update.path == layout.global_dir().join("MEMORY.md"))
    {
        return Err(ArchiveFailure::runtime(format!(
            "global index does not link {}.md",
            args.name
        )));
    }

    let archived = add_archive_metadata(
        &original,
        &args.archived_at,
        &args.reason,
        &args.superseded_by,
    )?;
    let archive_index = layout.archive_dir().join("MEMORY.md");
    let archive_index_original = if fs::symlink_metadata(&archive_index).is_ok() {
        Some(read_regular_file(&archive_index, "archive index")?)
    } else {
        None
    };
    let archive_index_updated = append_archive_index(
        archive_index_original.as_deref(),
        &args.name,
        &note_name,
        &description,
        &args.archived_at,
        &args.reason,
        &args.superseded_by,
    );

    if args.apply {
        apply_transaction(
            &source,
            &target,
            &archived,
            &archive_index,
            archive_index_original.is_some(),
            &archive_index_updated,
            &index_updates,
        )?;
    }

    let format = output_format(args.format, args.json);
    if format.is_json() {
        let updates: Vec<_> = index_updates
            .iter()
            .map(|update| update.display.clone())
            .collect();
        println!(
            "{}",
            serde_json::to_string(&json!({
                "schema_version": schema_version_for("agent-memory", "archive-retire", 1),
                "ok": true,
                "mode": if args.apply { "applied" } else { "dry-run" },
                "source": display_path(&source),
                "archive": display_path(&target),
                "reason": args.reason,
                "superseded_by": args.superseded_by,
                "archived_at": args.archived_at,
                "active_index_updates": updates,
            }))
            .expect("archive retirement should serialize")
        );
    } else if args.apply {
        println!("archived: {}", display_path(&target));
    } else {
        println!("dry-run: retire {}", display_path(&source));
        println!("archive: {}", display_path(&target));
        for update in &index_updates {
            println!("update index: {}", update.display);
        }
        println!("apply: rerun with --apply after approving this plan");
    }
    Ok(EXIT_OK)
}

fn validate_retire_args(args: &ArchiveRetireArgs) -> Result<(), ArchiveFailure> {
    if !is_valid_slug(&args.name) {
        return Err(ArchiveFailure::usage(format!(
            "invalid archive note slug: '{}'",
            args.name
        )));
    }
    if args.reason.trim().is_empty() {
        return Err(ArchiveFailure::usage("archive reason must not be empty"));
    }
    if args
        .superseded_by
        .iter()
        .any(|owner| owner.trim().is_empty() || owner.contains('\n') || owner.contains('\r'))
    {
        return Err(ArchiveFailure::usage(
            "superseded-by values must be non-empty single lines",
        ));
    }
    let bytes = args.archived_at.as_bytes();
    let valid_date = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit());
    if !valid_date {
        return Err(ArchiveFailure::usage("archived-at must use YYYY-MM-DD"));
    }
    Ok(())
}

fn superseded_dir(layout: &Layout) -> PathBuf {
    layout.archive_dir().join("superseded")
}

fn archive_rows(layout: &Layout) -> Result<Vec<ArchiveRow>, ArchiveFailure> {
    let root = superseded_dir(layout);
    if !root.exists() {
        return Ok(Vec::new());
    }
    ensure_regular_dir(&root, "superseded archive directory")?;
    let mut rows = Vec::new();
    for file in archive_markdown_files(&root)? {
        let contents = read_regular_file(&file, "archived note")?;
        let parsed = frontmatter::parse(&contents).unwrap_or_default();
        rows.push(ArchiveRow {
            file: file
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default(),
            name: parsed.name.unwrap_or_default(),
            description: parsed.description.unwrap_or_default(),
        });
    }
    Ok(rows)
}

fn archive_markdown_files(root: &Path) -> Result<Vec<PathBuf>, ArchiveFailure> {
    ensure_regular_dir(root, "superseded archive directory")?;
    let mut files = Vec::new();
    for entry in fs::read_dir(root).map_err(|err| {
        ArchiveFailure::runtime(format!("failed to read {}: {err}", display_path(root)))
    })? {
        let path = entry
            .map_err(|err| ArchiveFailure::runtime(format!("failed to read archive entry: {err}")))?
            .path();
        if path.extension().and_then(|value| value.to_str()) == Some("md") {
            read_regular_file(&path, "archived note")?;
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn active_references(
    layout: &Layout,
    slug: &str,
    source: &Path,
) -> Result<Vec<ActiveReference>, ArchiveFailure> {
    let wiki = format!("[[{slug}]]");
    let filename = format!("{slug}.md");
    let mut blockers = Vec::new();
    for (scope, dir) in memory_scopes(layout).map_err(ArchiveFailure::from_cli)? {
        for file in markdown_files(&dir).map_err(ArchiveFailure::from_cli)? {
            if file == source
                || file.file_name().and_then(|value| value.to_str()) == Some("MEMORY.md")
            {
                continue;
            }
            let contents = read_regular_file(&file, "active memory note")?;
            for (index, line) in contents.lines().enumerate() {
                if line.contains(&wiki) || line.contains(&filename) {
                    let relative = file
                        .strip_prefix(&layout.root)
                        .unwrap_or(&file)
                        .to_string_lossy()
                        .into_owned();
                    blockers.push(ActiveReference {
                        file: if relative.is_empty() {
                            scope.clone()
                        } else {
                            relative
                        },
                        line: index + 1,
                        text: line.trim().to_string(),
                    });
                }
            }
        }
    }
    Ok(blockers)
}

fn active_index_updates(layout: &Layout, slug: &str) -> Result<Vec<IndexUpdate>, ArchiveFailure> {
    let mut updates = Vec::new();
    for (scope, dir) in memory_scopes(layout).map_err(ArchiveFailure::from_cli)? {
        let index = dir.join("MEMORY.md");
        if !index.exists() {
            continue;
        }
        let original = read_regular_file(&index, "active memory index")?;
        let updated = remove_index_link(&original, &scope, slug);
        if updated != original {
            let display = index
                .strip_prefix(&layout.root)
                .unwrap_or(&index)
                .to_string_lossy()
                .into_owned();
            updates.push(IndexUpdate {
                path: index,
                display,
                updated,
            });
        }
    }
    Ok(updates)
}

fn remove_index_link(original: &str, scope: &str, slug: &str) -> String {
    let local_target = format!("({slug}.md)");
    let global_suffix = format!("/global/{slug}.md)");
    let mut output = String::new();
    for line in original.lines() {
        let remove = (scope == "global" && line.contains(&local_target))
            || (scope != "global" && line.contains(&global_suffix));
        if !remove {
            output.push_str(line);
            output.push('\n');
        }
    }
    output
}

fn add_archive_metadata(
    original: &str,
    archived_at: &str,
    reason: &str,
    superseded_by: &[String],
) -> Result<String, ArchiveFailure> {
    if original.contains("lifecycleStatus: archived") {
        return Err(ArchiveFailure::runtime(
            "source note is already marked archived",
        ));
    }
    let mut output = String::new();
    let mut inserted = false;
    for line in original.split_inclusive('\n') {
        output.push_str(line);
        if !inserted && line.trim_end() == "metadata:" {
            output.push_str("  lifecycleStatus: archived\n");
            output.push_str(&format!(
                "  archivedAt: {}\n",
                serde_json::to_string(archived_at).expect("date should serialize")
            ));
            output.push_str(&format!(
                "  archiveReason: {}\n",
                serde_json::to_string(reason).expect("reason should serialize")
            ));
            output.push_str(&format!(
                "  supersededBy: {}\n",
                serde_json::to_string(superseded_by).expect("owners should serialize")
            ));
            inserted = true;
        }
    }
    if !inserted {
        return Err(ArchiveFailure::runtime(
            "source note frontmatter has no metadata map",
        ));
    }
    Ok(output)
}

fn append_archive_index(
    original: Option<&str>,
    slug: &str,
    name: &str,
    description: &str,
    archived_at: &str,
    reason: &str,
    superseded_by: &[String],
) -> String {
    let mut output = original.unwrap_or(
        "# Superseded memory archive\n\nHistorical provenance only. This index is excluded from active recall.\n\n",
    ).to_string();
    if !output.ends_with('\n') {
        output.push('\n');
    }
    let owners = superseded_by.join(", ");
    let hook = if description.is_empty() {
        reason
    } else {
        description
    };
    output.push_str(&format!(
        "- [{name}](superseded/{slug}.md) — {archived_at}; {hook}; {reason}; superseded by {owners}\n"
    ));
    output
}

fn apply_transaction(
    source: &Path,
    target: &Path,
    archived: &str,
    archive_index: &Path,
    archive_index_existed: bool,
    archive_index_updated: &str,
    index_updates: &[IndexUpdate],
) -> Result<(), ArchiveFailure> {
    let target_dir_existed = target.parent().is_some_and(Path::exists);
    let target_dir = target
        .parent()
        .ok_or_else(|| ArchiveFailure::runtime("archive target has no parent"))?;
    let archive_root = target_dir
        .parent()
        .ok_or_else(|| ArchiveFailure::runtime("archive directory has no parent"))?;
    let archive_root_existed = archive_root.exists();
    create_regular_dir(archive_root, "archive root")?;
    create_regular_dir(target_dir, "superseded archive directory")?;

    let source_backup = sibling(source, ".archive-backup");
    let target_tmp = sibling(target, ".archive-tmp");
    let archive_index_backup = sibling(archive_index, ".archive-backup");
    let archive_index_tmp = sibling(archive_index, ".archive-tmp");
    let index_files: Vec<_> = index_updates
        .iter()
        .map(|update| {
            (
                update,
                sibling(&update.path, ".archive-backup"),
                sibling(&update.path, ".archive-tmp"),
            )
        })
        .collect();

    cleanup_paths(&[
        &source_backup,
        &target_tmp,
        &archive_index_backup,
        &archive_index_tmp,
    ]);
    for (_, backup, temporary) in &index_files {
        cleanup_paths(&[backup, temporary]);
    }
    let staging = (|| -> Result<(), ArchiveFailure> {
        write_new(&target_tmp, archived)?;
        write_new(&archive_index_tmp, archive_index_updated)?;
        for (update, _, temporary) in &index_files {
            write_new(temporary, &update.updated)?;
        }
        Ok(())
    })();
    if let Err(error) = staging {
        cleanup_paths(&[&target_tmp, &archive_index_tmp]);
        for (_, _, temporary) in &index_files {
            cleanup_paths(&[temporary]);
        }
        if !target_dir_existed {
            let _ = fs::remove_dir(target_dir);
        }
        if !archive_root_existed {
            let _ = fs::remove_dir(archive_root);
        }
        return Err(error);
    }

    let mut progress = TransactionProgress::default();
    let operation = (|| -> Result<(), ArchiveFailure> {
        fs::rename(source, &source_backup).map_err(|err| {
            ArchiveFailure::runtime(format!("failed to back up {}: {err}", display_path(source)))
        })?;
        progress.source_backed_up = true;

        if archive_index_existed {
            fs::rename(archive_index, &archive_index_backup).map_err(|err| {
                ArchiveFailure::runtime(format!(
                    "failed to back up {}: {err}",
                    display_path(archive_index)
                ))
            })?;
            progress.archive_index_backed_up = true;
        }
        for (position, (update, backup, _)) in index_files.iter().enumerate() {
            fs::rename(&update.path, backup).map_err(|err| {
                ArchiveFailure::runtime(format!(
                    "failed to back up {}: {err}",
                    display_path(&update.path)
                ))
            })?;
            progress.active_indexes_backed_up = position + 1;
        }

        fs::rename(&target_tmp, target).map_err(|err| {
            ArchiveFailure::runtime(format!("failed to install {}: {err}", display_path(target)))
        })?;
        progress.target_installed = true;
        fs::rename(&archive_index_tmp, archive_index).map_err(|err| {
            ArchiveFailure::runtime(format!(
                "failed to install {}: {err}",
                display_path(archive_index)
            ))
        })?;
        progress.archive_index_installed = true;
        for (position, (update, _, temporary)) in index_files.iter().enumerate() {
            fs::rename(temporary, &update.path).map_err(|err| {
                ArchiveFailure::runtime(format!(
                    "failed to install {}: {err}",
                    display_path(&update.path)
                ))
            })?;
            progress.active_indexes_installed = position + 1;
        }
        Ok(())
    })();

    if let Err(error) = operation {
        let rollback_errors = rollback_transaction(
            source,
            target,
            archive_index,
            &source_backup,
            &archive_index_backup,
            &index_files,
            &progress,
        );
        cleanup_paths(&[&target_tmp, &archive_index_tmp]);
        for (_, _, temporary) in &index_files {
            cleanup_paths(&[temporary]);
        }
        if !target_dir_existed {
            let _ = fs::remove_dir(target_dir);
        }
        if !archive_root_existed {
            let _ = fs::remove_dir(archive_root);
        }
        if rollback_errors.is_empty() {
            return Err(error);
        }
        return Err(ArchiveFailure::runtime(format!(
            "{}; rollback errors: {}",
            error.message(),
            rollback_errors.join("; ")
        )));
    }

    cleanup_paths(&[&source_backup, &archive_index_backup]);
    for (_, backup, _) in &index_files {
        cleanup_paths(&[backup]);
    }
    Ok(())
}

#[derive(Default)]
struct TransactionProgress {
    source_backed_up: bool,
    archive_index_backed_up: bool,
    active_indexes_backed_up: usize,
    target_installed: bool,
    archive_index_installed: bool,
    active_indexes_installed: usize,
}

fn rollback_transaction(
    source: &Path,
    target: &Path,
    archive_index: &Path,
    source_backup: &Path,
    archive_index_backup: &Path,
    index_files: &[(&IndexUpdate, PathBuf, PathBuf)],
    progress: &TransactionProgress,
) -> Vec<String> {
    let mut errors = Vec::new();
    for position in (0..progress.active_indexes_installed).rev() {
        let (update, _, _) = &index_files[position];
        collect_remove(&update.path, "remove installed active index", &mut errors);
    }
    if progress.archive_index_installed {
        collect_remove(archive_index, "remove installed archive index", &mut errors);
    }
    if progress.target_installed {
        collect_remove(target, "remove installed archived note", &mut errors);
    }
    for position in (0..progress.active_indexes_backed_up).rev() {
        let (update, backup, _) = &index_files[position];
        collect_rename(backup, &update.path, "restore active index", &mut errors);
    }
    if progress.archive_index_backed_up {
        collect_rename(
            archive_index_backup,
            archive_index,
            "restore archive index",
            &mut errors,
        );
    }
    if progress.source_backed_up {
        collect_rename(source_backup, source, "restore curated source", &mut errors);
    }
    errors
}

fn read_regular_file(path: &Path, label: &str) -> Result<String, ArchiveFailure> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        ArchiveFailure::runtime(format!(
            "{label} not found at {}: {err}",
            display_path(path)
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ArchiveFailure::runtime(format!(
            "{label} must be a regular, non-symlink file: {}",
            display_path(path)
        )));
    }
    fs::read_to_string(path).map_err(|err| {
        ArchiveFailure::runtime(format!("failed to read {}: {err}", display_path(path)))
    })
}

fn ensure_regular_dir(path: &Path, label: &str) -> Result<(), ArchiveFailure> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        ArchiveFailure::runtime(format!(
            "{label} not found at {}: {err}",
            display_path(path)
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ArchiveFailure::runtime(format!(
            "{label} must be a regular, non-symlink directory: {}",
            display_path(path)
        )));
    }
    Ok(())
}

fn reject_symlink_ancestor(path: &Path, label: &str) -> Result<(), ArchiveFailure> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(ArchiveFailure::runtime(format!(
            "{label} must not be a symlink: {}",
            display_path(path)
        ))),
        Ok(metadata) if !metadata.is_dir() => Err(ArchiveFailure::runtime(format!(
            "{label} must be a directory: {}",
            display_path(path)
        ))),
        Ok(_) | Err(_) => Ok(()),
    }
}

fn create_regular_dir(path: &Path, label: &str) -> Result<(), ArchiveFailure> {
    if path.exists() {
        return ensure_regular_dir(path, label);
    }
    fs::create_dir(path).map_err(|err| {
        ArchiveFailure::runtime(format!("failed to create {}: {err}", display_path(path)))
    })
}

fn write_new(path: &Path, contents: &str) -> Result<(), ArchiveFailure> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| {
            ArchiveFailure::runtime(format!("failed to create {}: {err}", display_path(path)))
        })?;
    file.write_all(contents.as_bytes()).map_err(|err| {
        ArchiveFailure::runtime(format!("failed to write {}: {err}", display_path(path)))
    })
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".to_string());
    path.with_file_name(format!(".{name}{suffix}"))
}

fn cleanup_paths(paths: &[&Path]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn collect_remove(path: &Path, label: &str, errors: &mut Vec<String>) {
    if let Err(err) = fs::remove_file(path) {
        errors.push(format!("{label} {}: {err}", display_path(path)));
    }
}

fn collect_rename(source: &Path, target: &Path, label: &str, errors: &mut Vec<String>) {
    if let Err(err) = fs::rename(source, target) {
        errors.push(format!(
            "{label} {} -> {}: {err}",
            display_path(source),
            display_path(target)
        ));
    }
}

fn output_format(format: OutputFormat, json: bool) -> OutputFormat {
    if json { OutputFormat::Json } else { format }
}

#[derive(Debug)]
enum ArchiveFailure {
    Cli(CliError),
    Blocked(Vec<ActiveReference>),
}

impl ArchiveFailure {
    fn runtime(message: impl Into<String>) -> Self {
        Self::Cli(CliError::runtime(message))
    }

    fn usage(message: impl Into<String>) -> Self {
        Self::Cli(CliError::usage(message))
    }

    fn from_cli(error: CliError) -> Self {
        Self::Cli(error)
    }

    fn exit_code(&self) -> i32 {
        match self {
            Self::Cli(error) => error.exit_code,
            Self::Blocked(_) => EXIT_RUNTIME,
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Cli(error) => error.message.clone(),
            Self::Blocked(blockers) => format!(
                "active references block retirement ({} finding(s))",
                blockers.len()
            ),
        }
    }

    fn into_cli_error(self) -> CliError {
        match self {
            Self::Cli(error) => error,
            Self::Blocked(blockers) => CliError::runtime(format!(
                "active references block retirement: {}",
                blockers
                    .iter()
                    .map(|blocker| format!("{}:{}", blocker.file, blocker.line))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

fn print_json_error(schema_command: &str, failure: &ArchiveFailure) {
    let (code, blockers) = match failure {
        ArchiveFailure::Blocked(blockers) => (
            "active-reference-blocked",
            blockers
                .iter()
                .map(|blocker| {
                    json!({
                        "file": blocker.file,
                        "line": blocker.line,
                        "text": blocker.text,
                    })
                })
                .collect::<Vec<_>>(),
        ),
        ArchiveFailure::Cli(error) if error.exit_code == EXIT_USAGE => ("usage-error", Vec::new()),
        ArchiveFailure::Cli(_) => ("runtime-error", Vec::new()),
    };
    println!(
        "{}",
        serde_json::to_string(&json!({
            "schema_version": schema_version_for("agent-memory", schema_command, 1),
            "ok": false,
            "error": {
                "code": code,
                "message": failure.message(),
            },
            "blockers": blockers,
        }))
        .expect("archive error should serialize")
    );
}

#[cfg(test)]
mod tests {
    use super::{add_archive_metadata, remove_index_link};

    #[test]
    fn metadata_is_inserted_under_existing_map() {
        let original = "---\nname: example\ndescription: example\nmetadata:\n  node_type: memory\n  type: feedback\n---\n\nBody.\n";
        let archived = add_archive_metadata(
            original,
            "2026-07-12",
            "enforced-by-runtime",
            &["nils-cli:crates/example/src/lib.rs".to_string()],
        )
        .expect("metadata");
        assert!(archived.contains("  lifecycleStatus: archived\n"));
        assert!(archived.contains("  supersededBy: [\"nils-cli:crates/example/src/lib.rs\"]"));
    }

    #[test]
    fn index_removal_is_scope_aware() {
        let global = "# Global\n- [One](one.md) — keep\n- [Old](old.md) — remove\n";
        assert_eq!(
            remove_index_link(global, "global", "old"),
            "# Global\n- [One](one.md) — keep\n"
        );
        let startup = "# Startup\n- [Old](../../global/old.md) — remove\n";
        assert_eq!(
            remove_index_link(startup, "profiles/startup", "old"),
            "# Startup\n"
        );
    }

    #[test]
    fn rollback_restores_source_and_active_indexes() {
        use std::fs;

        use super::{IndexUpdate, TransactionProgress, rollback_transaction, sibling};

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let source = tmp.path().join("global/source.md");
        let target = tmp.path().join("archive/superseded/source.md");
        let archive_index = tmp.path().join("archive/MEMORY.md");
        let active_index = tmp.path().join("global/MEMORY.md");
        fs::create_dir_all(source.parent().expect("source parent")).expect("global");
        fs::create_dir_all(target.parent().expect("target parent")).expect("archive");
        fs::write(&target, "archived").expect("target");
        fs::write(&archive_index, "new archive index").expect("archive index");
        fs::write(&active_index, "new active index").expect("active index");

        let source_backup = sibling(&source, ".archive-backup");
        let archive_index_backup = sibling(&archive_index, ".archive-backup");
        let active_index_backup = sibling(&active_index, ".archive-backup");
        fs::write(&source_backup, "source original").expect("source backup");
        fs::write(&archive_index_backup, "archive index original").expect("archive backup");
        fs::write(&active_index_backup, "active index original").expect("active backup");
        let update = IndexUpdate {
            path: active_index.clone(),
            display: "global/MEMORY.md".to_string(),
            updated: "new active index".to_string(),
        };
        let temporary = sibling(&active_index, ".archive-tmp");
        let files = vec![(&update, active_index_backup, temporary)];
        let errors = rollback_transaction(
            &source,
            &target,
            &archive_index,
            &source_backup,
            &archive_index_backup,
            &files,
            &TransactionProgress {
                source_backed_up: true,
                archive_index_backed_up: true,
                active_indexes_backed_up: 1,
                target_installed: true,
                archive_index_installed: true,
                active_indexes_installed: 1,
            },
        );

        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            fs::read_to_string(source).expect("source"),
            "source original"
        );
        assert_eq!(
            fs::read_to_string(archive_index).expect("archive index"),
            "archive index original"
        );
        assert_eq!(
            fs::read_to_string(active_index).expect("active index"),
            "active index original"
        );
        assert!(!target.exists());
    }
}
