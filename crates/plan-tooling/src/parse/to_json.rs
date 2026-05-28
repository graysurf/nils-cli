use std::io::Write;
use std::path::{Path, PathBuf};

use crate::parse::{Plan, parse_plan_with_display};

const USAGE: &str = r#"Usage:
  plan_to_json.sh --file <plan.md> [--sprint <n>] [--pretty]

Purpose:
  Parse a plan markdown file (Plan Format v1) into a stable JSON schema.

Options:
  --file <path>   Plan file to parse (required)
  --sprint <n>    Only include a single sprint number (optional)
  --pretty        Pretty-print JSON (indent=2)
  -h, --help      Show help

Exit:
  0: parsed successfully (JSON on stdout)
  1: parse error (prints error: lines to stderr)
  2: usage error
"#;

fn print_usage() {
    let _ = std::io::stderr().write_all(USAGE.as_bytes());
}

fn die(msg: &str) -> i32 {
    eprintln!("plan_to_json: {msg}");
    2
}

pub fn run(args: &[String]) -> i32 {
    let mut file: Option<String> = None;
    let mut sprint: Option<String> = None;
    let mut pretty = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                let Some(v) = args.get(i + 1) else {
                    return die("missing value for --file");
                };
                if v.is_empty() {
                    return die("missing value for --file");
                }
                file = Some(v.to_string());
                i += 2;
            }
            "--sprint" => {
                let Some(v) = args.get(i + 1) else {
                    return die("missing value for --sprint");
                };
                if v.is_empty() {
                    return die("missing value for --sprint");
                }
                sprint = Some(v.to_string());
                i += 2;
            }
            "--pretty" => {
                pretty = true;
                i += 1;
            }
            "-h" | "--help" => {
                print_usage();
                return 0;
            }
            other => {
                return die(&format!("unknown argument: {other}"));
            }
        }
    }

    let Some(file_arg) = file else {
        print_usage();
        return 2;
    };

    let repo_root = crate::repo_root::detect_from(Path::new(&file_arg));
    let display_path = file_arg.clone();
    let read_path = resolve_repo_relative(&repo_root, Path::new(&file_arg));

    if !read_path.is_file() {
        eprintln!("error: plan file not found: {display_path}");
        return 1;
    }

    let mut plan: Plan;
    let errors: Vec<String>;
    match parse_plan_with_display(&read_path, &display_path) {
        Ok((p, errs)) => {
            plan = p;
            errors = errs;
        }
        Err(err) => {
            eprintln!("error: {display_path}: {err}");
            return 1;
        }
    }

    plan.file = path_to_posix(&maybe_relativize(&read_path, &repo_root));

    if let Some(sprint_raw) = sprint.as_deref() {
        let want = match sprint_raw.parse::<i32>() {
            Ok(v) => v,
            Err(_) => {
                eprintln!(
                    "error: invalid --sprint (expected int): {}",
                    crate::repr::py_repr(sprint_raw)
                );
                return 2;
            }
        };
        plan.sprints.retain(|s| s.number == want);
    }

    if !errors.is_empty() {
        for err in errors {
            eprintln!("error: {err}");
        }
        return 1;
    }

    let json = if pretty {
        match serde_json::to_string_pretty(&plan) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("error: failed to encode JSON: {err}");
                return 1;
            }
        }
    } else {
        match serde_json::to_string(&plan) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("error: failed to encode JSON: {err}");
                return 1;
            }
        }
    };

    println!("{json}");
    0
}

fn resolve_repo_relative(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    repo_root.join(path)
}

fn maybe_relativize(path: &Path, repo_root: &Path) -> PathBuf {
    let Ok(path_abs) = path.canonicalize() else {
        return path.to_path_buf();
    };
    let Ok(root_abs) = repo_root.canonicalize() else {
        return path_abs;
    };
    match path_abs.strip_prefix(&root_abs) {
        Ok(rel) => rel.to_path_buf(),
        Err(_) => path_abs,
    }
}

fn path_to_posix(path: &Path) -> String {
    path.to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}
