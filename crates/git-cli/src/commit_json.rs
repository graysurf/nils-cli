use crate::clipboard;
use crate::commit_shared::{
    DiffNumstat, diff_numstat, git_output, git_status_code, git_stdout_trimmed_optional,
    is_lockfile, parse_name_status_z,
};
use anyhow::{Result, anyhow};
use nils_common::git::{self as common_git, GitContextError};
use serde_json::{Map, Number, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;
use time::OffsetDateTime;
use time::format_description;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputMode {
    Clipboard,
    Stdout,
    Both,
}

struct ContextJsonArgs {
    mode: OutputMode,
    pretty: bool,
    bundle: bool,
    out_dir: Option<String>,
    extra_args: Vec<String>,
}

enum ParseOutcome<T> {
    Continue(T),
    Exit(i32),
}

pub fn run(args: &[String]) -> i32 {
    match common_git::require_work_tree() {
        Ok(()) => {}
        Err(GitContextError::GitNotFound) => {
            eprintln!("❗ git is required but was not found in PATH.");
            return 1;
        }
        Err(GitContextError::NotRepository) => {
            eprintln!("❌ Not a git repository.");
            return 1;
        }
    }

    let parsed = match parse_args(args) {
        ParseOutcome::Continue(value) => value,
        ParseOutcome::Exit(code) => return code,
    };

    if !parsed.extra_args.is_empty() {
        eprintln!(
            "⚠️  Ignoring unknown arguments: {}",
            parsed.extra_args.join(" ")
        );
    }

    match git_status_code(&["diff", "--cached", "--quiet", "--exit-code"]) {
        Some(0) => {
            eprintln!("⚠️  No staged changes to record");
            return 1;
        }
        Some(1) => {}
        _ => {
            eprintln!("❌ Failed to check staged changes.");
            return 1;
        }
    }

    let out_dir = match resolve_out_dir(parsed.out_dir.as_deref()) {
        Ok(dir) => dir,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };

    if fs::create_dir_all(&out_dir).is_err() {
        eprintln!("❌ Failed to create output directory: {out_dir}");
        return 1;
    }

    let patch_path = format!("{out_dir}/staged.patch");
    let manifest_path = format!("{out_dir}/commit-context.json");

    let patch_bytes = match git_output(&[
        "-c",
        "core.quotepath=false",
        "diff",
        "--cached",
        "--no-color",
    ]) {
        Ok(output) => output.stdout,
        Err(_) => {
            eprintln!("❌ Failed to write staged patch: {patch_path}");
            return 1;
        }
    };

    if fs::write(&patch_path, &patch_bytes).is_err() {
        eprintln!("❌ Failed to write staged patch: {patch_path}");
        return 1;
    }

    let json = match build_json(parsed.pretty) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("{err:#}");
            return 1;
        }
    };

    if fs::write(&manifest_path, format!("{json}\n")).is_err() {
        eprintln!("❌ Failed to write JSON manifest: {manifest_path}");
        return 1;
    }

    let patch_text = String::from_utf8_lossy(&patch_bytes).to_string();

    match parsed.mode {
        OutputMode::Stdout => {
            print_bundle_or_json(&json, &patch_text, parsed.bundle);
            return 0;
        }
        OutputMode::Both => {
            print_bundle_or_json(&json, &patch_text, parsed.bundle);
        }
        OutputMode::Clipboard => {}
    }

    if parsed.bundle {
        let bundle = build_bundle(&json, &patch_text);
        let _ = clipboard::set_clipboard_best_effort(&bundle);
    } else {
        let _ = clipboard::set_clipboard_best_effort(&json);
    }

    if parsed.mode == OutputMode::Clipboard {
        println!("✅ JSON commit context copied to clipboard with:");
        if parsed.bundle {
            println!("  • Bundle (JSON + patch)");
        } else {
            println!("  • JSON manifest");
        }
        println!("  • Patch file written to: {patch_path}");
        println!("  • Manifest file written to: {manifest_path}");
    }

    0
}

fn parse_args(args: &[String]) -> ParseOutcome<ContextJsonArgs> {
    let mut mode = OutputMode::Clipboard;
    let mut pretty = false;
    let mut bundle = false;
    let mut out_dir: Option<String> = None;
    let mut extra_args: Vec<String> = Vec::new();

    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--stdout" | "-p" | "--print" => mode = OutputMode::Stdout,
            "--both" => mode = OutputMode::Both,
            "--pretty" => pretty = true,
            "--bundle" => bundle = true,
            "--out-dir" => {
                let value = iter.next().map(|v| v.to_string()).unwrap_or_default();
                if value.is_empty() {
                    eprintln!("❌ Missing value for --out-dir");
                    return ParseOutcome::Exit(2);
                }
                out_dir = Some(value);
            }
            value if value.starts_with("--out-dir=") => {
                let value = value.trim_start_matches("--out-dir=").to_string();
                out_dir = Some(value);
            }
            "--help" | "-h" => {
                print_usage();
                return ParseOutcome::Exit(0);
            }
            other => extra_args.push(other.to_string()),
        }
    }

    ParseOutcome::Continue(ContextJsonArgs {
        mode,
        pretty,
        bundle,
        out_dir,
        extra_args,
    })
}

fn print_usage() {
    println!(
        "Usage: git-cli commit context-json [--stdout|--both] [--pretty] [--bundle] [--out-dir <path>]"
    );
    println!("  --stdout    Print to stdout only (JSON by default; bundle with --bundle)");
    println!(
        "  --both      Print to stdout and copy to clipboard (JSON by default; bundle with --bundle)"
    );
    println!("  --pretty    Pretty-print JSON (default is compact)");
    println!("  --bundle    Print/copy a single bundle (JSON + patch content)");
    println!("  --out-dir   Write files to this directory (default: <git-dir>/commit-context)");
}

fn resolve_out_dir(out_dir: Option<&str>) -> Result<String> {
    let trimmed = out_dir.map(|value| value.trim()).unwrap_or("");
    if !trimmed.is_empty() {
        return Ok(trimmed.trim_end_matches('/').to_string());
    }

    let git_dir = git_stdout_trimmed_optional(&["rev-parse", "--git-dir"]).unwrap_or_default();
    if git_dir.is_empty() {
        return Err(anyhow!("❌ Failed to resolve git dir."));
    }

    Ok(format!("{}/commit-context", git_dir.trim_end_matches('/')))
}

fn build_json(pretty: bool) -> Result<String> {
    let branch = git_stdout_trimmed_optional(&["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let head = git_stdout_trimmed_optional(&["rev-parse", "--short", "HEAD"]);
    let repo_name =
        git_stdout_trimmed_optional(&["rev-parse", "--show-toplevel"]).and_then(|path| {
            Path::new(&path)
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        });
    let generated_at = generated_at();

    let name_status = git_output(&[
        "-c",
        "core.quotepath=false",
        "diff",
        "--cached",
        "--name-status",
        "-z",
    ])?;

    let entries = parse_name_status_z(&name_status.stdout)?;

    let mut status_counts: BTreeMap<String, i64> = BTreeMap::new();
    let mut top_dir_counts: BTreeMap<String, i64> = BTreeMap::new();

    let mut insertions: i64 = 0;
    let mut deletions: i64 = 0;
    let mut file_count: i64 = 0;
    let mut binary_file_count: i64 = 0;
    let mut lockfile_count: i64 = 0;
    let mut root_file_count: i64 = 0;

    let mut files: Vec<Value> = Vec::new();

    for entry in entries {
        file_count += 1;

        let status_letter = entry
            .status_raw
            .chars()
            .next()
            .map(|ch| ch.to_string())
            .unwrap_or_else(|| "".to_string());

        *status_counts.entry(status_letter.clone()).or_insert(0) += 1;

        if let Some((top, _)) = entry.path.split_once('/') {
            *top_dir_counts.entry(top.to_string()).or_insert(0) += 1;
        } else {
            root_file_count += 1;
        }

        let lockfile = is_lockfile(&entry.path);
        if lockfile {
            lockfile_count += 1;
        }

        let diff = diff_numstat(&entry.path).unwrap_or(DiffNumstat {
            added: None,
            deleted: None,
            binary: false,
        });

        if diff.binary {
            binary_file_count += 1;
        } else {
            if let Some(n) = diff.added {
                insertions += n;
            }
            if let Some(n) = diff.deleted {
                deletions += n;
            }
        }

        let mut file_obj = Map::new();
        file_obj.insert("path".to_string(), Value::String(entry.path.clone()));
        file_obj.insert("status".to_string(), Value::String(status_letter));

        if entry.status_raw.len() > 1 {
            let score_raw = &entry.status_raw[1..];
            if let Ok(score) = score_raw.parse::<i64>() {
                file_obj.insert("score".to_string(), Value::Number(Number::from(score)));
            }
        }

        if let Some(old_path) = entry.old_path.as_ref() {
            file_obj.insert("oldPath".to_string(), Value::String(old_path.clone()));
        }

        if diff.binary {
            file_obj.insert("insertions".to_string(), Value::Null);
            file_obj.insert("deletions".to_string(), Value::Null);
        } else {
            match diff.added {
                Some(n) => {
                    file_obj.insert("insertions".to_string(), Value::Number(Number::from(n)));
                }
                None => {
                    file_obj.insert("insertions".to_string(), Value::Null);
                }
            }
            match diff.deleted {
                Some(n) => {
                    file_obj.insert("deletions".to_string(), Value::Number(Number::from(n)));
                }
                None => {
                    file_obj.insert("deletions".to_string(), Value::Null);
                }
            }
        }

        file_obj.insert("binary".to_string(), Value::Bool(diff.binary));
        file_obj.insert("lockfile".to_string(), Value::Bool(lockfile));

        files.push(Value::Object(file_obj));
    }

    let status_counts_values: Vec<Value> = status_counts
        .into_iter()
        .map(|(status, count)| {
            let mut obj = Map::new();
            obj.insert("status".to_string(), Value::String(status));
            obj.insert("count".to_string(), Value::Number(Number::from(count)));
            Value::Object(obj)
        })
        .collect();

    let top_dir_values: Vec<Value> = top_dir_counts
        .into_iter()
        .map(|(name, count)| {
            let mut obj = Map::new();
            obj.insert("name".to_string(), Value::String(name));
            obj.insert("count".to_string(), Value::Number(Number::from(count)));
            Value::Object(obj)
        })
        .collect();

    let mut summary = Map::new();
    summary.insert(
        "fileCount".to_string(),
        Value::Number(Number::from(file_count)),
    );
    summary.insert(
        "insertions".to_string(),
        Value::Number(Number::from(insertions)),
    );
    summary.insert(
        "deletions".to_string(),
        Value::Number(Number::from(deletions)),
    );
    summary.insert(
        "binaryFileCount".to_string(),
        Value::Number(Number::from(binary_file_count)),
    );
    summary.insert(
        "lockfileCount".to_string(),
        Value::Number(Number::from(lockfile_count)),
    );
    summary.insert(
        "rootFileCount".to_string(),
        Value::Number(Number::from(root_file_count)),
    );
    summary.insert(
        "topLevelDirCount".to_string(),
        Value::Number(Number::from(top_dir_values.len() as i64)),
    );

    let mut staged = Map::new();
    staged.insert("summary".to_string(), Value::Object(summary));
    staged.insert(
        "statusCounts".to_string(),
        Value::Array(status_counts_values),
    );

    let mut structure = Map::new();
    structure.insert("topLevelDirs".to_string(), Value::Array(top_dir_values));
    staged.insert("structure".to_string(), Value::Object(structure));
    staged.insert("files".to_string(), Value::Array(files));

    let mut patch = Map::new();
    patch.insert(
        "path".to_string(),
        Value::String("staged.patch".to_string()),
    );
    patch.insert(
        "format".to_string(),
        Value::String("git diff --cached".to_string()),
    );
    staged.insert("patch".to_string(), Value::Object(patch));

    let mut repo = Map::new();
    repo.insert(
        "name".to_string(),
        repo_name.map(Value::String).unwrap_or(Value::Null),
    );

    let mut git = Map::new();
    git.insert(
        "branch".to_string(),
        branch.map(Value::String).unwrap_or(Value::Null),
    );
    git.insert(
        "head".to_string(),
        head.map(Value::String).unwrap_or(Value::Null),
    );

    let mut root = Map::new();
    root.insert("schemaVersion".to_string(), Value::Number(Number::from(1)));
    root.insert(
        "generatedAt".to_string(),
        generated_at.map(Value::String).unwrap_or(Value::Null),
    );
    root.insert("repo".to_string(), Value::Object(repo));
    root.insert("git".to_string(), Value::Object(git));
    root.insert("staged".to_string(), Value::Object(staged));

    let value = Value::Object(root);

    if pretty {
        Ok(serde_json::to_string_pretty(&value)?)
    } else {
        Ok(serde_json::to_string(&value)?)
    }
}

fn generated_at() -> Option<String> {
    if env::var("GIT_CLI_FIXTURE_DATE_MODE").ok().as_deref() == Some("fixed") {
        return Some("2000-01-02T03:04:05Z".to_string());
    }

    let format =
        format_description::parse_borrowed::<2>("[year]-[month]-[day]T[hour]:[minute]:[second]Z")
            .ok()?;
    OffsetDateTime::now_utc().format(&format).ok()
}

fn print_bundle_or_json(json: &str, patch_text: &str, bundle: bool) {
    if bundle {
        print!("{}", build_bundle(json, patch_text));
    } else {
        println!("{json}");
    }
}

fn build_bundle(json: &str, patch_text: &str) -> String {
    let mut out = String::new();
    out.push_str("===== commit-context.json =====\n");
    out.push_str(json);
    out.push('\n');
    out.push('\n');
    out.push_str("===== staged.patch =====\n");
    out.push_str(patch_text);
    out
}
