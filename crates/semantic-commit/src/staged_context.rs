use crate::git;
use nils_common::cli_contract::exit;
use nils_common::git as common_git;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Output;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const EXIT_ERROR: i32 = exit::RUNTIME;
const EXIT_NO_STAGED_CHANGES: i32 = 2;
const EXIT_DEPENDENCY_ERROR: i32 = 5;
const CAT_PAGER_ENV: [(&str, &str); 2] = [("GIT_PAGER", "cat"), ("PAGER", "cat")];
const STAGED_CONTEXT_SCHEMA_VERSION: &str = "cli.semantic-commit.staged-context.v2";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputFormat {
    Bundle,
    Json,
    Patch,
}

impl OutputFormat {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "bundle" => Some(Self::Bundle),
            "json" => Some(Self::Json),
            "patch" => Some(Self::Patch),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct StagedContextOptions {
    format: OutputFormat,
    repo: Option<PathBuf>,
}

impl Default for StagedContextOptions {
    fn default() -> Self {
        Self {
            format: OutputFormat::Bundle,
            repo: None,
        }
    }
}

#[derive(Debug)]
struct Bundle {
    context_json: String,
    patch: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct NumstatEntry {
    insertions: Option<i64>,
    deletions: Option<i64>,
    binary: bool,
}

pub fn run(args: &[String]) -> i32 {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(code) => return code,
    };

    if !git::command_exists("git") {
        eprintln!("error: git is required (ensure it is installed and on PATH)");
        return EXIT_DEPENDENCY_ERROR;
    }

    if !git::is_inside_work_tree(options.repo.as_deref()) {
        eprintln!("error: must run inside a git work tree");
        return EXIT_ERROR;
    }

    match git::has_staged_changes(options.repo.as_deref()) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("error: no staged changes (stage files with git add first)");
            return EXIT_NO_STAGED_CHANGES;
        }
        Err(err) => {
            eprintln!("{err:#}");
            return EXIT_ERROR;
        }
    }

    let bundle = match build_bundle(options.repo.as_deref()) {
        Ok(bundle) => bundle,
        Err(err) => {
            eprintln!("{err:#}");
            return EXIT_ERROR;
        }
    };

    match options.format {
        OutputFormat::Bundle => {
            println!("===== commit-context.json =====");
            println!("{}", bundle.context_json);
            println!();
            println!("===== staged.patch =====");
            if let Err(err) = std::io::stdout().write_all(&bundle.patch) {
                eprintln!("{err:#}");
                return EXIT_ERROR;
            }
        }
        OutputFormat::Json => {
            println!("{}", bundle.context_json);
        }
        OutputFormat::Patch => {
            if let Err(err) = std::io::stdout().write_all(&bundle.patch) {
                eprintln!("{err:#}");
                return EXIT_ERROR;
            }
        }
    }

    0
}

fn parse_args(args: &[String]) -> Result<StagedContextOptions, i32> {
    let mut options = StagedContextOptions::default();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_usage_stdout();
                return Err(0);
            }
            "--format" => {
                let value = match args.get(i + 1) {
                    Some(value) => value,
                    None => {
                        eprintln!("error: --format requires a value");
                        print_usage_stderr();
                        return Err(EXIT_ERROR);
                    }
                };

                let Some(format) = OutputFormat::parse(value) else {
                    eprintln!(
                        "error: invalid --format value: {value} (expected: bundle, json, patch)"
                    );
                    print_usage_stderr();
                    return Err(EXIT_ERROR);
                };

                options.format = format;
                i += 2;
            }
            "--json" => {
                options.format = OutputFormat::Json;
                i += 1;
            }
            "--repo" => {
                let value = match args.get(i + 1) {
                    Some(value) => value.clone(),
                    None => {
                        eprintln!("error: --repo requires a path");
                        print_usage_stderr();
                        return Err(EXIT_ERROR);
                    }
                };
                options.repo = Some(PathBuf::from(value));
                i += 2;
            }
            other => {
                eprintln!("error: unknown argument: {other}");
                print_usage_stderr();
                return Err(EXIT_ERROR);
            }
        }
    }

    Ok(options)
}

fn build_bundle(repo: Option<&Path>) -> anyhow::Result<Bundle> {
    let generated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .ok()
        .map(|s| s.to_string());

    let repo_root = git_string(repo, &["rev-parse", "--show-toplevel"])?;
    let repo_name = Path::new(&repo_root)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|s| s.to_string());

    let branch = git_string_ok(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let head = git_string_ok(repo, &["rev-parse", "--short", "HEAD"]);

    let name_status = git_bytes(
        repo,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--cached",
            "--name-status",
            "-z",
        ],
    )?;
    let numstats = diff_numstat_map(repo)?;

    let mut status_counts: BTreeMap<String, u32> = BTreeMap::new();
    let mut top_dir_counts: BTreeMap<String, u32> = BTreeMap::new();
    let mut insertions: i64 = 0;
    let mut deletions: i64 = 0;
    let mut file_count: i64 = 0;
    let mut binary_file_count: i64 = 0;
    let mut lockfile_count: i64 = 0;
    let mut root_file_count: i64 = 0;

    let mut files: Vec<Value> = Vec::new();
    for entry in parse_name_status_z(&name_status)? {
        file_count += 1;

        *status_counts.entry(entry.status.clone()).or_insert(0) += 1;

        if let Some((top, _)) = entry.path.split_once('/') {
            *top_dir_counts.entry(top.to_string()).or_insert(0) += 1;
        } else {
            root_file_count += 1;
        }

        let lockfile = is_lockfile(&entry.path);
        if lockfile {
            lockfile_count += 1;
        }

        let file_stats = numstats.get(&entry.path).copied().unwrap_or_default();
        if file_stats.binary {
            binary_file_count += 1;
        } else {
            if let Some(n) = file_stats.insertions {
                insertions += n;
            }
            if let Some(n) = file_stats.deletions {
                deletions += n;
            }
        }

        let mut obj = serde_json::Map::new();
        obj.insert("path".to_string(), json!(entry.path));
        obj.insert("status".to_string(), json!(entry.status));
        if let Some(score) = entry.score {
            obj.insert("score".to_string(), json!(score));
        }
        if let Some(old_path) = entry.old_path {
            obj.insert("old_path".to_string(), json!(old_path));
            obj.insert("oldPath".to_string(), json!(old_path));
        }
        obj.insert(
            "insertions".to_string(),
            file_stats.insertions.map_or(Value::Null, |n| json!(n)),
        );
        obj.insert(
            "deletions".to_string(),
            file_stats.deletions.map_or(Value::Null, |n| json!(n)),
        );
        obj.insert("binary".to_string(), json!(file_stats.binary));
        obj.insert("lockfile".to_string(), json!(lockfile));
        files.push(Value::Object(obj));
    }

    let status_counts: Vec<Value> = status_counts
        .into_iter()
        .map(|(status, count)| json!({ "status": status, "count": count }))
        .collect();

    let top_level_dirs: Vec<Value> = top_dir_counts
        .iter()
        .map(|(name, count)| json!({ "name": name, "count": count }))
        .collect();

    let top_level_dir_count = top_level_dirs.len();
    let status_counts_alias = status_counts.clone();
    let top_level_dirs_alias = top_level_dirs.clone();
    let generated_at_alias = generated_at.clone();
    let context = json!({
        "schema_version": STAGED_CONTEXT_SCHEMA_VERSION,
        "schemaVersion": 1,
        "generated_at": generated_at,
        "generatedAt": generated_at_alias,
        "repo": { "name": repo_name },
        "git": { "branch": branch, "head": head },
        "staged": {
            "summary": {
                "file_count": file_count,
                "fileCount": file_count,
                "insertions": insertions,
                "deletions": deletions,
                "binary_file_count": binary_file_count,
                "binaryFileCount": binary_file_count,
                "lockfile_count": lockfile_count,
                "lockfileCount": lockfile_count,
                "root_file_count": root_file_count,
                "rootFileCount": root_file_count,
                "top_level_dir_count": top_level_dir_count,
                "topLevelDirCount": top_level_dir_count,
            },
            "status_counts": status_counts,
            "statusCounts": status_counts_alias,
            "structure": {
                "top_level_dirs": top_level_dirs,
                "topLevelDirs": top_level_dirs_alias,
            },
            "files": files,
            "patch": { "path": "staged.patch", "format": "git diff --cached" }
        },
    });

    let context_json = serde_json::to_string(&context)?;
    let patch = git_bytes(
        repo,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--cached",
            "--no-color",
        ],
    )?;

    Ok(Bundle {
        context_json,
        patch,
    })
}

struct NameStatusEntry {
    status: String,
    score: Option<u32>,
    path: String,
    old_path: Option<String>,
}

fn parse_name_status_z(buf: &[u8]) -> anyhow::Result<Vec<NameStatusEntry>> {
    let parts = common_git::parse_name_status_z(buf).map_err(|err| anyhow::anyhow!("{err}"))?;
    let mut out: Vec<NameStatusEntry> = Vec::new();

    for entry in parts {
        let raw_status = std::str::from_utf8(entry.status_raw)?;
        let path = std::str::from_utf8(entry.path)?.to_string();
        let old_path = match entry.old_path {
            Some(path) => Some(std::str::from_utf8(path)?.to_string()),
            None => None,
        };

        let status_letter = raw_status
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("error: malformed name-status output"))?;
        let status = status_letter.to_string();

        let score = raw_status[status_letter.len_utf8()..].parse::<u32>().ok();

        out.push(NameStatusEntry {
            status,
            score,
            path,
            old_path,
        });
    }

    Ok(out)
}

fn diff_numstat_map(repo: Option<&Path>) -> anyhow::Result<BTreeMap<String, NumstatEntry>> {
    let stdout = git_bytes(
        repo,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--cached",
            "--numstat",
            "-z",
        ],
    )?;

    parse_numstat_z(&stdout)
}

fn parse_numstat_z(buf: &[u8]) -> anyhow::Result<BTreeMap<String, NumstatEntry>> {
    let mut map: BTreeMap<String, NumstatEntry> = BTreeMap::new();

    let mut i = 0;
    while i < buf.len() {
        let added = read_field(buf, &mut i, b'\t')?;
        let deleted = read_field(buf, &mut i, b'\t')?;

        let path = if i < buf.len() && buf[i] == 0 {
            i += 1;
            let _old_path = read_field(buf, &mut i, b'\0')?;
            read_field(buf, &mut i, b'\0')?
        } else {
            read_field(buf, &mut i, b'\0')?
        };

        let (insertions, deletions, binary) = if added == "-" || deleted == "-" {
            (None, None, true)
        } else {
            (parse_i64_opt(&added), parse_i64_opt(&deleted), false)
        };

        map.insert(
            path,
            NumstatEntry {
                insertions,
                deletions,
                binary,
            },
        );
    }

    Ok(map)
}

fn parse_i64_opt(value: &str) -> Option<i64> {
    value.parse::<i64>().ok()
}

fn read_field(buf: &[u8], index: &mut usize, delimiter: u8) -> anyhow::Result<String> {
    if *index > buf.len() {
        return Err(anyhow::anyhow!("error: malformed numstat output"));
    }

    let start = *index;
    while *index < buf.len() && buf[*index] != delimiter {
        *index += 1;
    }

    if *index >= buf.len() {
        return Err(anyhow::anyhow!("error: malformed numstat output"));
    }

    let field = std::str::from_utf8(&buf[start..*index])?.to_string();
    *index += 1;
    Ok(field)
}

fn is_lockfile(path: &str) -> bool {
    common_git::is_lockfile_path(path)
}

fn git_string(repo: Option<&Path>, args: &[&str]) -> anyhow::Result<String> {
    let output = git_output(repo, args)?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "error: git command failed: git {}\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_string_ok(repo: Option<&Path>, args: &[&str]) -> Option<String> {
    let output = git_output(repo, args).ok()?;

    if !output.status.success() {
        return None;
    }

    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn git_bytes(repo: Option<&Path>, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = git_output(repo, args)?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "error: git command failed: git {}\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        ));
    }

    Ok(output.stdout)
}

fn git_output(repo: Option<&Path>, args: &[&str]) -> anyhow::Result<Output> {
    match repo {
        Some(repo) => common_git::run_output_in_with_env(repo, args, &CAT_PAGER_ENV),
        None => common_git::run_output_with_env(args, &CAT_PAGER_ENV),
    }
    .map_err(Into::into)
}

fn print_usage_stdout() {
    print_usage(false);
}

fn print_usage_stderr() {
    print_usage(true);
}

fn print_usage(stderr: bool) {
    let out: &mut dyn std::io::Write = if stderr {
        &mut std::io::stderr()
    } else {
        &mut std::io::stdout()
    };

    let _ = writeln!(
        out,
        "Usage: semantic-commit staged-context [--format <bundle|json|patch>] [--repo <path>]"
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Print staged change context for commit message generation."
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "Options:");
    let _ = writeln!(
        out,
        "  --format <mode>  Output format: bundle (default), json, patch"
    );
    let _ = writeln!(out, "  --json           Equivalent to --format json");
    let _ = writeln!(out, "  --repo <path>    Run git commands against repo path");
    let _ = writeln!(out);
    let _ = writeln!(out, "Outputs:");
    let _ = writeln!(out, "  - bundle: commit-context.json + staged.patch");
    let _ = writeln!(out, "  - json: commit-context.json only");
    let _ = writeln!(out, "  - patch: staged.patch only");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_format_parse_supports_known_values() {
        assert_eq!(OutputFormat::parse("bundle"), Some(OutputFormat::Bundle));
        assert_eq!(OutputFormat::parse("json"), Some(OutputFormat::Json));
        assert_eq!(OutputFormat::parse("patch"), Some(OutputFormat::Patch));
        assert_eq!(OutputFormat::parse("other"), None);
    }

    #[test]
    fn parse_numstat_z_parses_standard_rows() {
        let map = parse_numstat_z(b"12\t3\tsrc/main.rs\0").expect("parse numstat");
        let value = map.get("src/main.rs").expect("missing numstat entry");
        assert_eq!(
            *value,
            NumstatEntry {
                insertions: Some(12),
                deletions: Some(3),
                binary: false,
            }
        );
    }

    #[test]
    fn parse_numstat_z_parses_binary_rows() {
        let map = parse_numstat_z(b"-\t-\tassets/logo.bin\0").expect("parse binary numstat");
        let value = map.get("assets/logo.bin").expect("missing numstat entry");
        assert_eq!(
            *value,
            NumstatEntry {
                insertions: None,
                deletions: None,
                binary: true,
            }
        );
    }

    #[test]
    fn parse_numstat_z_parses_rename_rows() {
        let map = parse_numstat_z(b"3\t1\t\0old.txt\0new.txt\0").expect("parse rename numstat");
        let value = map.get("new.txt").expect("missing numstat entry");
        assert_eq!(
            *value,
            NumstatEntry {
                insertions: Some(3),
                deletions: Some(1),
                binary: false,
            }
        );
    }

    #[test]
    fn parse_numstat_z_rejects_malformed_rows() {
        let result = parse_numstat_z(b"12\t3\tno-null-terminator");
        assert!(result.is_err());
    }

    #[test]
    fn parse_name_status_z_parses_basic_entries() {
        let buf = b"A\0src/main.rs\0M\0README.md\0";
        let entries = parse_name_status_z(buf).expect("parse name-status output");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].status, "A");
        assert_eq!(entries[0].path, "src/main.rs");
        assert_eq!(entries[0].score, None);
        assert_eq!(entries[0].old_path, None);

        assert_eq!(entries[1].status, "M");
        assert_eq!(entries[1].path, "README.md");
        assert_eq!(entries[1].score, None);
        assert_eq!(entries[1].old_path, None);
    }

    #[test]
    fn parse_name_status_z_parses_renames_and_copies_with_scores() {
        let buf = b"R087\0old/path.txt\0new/path.txt\0C100\0src/lib.rs\0src/lib_copy.rs\0";
        let entries = parse_name_status_z(buf).expect("parse rename/copy entries");

        assert_eq!(entries.len(), 2);

        assert_eq!(entries[0].status, "R");
        assert_eq!(entries[0].score, Some(87));
        assert_eq!(entries[0].path, "new/path.txt");
        assert_eq!(entries[0].old_path.as_deref(), Some("old/path.txt"));

        assert_eq!(entries[1].status, "C");
        assert_eq!(entries[1].score, Some(100));
        assert_eq!(entries[1].path, "src/lib_copy.rs");
        assert_eq!(entries[1].old_path.as_deref(), Some("src/lib.rs"));
    }

    #[test]
    fn parse_name_status_z_rejects_malformed_rename_entry() {
        let result = parse_name_status_z(b"R100\0old/path.txt\0");
        assert!(result.is_err());
        let err = match result {
            Ok(_) => panic!("expected malformed name-status output"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("malformed name-status output"));
    }

    #[test]
    fn parse_name_status_z_tolerates_invalid_similarity_score() {
        let entries = parse_name_status_z(b"Rxx\0old/path.txt\0new/path.txt\0")
            .expect("parse rename with non-numeric score");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, "R");
        assert_eq!(entries[0].score, None);
        assert_eq!(entries[0].path, "new/path.txt");
        assert_eq!(entries[0].old_path.as_deref(), Some("old/path.txt"));
    }

    #[test]
    fn is_lockfile_matches_known_package_manager_lockfiles() {
        assert!(is_lockfile("yarn.lock"));
        assert!(is_lockfile("frontend/package-lock.json"));
        assert!(is_lockfile("subdir/pnpm-lock.yaml"));
        assert!(is_lockfile("bun.lockb"));
        assert!(is_lockfile("bun.lock"));
        assert!(is_lockfile("npm-shrinkwrap.json"));
        assert!(!is_lockfile("package-lock.json.bak"));
        assert!(!is_lockfile("Cargo.lock"));
    }
}
