use crate::clipboard;
use nils_common::process;
use nils_common::shell::quote_posix_single;
use std::io::{self, Write};
use std::process::Output;

pub fn dispatch(cmd: &str, args: &[String]) -> Option<i32> {
    match cmd {
        "zip" => Some(zip(args)),
        "copy-staged" | "copy" => Some(copy_staged(args)),
        "root" => Some(root(args)),
        "commit-hash" | "hash" => Some(commit_hash(args)),
        _ => None,
    }
}

fn zip(_args: &[String]) -> i32 {
    let short = match git_stdout_trimmed(&["rev-parse", "--short", "HEAD"]) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let filename = format!("backup-{short}.zip");
    let output = match run_git_output(&["archive", "--format", "zip", "HEAD", "-o", &filename]) {
        Some(output) => output,
        None => return 1,
    };
    if output.status.success() {
        0
    } else {
        emit_output(&output);
        exit_code(&output)
    }
}

fn copy_staged(args: &[String]) -> i32 {
    let mut mode = CopyMode::Clipboard;
    let mut mode_flags = 0usize;
    let mut unknown_arg: Option<String> = None;

    for arg in args {
        match arg.as_str() {
            "--stdout" | "-p" | "--print" => {
                mode = CopyMode::Stdout;
                mode_flags += 1;
            }
            "--both" => {
                mode = CopyMode::Both;
                mode_flags += 1;
            }
            "--help" | "-h" => {
                print_copy_staged_help();
                return 0;
            }
            _ => {
                if unknown_arg.is_none() {
                    unknown_arg = Some(arg.to_string());
                }
            }
        }
    }

    if mode_flags > 1 {
        eprintln!("❗ Only one output mode is allowed: --stdout or --both");
        return 1;
    }

    if let Some(arg) = unknown_arg {
        eprintln!("❗ Unknown argument: {arg}");
        eprintln!("Usage: git-cli utils copy-staged [-p|--stdout|--both]");
        return 1;
    }

    let output = match run_git_output(&["diff", "--cached", "--no-color"]) {
        Some(output) => output,
        None => return 1,
    };
    if !output.status.success() {
        emit_output(&output);
        return exit_code(&output);
    }

    let diff = trim_trailing_newlines(&String::from_utf8_lossy(&output.stdout)).to_string();
    if diff.is_empty() {
        println!("⚠️  No staged changes to copy");
        return 1;
    }

    match mode {
        CopyMode::Stdout => {
            println!("{diff}");
            0
        }
        CopyMode::Clipboard => {
            let _ = clipboard::set_clipboard_best_effort(&diff);
            println!("✅ Staged diff copied to clipboard");
            0
        }
        CopyMode::Both => {
            let _ = clipboard::set_clipboard_best_effort(&diff);
            println!("{diff}");
            println!("✅ Staged diff copied to clipboard");
            0
        }
    }
}

fn root(args: &[String]) -> i32 {
    let shell_mode = args.iter().any(|arg| arg == "--shell");
    let output = match run_git_output(&["rev-parse", "--show-toplevel"]) {
        Some(output) => output,
        None => return 1,
    };

    if !output.status.success() {
        eprintln!("❌ Not in a git repository");
        return 1;
    }

    let root = trim_trailing_newlines(&String::from_utf8_lossy(&output.stdout)).to_string();
    if shell_mode {
        println!("cd -- {}", shell_escape(&root));
        eprintln!("📁 Jumped to Git root: {root}");
    } else {
        println!();
        println!("📁 Jumped to Git root: {root}");
    }
    0
}

fn commit_hash(args: &[String]) -> i32 {
    let Some(ref_arg) = args.first() else {
        eprintln!("❌ Missing git ref");
        return 1;
    };

    let ref_commit = format!("{ref_arg}^{{commit}}");
    let output = match run_git_output(&["rev-parse", "--verify", "--quiet", &ref_commit]) {
        Some(output) => output,
        None => return 1,
    };
    if !output.status.success() {
        emit_output(&output);
        return exit_code(&output);
    }

    let _ = io::stdout().write_all(&output.stdout);
    0
}

fn run_git_output(args: &[&str]) -> Option<Output> {
    match run_output("git", args) {
        Ok(output) => Some(output),
        Err(err) => {
            eprintln!("{err}");
            None
        }
    }
}

fn run_output(cmd: &str, args: &[&str]) -> Result<Output, String> {
    process::run_output(cmd, args)
        .map(|output| output.into_std_output())
        .map_err(|err| format!("spawn {cmd}: {err}"))
}

fn git_stdout_trimmed(args: &[&str]) -> Result<String, i32> {
    let output = run_git_output(args).ok_or(1)?;
    if !output.status.success() {
        emit_output(&output);
        return Err(exit_code(&output));
    }
    Ok(trim_trailing_newlines(&String::from_utf8_lossy(&output.stdout)).to_string())
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().unwrap_or(1)
}

fn emit_output(output: &Output) {
    let _ = io::stdout().write_all(&output.stdout);
    let _ = io::stderr().write_all(&output.stderr);
}

fn trim_trailing_newlines(input: &str) -> &str {
    input.trim_end_matches(['\n', '\r'])
}

fn shell_escape(value: &str) -> String {
    quote_posix_single(value)
}

fn print_copy_staged_help() {
    print!(
        "Usage: git-cli utils copy-staged [-p|--stdout|--both]\n  -p, --stdout, --print   Print staged diff to stdout (no status message)\n  --both                  Print to stdout and copy to clipboard\n"
    );
}

enum CopyMode {
    Clipboard,
    Stdout,
    Both,
}
